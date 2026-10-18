//! Job lifecycle and execution over independently owned typed components.
mod adapters;
pub(crate) mod checkpoints;
pub use checkpoints::PageBoundaryObservation;
mod command_execution;
mod commands;
mod evaluation;
mod execution;
mod recovery;
mod reuse;
pub use execution::RunProgress;
pub use reuse::ReuseStats;
mod formats;
mod services;
mod shipment;
use crate::{
    core::{
        characters::LanguageProfile,
        config::EngineConfig,
        constants::Constants as C,
        errors::{Error, Result},
        persistence::PersistentComponent,
        runtime::RootHold,
    },
    diagnostics::{Diagnostics, TraceRecorder},
    fonts::catalog::FontCatalog,
    language::{
        assignments::Assignments,
        character_tokens::Token,
        conditionals::Conditionals,
        expansion::Expander,
        extended::ExtendedState,
        input::InputProcessor,
        input_frames::SourceStream,
        pdf_commands::PDFUtilities,
        primitives::{self, StartupDate},
        random_numbers::RandomNumbers,
        records::{Quantity, ValueKey},
        state::Environment,
        streams::Streams,
    },
    pdf::backend::PDFBackend,
    resources::search::InputResources,
    typesetting::{
        alignments::Alignments,
        display::PageDisplayList,
        hyphenation::Hyphenation,
        layout::{Layout, TypesettingContext},
        math_atoms::MathState,
        nodes::NodeBuilder,
        pages::{OutputHandler, PageBuilder},
        vertical::Vertical,
    },
};
use chrono::{Datelike, Timelike};
use serde_json::json;
use std::{collections::BTreeMap, path::Path, sync::Arc};
pub type PDFTeXEngine = Engine;
pub struct Engine {
    host: crate::resources::host::HostHandle,
    execution: Option<execution::Execution>,
    replay: Option<recovery::Replay>,
    pub core: EngineCore,
    pub env: Environment,
    pub fonts: FontCatalog,
    pub builder: NodeBuilder,
    pub layout: Layout,
    pub page_builder: PageBuilder,
    pub vertical: Vertical,
    pub hyphenation: Hyphenation,
    pub math: MathState,
    pub alignments: Alignments,
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use crate::typesetting::box_commands::BoxServices;

    // Original OwnershipPerformanceTests.test_global_replacement_releases_a_hidden_saved_box.
    #[test]
    fn global_replacement_reclaims_hidden_saved_boxes_on_restoration() {
        let mut engine = Engine::new(EngineConfig::default()).unwrap();
        let old = engine.builder.new_null_box().unwrap();
        engine.view().assign_box(0, old, false).unwrap();
        engine.view().begin_group_owned("explicit", None).unwrap();
        let local = engine.builder.new_null_box().unwrap();
        engine.view().assign_box(0, local, false).unwrap();
        let final_box = engine.builder.new_null_box().unwrap();
        engine.view().assign_box(0, final_box, true).unwrap();
        assert!(engine.builder.nodes.contains_key(&old));
        engine.view().end_group_owned("explicit", false).unwrap();
        assert!(!engine.builder.nodes.contains_key(&old));
        assert!(!engine.builder.nodes.contains_key(&local));
        assert!(engine.builder.nodes.contains_key(&final_box));
    }
}
pub(crate) mod local;
pub struct EngineCore {
    pub(crate) local: local::Local,
    pub(crate) paragraph_preview: crate::paragraph_preview::State,
    pub(crate) reuse: reuse::ExecutionReuse,
    pub config: EngineConfig,
    pub input: InputProcessor,
    pub expansion: Expander,
    pub conditionals: Conditionals,
    pub assignments: Assignments,
    pub extended: ExtendedState,
    pub random_numbers: RandomNumbers,
    pub streams: Streams,
    pub pdf_utilities: PDFUtilities,
    pub backend: PDFBackend,
    pub trace: TraceRecorder,
    pub diagnostics: Diagnostics,
    pub ready_already: i64,
    pub failure: Option<Error>,
    pub processing_phase: String,
    pub failure_help: Arc<[Token]>,
    pub output_pages: Vec<PageDisplayList>,
    pub interaction: u8,
    pub format_snapshot: Option<crate::serialization::formats::FormatSnapshot>,
    pub setbox_allowed: bool,
    pub box_after_assignment: Option<Token>,
    pub roots: RootHold,
    pub page_observation: PageBuilder,
    pub math_observation: MathState,
    pub marks: BTreeMap<(i64, u8), Arc<[Token]>>,
    pub stream_eof: BTreeMap<i64, bool>,
    pub alignment_depth: usize,
}
pub(crate) struct EngineView<'a> {
    pub core: &'a mut EngineCore,
    pub env: &'a mut Environment,
    pub fonts: &'a mut FontCatalog,
    pub builder: &'a mut NodeBuilder,
    pub layout: &'a mut Layout,
    pub pages: &'a mut PageBuilder,
    pub vertical: &'a mut Vertical,
    pub hyphenation: &'a mut Hyphenation,
    pub math: &'a mut MathState,
    pub alignments: &'a mut Alignments,
}
pub(crate) struct EvalView<'a> {
    pub core: &'a mut EngineCore,
    pub env: &'a mut Environment,
    pub fonts: &'a mut FontCatalog,
    pub builder: &'a mut NodeBuilder,
    pub layout: &'a mut Layout,
}
pub(crate) fn startup_date() -> StartupDate {
    let now = crate::resources::host::local_date(crate::resources::host::now());
    StartupDate {
        hour: now.hour() as i64,
        minute: now.minute() as i64,
        day: now.day() as i64,
        month: now.month() as i64,
        year: now.year() as i64,
    }
}
impl Engine {
    pub(crate) fn local_origin(&mut self, id: u64, offset: usize, source: Arc<[u8]>, path: String) {
        self.core.local.trace_enabled = self.core.config.exact_local_trace;
        let everypar_empty = self
            .env
            .values
            .raw()
            .get(&ValueKey::parameter("everypar"))
            .is_none_or(|v| matches!(v,Quantity::Tokens(t) if t.is_empty()));
        let quiescent = self.execution.as_ref().is_some_and(|e| {
            e.local_literal_prefix() && (e.checkpoint_clone().is_some() || everypar_empty)
        });
        self.core.local.checkpoint(
            id,
            offset,
            source,
            path,
            &self.env,
            &self.core.input,
            quiescent,
        );
    }
    pub fn completed_page_count(&self) -> usize {
        let pages = self.layout.pages.len();
        pages.min(
            self.core
                .backend
                .resources
                .pages
                .len()
                .saturating_sub(usize::from(
                    self.core.backend.document.current_page.is_some(),
                )),
        )
    }
    pub(crate) fn prepare_preview_page(
        &self,
        page: usize,
    ) -> Result<(PDFBackend, FontCatalog, PageDisplayList)> {
        let _host = crate::resources::host::enter(self.host.clone());
        if page == 0 || page > self.completed_page_count() {
            return Err(Error::value("Completed preview page unavailable"));
        }
        let (mut backend, mut fonts, mut env) = {
            #[cfg(feature = "latency-diagnostics")]
            let _clone = crate::latency_diagnostics::phase(
                "preview_observer_clone",
                "PDF/font/environment observer",
                None,
            );
            (
                self.core.backend.clone(),
                self.fonts.detached_clone()?,
                self.env.clone(),
            )
        };
        // Preview is an isolated observer, even if a later construction segment
        // is currently recording. Its font/PDF preparation cannot join that
        // segment's shared ordered-effect journal.
        env.reuse_stateful.set(false);
        env.reuse_paragraph_level.set(None);
        env.reuse_font_construction_allowed.set(false);
        env.reuse_physical_input.set(None);
        env.reuse_input_lookups.borrow_mut().clear();
        env.reuse_effects.borrow_mut().take();
        env.reuse_sequence.borrow_mut().take();
        env.reuse_expansions.set(None);
        #[cfg(feature = "latency-diagnostics")]
        let _prepare = crate::latency_diagnostics::phase(
            "preview_paint_prepare",
            "isolated page operators",
            None,
        );
        let prepared = if self.execution.is_none() {
            self.core
                .output_pages
                .get(page - 1)
                .cloned()
                .ok_or_else(|| Error::value("Prepared preview page unavailable"))?
        } else {
            if self.core.config.exact_local_updates || self.core.config.paragraph_preview {
                backend
                    .prepare_page_range(
                        &self.layout.pages[page - 1..page],
                        page - 1,
                        &mut fonts,
                        &mut env,
                        &mut |_| {},
                    )?
                    .remove(0)
            } else {
                backend
                    .prepare_preview_page_range(
                        &self.layout.pages[page - 1..page],
                        page - 1,
                        &mut fonts,
                        &mut env,
                        &mut |_| {},
                    )?
                    .remove(0)
            }
        };
        Ok((backend, fonts, prepared))
    }
    pub fn new(config: EngineConfig) -> Result<Self> {
        Self::new_with_host(config, crate::resources::host::current())
    }
    pub fn new_with_host(
        config: EngineConfig,
        host: crate::resources::host::HostHandle,
    ) -> Result<Self> {
        let _host = crate::resources::host::enter(host.clone());
        config.validate()?;
        let fonts = FontCatalog::new(&config, None)?;
        let capacities = fonts.capacities.clone();
        let env = Environment::new(capacities.clone());
        let resources = InputResources::new(
            config.ini,
            &config.program_name,
            config.output_directory.as_ref().map(Into::into),
            fonts.locations.clone(),
        )?;
        let mut input = InputProcessor::new(
            config.language_profile,
            config.ini,
            capacities.clone(),
            resources,
        );
        input.recovery = crate::core::recovery::RecoveryState::new(
            config.recovery_policy,
            config.recovery_limit,
        );
        input.initialize_cat_codes();
        input.tracing = true;
        let mut expansion = Expander::new(&input);
        expansion.expansion_limit = config.expansion_limit;
        if config.recovery_policy == crate::core::recovery::RecoveryPolicy::Interactive {
            expansion.uninterrupted_expansion_limit = Some(1_000_000);
            expansion.recovery_depth_limit = Some(1000);
        }
        let mut builder = NodeBuilder::new(capacities);
        builder.tracing = true;
        builder.recovery_glyphs =
            config.recovery_policy == crate::core::recovery::RecoveryPolicy::Interactive;
        let roots = builder.ownership.hold(std::iter::empty());
        let mut layout = Layout::default();
        layout.exact_repro = config.exact_repro;
        layout.greedy_paragraphs = config.greedy_paragraphs;
        layout.tracing = true;
        let record_limit =
            if config.recovery_policy != crate::core::recovery::RecoveryPolicy::Strict {
                Some(1024)
            } else {
                None
            };
        let core = EngineCore {
            local: Default::default(),
            paragraph_preview: Default::default(),
            reuse: Default::default(),
            trace: TraceRecorder::new(config.trace_enabled, config.event_limit),
            interaction: config.interaction_mode.unwrap_or(1),
            backend: PDFBackend {
                exact_repro: config.exact_repro,
                ..PDFBackend::default()
            },
            random_numbers: RandomNumbers::new(config.exact_repro),
            config,
            input,
            expansion,
            conditionals: Conditionals::default(),
            assignments: Assignments::default(),
            extended: ExtendedState::default(),
            streams: Streams::default(),
            pdf_utilities: PDFUtilities::default(),
            diagnostics: Diagnostics {
                record_limit,
                ..Diagnostics::default()
            },
            ready_already: 0,
            failure: None,
            processing_phase: "initialization".into(),
            failure_help: Arc::from([]),
            output_pages: Vec::new(),
            format_snapshot: None,
            setbox_allowed: true,
            box_after_assignment: None,
            roots,
            page_observation: PageBuilder::default(),
            math_observation: MathState::default(),
            marks: BTreeMap::new(),
            stream_eof: BTreeMap::new(),
            alignment_depth: 0,
        };
        Ok(Self {
            host,
            execution: None,
            replay: None,
            core,
            env,
            fonts,
            builder,
            layout,
            page_builder: PageBuilder {
                output_handler: OutputHandler::Routine,
                ..PageBuilder::default()
            },
            vertical: Vertical::default(),
            hyphenation: Hyphenation::default(),
            math: MathState::default(),
            alignments: Alignments::default(),
        })
    }
    pub(crate) fn view(&mut self) -> EngineView<'_> {
        EngineView {
            core: &mut self.core,
            env: &mut self.env,
            fonts: &mut self.fonts,
            builder: &mut self.builder,
            layout: &mut self.layout,
            pages: &mut self.page_builder,
            vertical: &mut self.vertical,
            hyphenation: &mut self.hyphenation,
            math: &mut self.math,
            alignments: &mut self.alignments,
        }
    }
    pub fn init_prim(&mut self) -> Result<()> {
        let _host = crate::resources::host::enter(self.host.clone());
        primitives::initialize(
            &mut self.env,
            &mut self.core.expansion,
            &mut self.core.input,
            &self.layout,
            startup_date(),
            self.core.config.ini,
        )
    }
    pub fn start_input(&mut self, filename: &Path) -> bool {
        let _host = crate::resources::host::enter(self.host.clone());
        if self.execution.is_some() {
            self.core.input.last_input_error =
                Some("Cannot replace input during an active run".into());
            return false;
        }
        self.prepare_replay(filename);
        self.core.input.start_input(&self.env, filename, None, None)
    }
    pub fn start_terminal(&mut self) -> bool {
        let _host = crate::resources::host::enter(self.host.clone());
        if !self.host.native_capabilities() {
            self.core.input.last_input_error = Some(
                "Terminal input is unavailable; supply document bytes through the host".into(),
            );
            return false;
        }
        self.core.input.job_name = Some("texput".into());
        self.core.input.start_input(
            &self.env,
            Path::new("<terminal>"),
            None,
            Some(Box::new(SourceStream::new(std::io::BufReader::new(
                std::io::stdin(),
            )))),
        )
    }
    pub fn recovered(&self) -> bool {
        self.core.input.recovery.count > 0
    }
    pub fn status(&self) -> &'static str {
        if self.core.failure.is_some() || !self.core.diagnostics.errors.is_empty() {
            "failed"
        } else if self.execution.is_some() {
            "running"
        } else {
            "complete"
        }
    }
    pub fn record_failure(&mut self, error: Error) {
        let _host = crate::resources::host::enter(self.host.clone());
        self.view().record_failure(error)
    }
    pub fn main_body(&mut self) -> bool {
        if self.execution.is_none() {
            if let Err(error) = self.begin_run() {
                return self.finish_run(Err(error));
            }
        }
        loop {
            match self.advance_run(usize::MAX) {
                Ok(RunProgress::Yielded) => {}
                Ok(RunProgress::Complete) => return true,
                Ok(RunProgress::Failed) => {
                    if self.retry_recovery(true).unwrap_or(false) {
                        continue;
                    }
                    return false;
                }
                Err(error) => return self.finish_run(Err(error)),
            }
        }
    }
    fn finish_run(&mut self, result: Result<()>) -> bool {
        let _host = crate::resources::host::enter(self.host.clone());
        self.execution = None;
        if let Err(error) = result {
            self.record_failure(error);
            if let Err(error) = self.view().ship_out_page() {
                self.view()
                    .event("partial_page_discard", json!({"message":error.message}));
                self.layout.page_contents.clear();
            }
        }
        self.cleanup_run()
    }
    fn cleanup_run(&mut self) -> bool {
        self.view().eval().drain_recovery();
        self.execution = None;
        while let Some(group) = self.env.groups.last() {
            let kind = group.kind.clone();
            if let Err(error) = self.view().end_group_owned(&kind, false) {
                self.record_failure(error);
                break;
            }
        }
        while !self.builder.context_stack.is_empty() {
            let head = self.builder.context.cur_list_head;
            self.builder.context.cur_list_head = C::NULL;
            self.builder.context.cur_list_tail = C::NULL;
            self.builder.context.cur_l = None;
            self.builder.context.cur_item = None;
            self.builder.context.lig_stack.clear();
            self.builder.context.unicode_run.clear();
            if head != C::NULL {
                if let Err(error) = self.builder.release(head, true) {
                    self.record_failure(error)
                }
            }
            if let Err(error) = self.builder.pop_context() {
                self.record_failure(error);
                break;
            }
        }
        self.core.input.close_files_and_terminate();
        if let Err(error) = self.core.streams.close() {
            self.record_failure(error)
        }
        if let Err(error) = self.view().with_pages(|p, v, _, c, _| p.discard(v, c)) {
            self.record_failure(error)
        }
        self.view().refresh_roots();
        if let Err(error) = self.builder.reclaim_unreachable(&self.env.box_roots()) {
            self.record_failure(error)
        }
        self.core.processing_phase = "emission".into();
        let pages = self.layout.pages.clone();
        let interaction = self.core.interaction;
        let mut warning = |text: &str| self.core.diagnostics.warning(text, interaction);
        match self
            .core
            .backend
            .prepare_pages(&pages, &mut self.fonts, &mut self.env, &mut warning)
        {
            Ok(pages) => {
                self.layout.pages = pages.clone().into();
                self.core.output_pages = pages
            }
            Err(error) => self.record_failure(error),
        }
        let status = self.status();
        self.view().event("run_end", json!({"status":status}));
        self.status() == "complete"
    }
    pub fn load_format(&mut self, path: &Path) -> Result<()> {
        let _host = crate::resources::host::enter(self.host.clone());
        crate::serialization::formats::read_into(self, path)
    }
    /// Restore a decoded initialized image without reading/decompressing a file.
    /// All original compatibility/resource checks still execute. This does not
    /// restore a live execution checkpoint or unfinished typesetting state.
    pub fn restore_initialized_state(
        &mut self,
        state: crate::serialization::formats::FormatSnapshot,
        directory: &Path,
    ) -> Result<()> {
        let _host = crate::resources::host::enter(self.host.clone());
        crate::serialization::formats::restore_into(self, state, directory)
    }
    pub fn write_format(&mut self, path: &Path) -> Result<()> {
        let _host = crate::resources::host::enter(self.host.clone());
        if self.status() != "complete" || self.recovered() {
            return Err(Error::value(
                "No successful initialization dump is available",
            ));
        }
        let state = self
            .core
            .format_snapshot
            .as_ref()
            .ok_or_else(|| Error::value("No successful initialization dump is available"))?;
        crate::serialization::formats::write_snapshot(state, path)?;
        crate::serialization::provenance::write_manifest(
            state,
            &self.fonts.search.policy,
            &self.core.input.source_resources,
            self.core.config.exact_repro,
            path,
        )?;
        Ok(())
    }
    pub fn write_pdf(&mut self, path: &Path) -> Result<bool> {
        let _host = crate::resources::host::enter(self.host.clone());
        if self.recovered() {
            return Err(Error::value(
                "Recovered previews cannot be exported as an ordinary successful PDF",
            ));
        }
        if self.execution.is_some() {
            return Err(Error::value("Cannot export PDF from an unfinished run"));
        }
        if InputProcessor::parameter(&self.env, "pdfoutput", 1) <= 0 {
            return Err(Error::value(
                "DVI output is not implemented; set pdfoutput=1 for PDF writing",
            ));
        }
        let options = self.view().pdf_write_options()?;
        let interaction = self.core.interaction;
        let mut warning = |text: &str| self.core.diagnostics.warning(text, interaction);
        self.core.backend.write_pdf(
            &self.core.output_pages,
            path,
            &self.fonts,
            &options,
            &mut warning,
        )
    }
    /// Report the existing export policy without serializing a PDF on edits.
    pub fn pdf_output_available(&self) -> bool {
        !self.recovered()
            && self.execution.is_none()
            && self.status() == "complete"
            && InputProcessor::parameter(&self.env, "pdfoutput", 1) > 0
            && !self.core.output_pages.is_empty()
            && self.core.backend.document.draft != Some(true)
    }
    pub fn export_pdf_bytes(&mut self) -> Result<Option<Vec<u8>>> {
        let _host = crate::resources::host::enter(self.host.clone());
        if self.recovered() {
            return Err(Error::value(
                "Recovered previews cannot be exported as an ordinary successful PDF",
            ));
        }
        if self.execution.is_some() {
            return Err(Error::value("Cannot export PDF from an unfinished run"));
        }
        if InputProcessor::parameter(&self.env, "pdfoutput", 1) <= 0 {
            return Err(Error::value(
                "DVI output is not implemented; set pdfoutput=1 for PDF writing",
            ));
        }
        let options = self.view().pdf_write_options()?;
        let interaction = self.core.interaction;
        let mut warning = |text: &str| self.core.diagnostics.warning(text, interaction);
        self.core
            .backend
            .pdf_bytes(&self.core.output_pages, &self.fonts, &options, &mut warning)
    }
    pub fn dump_layout(&self, path: &Path) -> Result<()> {
        let _host = crate::resources::host::enter(self.host.clone());
        let data = crate::diagnostics::snapshot::layout_snapshot(
            &self.layout,
            &self.builder,
            &self.fonts,
            &self.core.diagnostics,
            self.status(),
            Some(&self.core.output_pages),
            self.core.trace.events.len(),
            self.core.trace.dropped,
        )?;
        crate::diagnostics::snapshot::write_layout(path, &data)
    }
}
impl EngineView<'_> {
    pub(crate) fn eval(&mut self) -> EvalView<'_> {
        self.observe();
        EvalView {
            core: self.core,
            env: self.env,
            fonts: self.fonts,
            builder: self.builder,
            layout: self.layout,
        }
    }
    pub(crate) fn observe(&mut self) {
        use crate::typesetting::{hyphenation::HyphenationEvent as H, pages::PageEvent as P};
        {
            let mut evaluation = EvalView {
                core: self.core,
                env: self.env,
                fonts: self.fonts,
                builder: self.builder,
                layout: self.layout,
            };
            for event in std::mem::take(&mut self.pages.events) {
                match event {
                    P::Shipout {page,width,height} => evaluation.event_if_requested("shipout",||json!({"page":page,"explicit":false,"width_sp":width,"height_sp":height})),
                    P::BreakCandidate {node,total,goal,depth,penalty,badness,cost} => evaluation.event_if_requested("page_break_candidate",||json!({"node":node,"total_sp":total,"goal_sp":goal,"depth_sp":depth,"penalty":penalty,"badness":badness,"cost":cost})),
                    P::InsertionSplit {register,node,goal,kept,size} => evaluation.event_if_requested("insertion_split",||json!({"register":register,"node":node,"goal_sp":goal,"kept":kept,"size_sp":size})),
                    P::OutputBegin {dead_cycles,box_pointer} => evaluation.event_if_requested("output_begin",||json!({"dead_cycles":dead_cycles,"box":box_pointer})),
                    P::OutputEnd {dead_cycles} => evaluation.event_if_requested("output_end",||json!({"dead_cycles":dead_cycles})),
                }
            }
            for event in std::mem::take(&mut self.hyphenation.events) {
                match event {
                    H::Data {language,patterns} => evaluation.event_if_requested("hyphenation_data",||json!({"language":language,"data_kind":if patterns {"patterns"}else{"exceptions"}})),
                    H::Language {language,left,right} => evaluation.event_if_requested("language",||json!({"language":language,"left_min":left,"right_min":right})),
                    H::Word {language,font,letters,levels,left,right,positions} => evaluation.event_if_requested("hyphenation_word",||json!({"language":language,"font":font,"characters":letters,"levels":levels,"left_min":left,"right_min":right,"inserted_positions":positions})),
                }
            }
        }

        let p = &*self.pages;
        let o = &mut self.core.page_observation;
        o.active = p.active;
        o.output_dimensions = p.output_dimensions;
        o.dead_cycles = p.dead_cycles;
        o.insert_penalties = p.insert_penalties;
        o.last_glue = p.last_glue;
        o.last_penalty = p.last_penalty;
        o.last_kern = p.last_kern;
        o.last_node_kind = p.last_node_kind;
        o.goal = p.goal;
        o.height = p.height;
        o.depth = p.depth;
        o.stretch = p.stretch;
        o.shrink = p.shrink;
        o.insertions = p.insertions.clone();
        o.page.clone_from(&p.page);
        self.core.marks = self.vertical.marks.clone();
        self.core.math_observation.depth = self.math.depth;
        self.core.math_observation.copy_last_line_from(&self.math);
        self.core
            .math_observation
            .list_frames
            .clone_from(&self.math.list_frames);
        self.core.math_observation.equation_number = self.math.equation_number;
        self.refresh_roots();
    }
    pub(crate) fn refresh_roots(&mut self) {
        // Environment reclamation supplies its current/saved roots after the
        // assignment/restoration. Caching the old register roots retains boxes
        // that Python immediately reclaims.
        let mut roots = self.core.roots.roots.borrow_mut();
        roots.clear();
        self.pages.append_roots(&mut roots);
        self.vertical.append_roots(&mut roots);
        // Match the reference's live math/alignment root providers.
        roots.extend(
            self.math
                .held
                .iter()
                .chain(&self.alignments.held)
                .copied()
                .filter(|root| self.builder.nodes.contains_key(root)),
        );
        self.core.backend.document.append_form_roots(&mut roots);
    }
    pub(crate) fn event_if_requested(
        &mut self,
        kind: &str,
        fields: impl FnOnce() -> serde_json::Value,
    ) {
        self.eval().event_if_requested(kind, fields);
    }
    pub(crate) fn event(&mut self, kind: &str, fields: serde_json::Value) {
        self.eval().event(kind, fields)
    }
    pub(crate) fn record_failure(&mut self, error: Error) {
        self.eval().drain_recovery();
        let mut error = error.capture(self.core.processing_phase.clone(), self.core.input.source());
        if self.core.config.recovery_policy == crate::core::recovery::RecoveryPolicy::Strict {
            error.details = None;
        }
        let primary = self.core.failure.is_none();
        if primary {
            self.core.failure = Some(error.clone())
        }
        let mut fields = serde_json::to_value(error.diagnostic()).unwrap_or(json!({}));
        fields["primary"] = json!(primary);
        self.event("failure", fields);
        let help = if self.core.failure_help.is_empty() {
            None
        } else {
            let tokens = self.core.failure_help.clone();
            crate::language::streams::display_tokens(&mut self.eval(), &tokens).ok()
        };
        self.core.diagnostics.print_err(
            &error,
            self.core.interaction,
            self.core.input.source(),
            help,
            InputProcessor::parameter(self.env, "errorcontextlines", 5),
        );
    }
}
