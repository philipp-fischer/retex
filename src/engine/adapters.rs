//! List, box, alignment and math services for the command interpreter.
use super::command_execution::{CommandRequest, ParagraphMaterial, ParagraphTask};
use super::*;
use crate::{
    core::{
        enums::{GroupKind, ListMode, MathStyle},
        values::GlueSpec,
    },
    language::{
        expansion::ExpansionRequest,
        input::ExpandedInput,
        services::{LanguageServices, ListServices},
    },
    typesetting::{
        alignments::AlignmentServices,
        box_commands::{BoxCommands, BoxServices},
        classic_lists::ClassicLists,
        math_atoms::Atom,
        math_construction::MathContext,
        math_layout::{MathLayout, MathLifecycleServices},
        math_parser::{
            MathParseContext, MathParser, MathParserAction as Action, MathParserServices,
            MathParserStep, MathParserValue as Value, MathStop,
        },
        math_renderer::MathRenderer,
        nodes::ArenaStateServices,
        paragraph_operations::ParagraphOperations as Par,
        penalties::Penalties,
        vertical::DiscardKind,
    },
};
impl EngineView<'_> {
    pub(super) fn with_math_parser<T>(
        &mut self,
        f: impl FnOnce(&mut MathParseContext<'_, '_>, &mut dyn MathParserServices) -> Result<T>,
    ) -> Result<T> {
        self.observe();
        let mut warnings = Vec::new();
        let mut warn = |s: &str| warnings.push(s.to_string());
        let mut ctx = TypesettingContext {
            builder: self.builder,
            fonts: self.fonts,
            env: self.env,
            warn: &mut warn,
        };
        let mut parse = MathParseContext {
            typesetting: &mut ctx,
            layout: self.layout,
            state: self.math,
        };
        let mut services = ParserServices { core: self.core };
        let result = f(&mut parse, &mut services);
        for warning in warnings {
            self.warning(&warning);
        }
        result
    }
    pub(super) fn with_pages<T>(
        &mut self,
        f: impl FnOnce(
            &mut PageBuilder,
            &mut Vertical,
            &mut Layout,
            &mut TypesettingContext<'_>,
            &mut super::shipment::PageController<'_>,
        ) -> Result<T>,
    ) -> Result<T> {
        self.observe();
        let mut warnings = Vec::new();
        let result = self.with_pages_buffered(&mut warnings, f);
        for warning in warnings {
            self.warning(&warning)
        }
        result
    }
    pub(super) fn with_pages_buffered<T>(
        &mut self,
        warnings: &mut Vec<String>,
        f: impl FnOnce(
            &mut PageBuilder,
            &mut Vertical,
            &mut Layout,
            &mut TypesettingContext<'_>,
            &mut super::shipment::PageController<'_>,
        ) -> Result<T>,
    ) -> Result<T> {
        let mut warn = |s: &str| warnings.push(s.to_string());
        let mut ctx = TypesettingContext {
            builder: self.builder,
            fonts: self.fonts,
            env: self.env,
            warn: &mut warn,
        };
        let mut controller = super::shipment::PageController {
            core: self.core,
            hyphenation: self.hyphenation,
            math: self.math,
            alignments: self.alignments,
        };
        f(
            self.pages,
            self.vertical,
            self.layout,
            &mut ctx,
            &mut controller,
        )
    }
    pub(super) fn with_lists<T>(
        &mut self,
        f: impl FnOnce(&mut TypesettingContext<'_>, &mut PageBuilder, &MathState) -> Result<T>,
    ) -> Result<T> {
        let mut warnings = Vec::new();
        let mut ctx = TypesettingContext {
            builder: self.builder,
            fonts: self.fonts,
            env: self.env,
            warn: &mut |s| warnings.push(s.to_string()),
        };
        let result = f(&mut ctx, self.pages, self.math);
        for warning in warnings {
            self.warning(&warning)
        }
        result
    }
    pub(super) fn consume_outer(&mut self) -> Result<()> {
        self.with_pages(|p, v, l, c, s| Par::consume_outer(p, v, l, c, s))
    }
    pub(super) fn ship_out_page(&mut self) -> Result<()> {
        // layout.py finishes the page builder, including completed material
        // still queued when a later command fails. Shipment must go through the
        // normal output handler and PDF/file effects, rather than page_contents.
        self.with_pages(|p, v, l, c, s| p.finish(v, l, c, s))
    }
    pub(super) fn start_paragraph_owned(&mut self, indent: bool, resume: bool) -> Result<()> {
        self.drive_request(CommandRequest::Paragraph(ParagraphTask::start(
            indent, resume,
        )))?;
        Ok(())
    }
    pub(super) fn finish_start_paragraph(&mut self, indent: bool, resume: bool) -> Result<()> {
        self.core.reuse.reject(self.env);
        self.core.reuse.attempted = false;
        if !resume {
            self.builder.context.prev_graf = 0
        }
        self.builder
            .push_context(self.fonts, self.env, ListMode::Horizontal, false)?;
        self.builder.context.vertical_paragraph = true;
        let state = Hyphenation::state(self.env, self.core.config.language_profile);
        self.builder.context.initial_hyphenation = state;
        self.builder.context.current_language = state.0;
        if indent {
            let p = self.builder.new_null_box()?;
            self.builder.node_mut(p)?.width = self.parameter("parindent", 0);
            self.builder.tail_append(p)?
        }
        if !resume {
            self.run_hook("everypar")?
        }
        Ok(())
    }
    pub(super) fn finish_paragraph_owned(&mut self, display: bool) -> Result<()> {
        self.drive_request(CommandRequest::Paragraph(ParagraphTask::finish(display)))?;
        Ok(())
    }
    pub(super) fn prepare_finish_paragraph(
        &mut self,
        display: bool,
    ) -> Result<Option<ParagraphMaterial>> {
        // Native line breaking/preparation (including its transparent source-
        // keyed memo bookkeeping) has no TeX callbacks. Its variable glyph
        // output is the parameter; escaping scalar state is checked at Ready.
        let _native = self
            .builder
            .nodes
            .contract(crate::checked_material::Consumer::Preparation);
        if !self.builder.context.vertical_paragraph {
            return Ok(None);
        }
        let preview_started = self
            .core
            .config
            .paragraph_preview_timings
            .then(crate::local_investigation::now);
        self.core.paragraph_preview.line_break_ms = 0.0;
        let head = self.builder.take_list(self.fonts, self.env)?;
        let previous = self.vertical_context_mut()?.prev_graf;
        let profile = self.core.config.language_profile;
        let investigation_started = self
            .core
            .config
            .exact_local_trace
            .then(crate::local_investigation::now);
        let cached_layout = self.begin_layout_reuse(head, previous, display)?;
        let investigation_reused = cached_layout.is_some();
        let (cached_boxes, cached_penalties) = match cached_layout {
            Some((boxes, penalties)) => (Some(boxes), Some(penalties)),
            None => (None, None),
        };
        let boxes = {
            let mut warnings = Vec::new();
            let mut ctx = TypesettingContext {
                builder: self.builder,
                fonts: self.fonts,
                env: self.env,
                warn: &mut |s| warnings.push(s.to_string()),
            };
            let boxes = if let Some(boxes) = cached_boxes {
                boxes
            } else if head == C::NULL {
                Vec::new()
            } else {
                let started = self
                    .core
                    .config
                    .paragraph_preview_timings
                    .then(crate::local_investigation::now);
                let result = self.layout.pack_paragraph(
                    &mut ctx,
                    self.hyphenation,
                    profile,
                    head,
                    previous,
                )?;
                if let Some(started) = started {
                    self.core.paragraph_preview.line_break_ms =
                        crate::local_investigation::now().saturating_sub(started) as f64 / 1e6;
                }
                result
            };
            if let Some(&p) = boxes.last() {
                self.math.set_last_line(Some(Par::line_extent(&ctx, p)?))
            }
            for warning in warnings {
                self.warning(&warning)
            }
            boxes
        };
        let hold = self.builder.ownership.hold(boxes.iter().copied());
        self.builder.pop_context()?;
        self.builder.context.prev_graf += boxes.len() as i64;
        let reused_penalties = cached_penalties.is_some();
        let mut penalties = cached_penalties.unwrap_or_default();
        if !reused_penalties {
            for (index, &p) in boxes.iter().take(boxes.len().saturating_sub(1)).enumerate() {
                penalties.push(Penalties::between(
                    self.env,
                    previous + index as i64 + 1,
                    index as i64 + 1,
                    (boxes.len() - index - 1) as i64,
                    display,
                    self.builder.node(p)?.discretionary_break,
                )?)
            }
        }
        self.finish_layout_reuse(&boxes, &penalties);
        self.cache_paragraph(&boxes, &penalties, display);
        if let Some(started) = investigation_started {
            let elapsed =
                crate::local_investigation::now().saturating_sub(started) as f64 / 1_000_000.0;
            let material = crate::local_investigation::material(self.builder, &boxes);
            self.core.local.trace(serde_json::json!({"event":"paragraph_layout","layout_reused":investigation_reused,"layout_and_cache_ms":elapsed,"material":material}));
        }
        if !display {
            self.with_layout(|_, c| {
                Par::normal_paragraph(c);
                Ok(())
            })?
        }
        if !boxes.is_empty() {
            self.layout.paragraph_count += 1
        }
        self.core.paragraph_preview.layout_ms = preview_started.map_or(0.0, |s| {
            crate::local_investigation::now().saturating_sub(s) as f64 / 1e6
        });
        Ok(Some(ParagraphMaterial {
            boxes,
            penalties,
            hold,
        }))
    }
    pub(super) fn read_alignment(&mut self, vertical: bool) -> Result<i64> {
        stacker::maybe_grow(256 * 1024, 2 * 1024 * 1024, || {
            self.read_alignment_inner(vertical)
        })
    }
    fn read_alignment_inner(&mut self, vertical: bool) -> Result<i64> {
        let mut a = std::mem::take(self.alignments);
        self.core.alignment_depth = a.depth;
        let result = a.read(self, vertical);
        *self.alignments = a;
        self.core.alignment_depth = self.alignments.depth;
        result
    }
}
impl BoxServices for EngineView<'_> {
    fn with_layout<T>(
        &mut self,
        f: impl FnOnce(&mut Layout, &mut TypesettingContext<'_>) -> Result<T>,
    ) -> Result<T> {
        let mut warnings = Vec::new();
        let mut ctx = TypesettingContext {
            builder: self.builder,
            fonts: self.fonts,
            env: self.env,
            warn: &mut |s| warnings.push(s.to_string()),
        };
        let result = f(self.layout, &mut ctx);
        for warning in warnings {
            self.warning(&warning)
        }
        result
    }
    fn begin_group(&mut self, k: &str, g: GroupKind) -> Result<()> {
        self.begin_group_owned(k, Some(g))
    }
    fn end_group(&mut self, k: &str, f: bool) -> Result<()> {
        self.end_group_owned(k, f)
    }
    fn start_paragraph(&mut self, i: bool) -> Result<()> {
        self.start_paragraph_owned(i, false)
    }
    fn finish_paragraph(&mut self) -> Result<()> {
        self.finish_paragraph_owned(false)
    }
    fn handle_par(&mut self) -> Result<()> {
        if self.builder.context.vertical_paragraph {
            self.finish_paragraph()
        } else {
            if self.builder.context.mode == ListMode::Vertical {
                self.with_layout(|_, c| {
                    Par::normal_paragraph(c);
                    Ok(())
                })?
            }
            Ok(())
        }
    }
    fn insert_par_token(&mut self, n: i64) -> Result<bool> {
        if self.builder.context.vertical_paragraph && self.parameter("partokencontext", 0) > n {
            self.back_input()?;
            self.core
                .input
                .back_token(self.core.input.par_token, self.core.input.source())?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    fn execute_token(&mut self, in_box: bool) -> Result<bool> {
        self.command_operation(in_box, false)
    }
    fn append_box(&mut self, p: i64, adjusted: bool) -> Result<()> {
        self.with_pages(|pages, v, l, c, s| {
            if adjusted {
                Par::append_adjusted(pages, v, l, c, s, p)
            } else {
                Par::append_box(pages, v, l, c, s, p)
            }
        })
    }
    fn take_register_box(&mut self, n: i64, copy: bool) -> Result<i64> {
        #[cfg(feature = "latency-diagnostics")]
        crate::output_diagnostics::register(n, copy);
        #[cfg(feature = "latency-diagnostics")]
        let _diag = (copy && crate::latency_diagnostics::enabled()).then(|| {
            crate::latency_diagnostics::phase(
                "box_register_copy",
                &format!(
                    "register={n} {}",
                    crate::latency_diagnostics::source_chain(self.core.input.source().as_ref())
                ),
                None,
            )
        });
        let mut service = ArenaStateServices {
            builder: self.builder,
            error: None,
            restorations: Vec::new(),
        };
        let result = self.env.take_box(n, copy, Some(&mut service));
        if let Some(e) = service.error {
            return Err(e);
        }
        result
    }
    fn assign_box(&mut self, n: i64, p: i64, global: bool) -> Result<()> {
        self.refresh_roots();
        let mut service = ArenaStateServices {
            builder: self.builder,
            error: None,
            restorations: Vec::new(),
        };
        self.env.assign_box(n, p, global, Some(&mut service))?;
        if let Some(e) = service.error {
            return Err(e);
        }
        Ok(())
    }
    fn lastbox(&mut self) -> Result<i64> {
        self.with_lists(|c, p, _| ClassicLists::lastbox(c, p))
    }
    fn split_box(&mut self) -> Result<i64> {
        let n = self.scan_register(None)?;
        if !self.scan_keyword("to")? {
            return Err(Error::value("Missing to in vsplit"));
        }
        let goal = self.scan_dimen()?;
        let mut warnings = Vec::new();
        let mut ctx = TypesettingContext {
            builder: self.builder,
            fonts: self.fonts,
            env: self.env,
            warn: &mut |s| warnings.push(s.to_string()),
        };
        let result = self.vertical.split(self.layout, &mut ctx, n, goal);
        for warning in warnings {
            self.warning(&warning)
        }
        result
    }
    fn take_discards(&mut self, split: bool) -> i64 {
        match self.vertical.take_discards(
            self.builder,
            if split {
                DiscardKind::Split
            } else {
                DiscardKind::Page
            },
        ) {
            Ok(p) => p,
            Err(e) => {
                self.record_failure(e);
                C::NULL
            }
        }
    }
    fn setbox_allowed(&self) -> bool {
        self.core.setbox_allowed
    }
    fn set_setbox_allowed(&mut self, b: bool) {
        self.core.setbox_allowed = b
    }
    fn pending_box_assignment(&mut self) -> Option<Token> {
        self.core.box_after_assignment.take()
    }
    fn missing_character(&mut self, font: usize, code: u32) {
        let name = self.fonts.items[font].name.clone();
        self.warning(&format!(
            "Missing character: There is no {} in font {name}!",
            char::from_u32(code).unwrap_or('?')
        ))
    }
}
impl AlignmentServices for EngineView<'_> {
    fn expanded_protected(&mut self) -> Result<()> {
        self.run_expansion(ExpansionRequest::XToken { protected: true })?;
        Ok(())
    }
    fn expand_current(&mut self) -> Result<bool> {
        match self.run_expansion(ExpansionRequest::Current)? {
            crate::language::expansion::ExpansionValue::Flag(v) => Ok(v),
            _ => Ok(true),
        }
    }
    fn extract_adjustments(&mut self, p: i64) -> Result<(Vec<i64>, Vec<i64>)> {
        self.vertical.extract_adjustments(self.builder, p)
    }
    fn splice(&mut self, p: &[i64]) -> Result<()> {
        self.with_pages(|pages, v, l, c, s| Par::splice(pages, v, l, c, s, p))
    }
    fn consume_outer(&mut self) -> Result<()> {
        EngineView::consume_outer(self)
    }
}
impl MathLifecycleServices for EngineView<'_> {
    fn math_state(&self) -> &MathState {
        self.math
    }
    fn math_state_mut(&mut self) -> &mut MathState {
        self.math
    }
    fn paragraph_before_display(&mut self) -> Result<()> {
        self.finish_paragraph_owned(true)
    }
    fn vertical_prev_graf(&self) -> i64 {
        std::iter::once(&self.builder.context)
            .chain(self.builder.context_stack.iter().rev())
            .find(|c| c.mode == ListMode::Vertical)
            .map_or(0, |c| c.prev_graf)
    }
    fn increment_prev_graf(&mut self, n: i64) {
        if let Ok(c) = self.vertical_context_mut() {
            c.prev_graf += n
        }
    }
    fn parse_math_shift(&mut self) -> Result<Vec<Atom>> {
        let mut parser = MathParser::default();
        let mut returned = None;
        {
            let mut warn = |_: &str| {};
            let mut ctx = TypesettingContext {
                builder: self.builder,
                fonts: self.fonts,
                env: self.env,
                warn: &mut warn,
            };
            parser.start(
                &MathParseContext {
                    typesetting: &mut ctx,
                    layout: self.layout,
                    state: self.math,
                },
                MathStop::Shift,
            )?;
        }
        loop {
            self.observe();
            let step = {
                let mut warnings = Vec::new();
                let mut ctx = TypesettingContext {
                    builder: self.builder,
                    fonts: self.fonts,
                    env: self.env,
                    warn: &mut |s| warnings.push(s.to_string()),
                };
                let mut parse = MathParseContext {
                    typesetting: &mut ctx,
                    layout: self.layout,
                    state: self.math,
                };
                let mut services = ParserServices { core: self.core };
                let result = parser.resume(&mut parse, &mut services, returned.take());
                for s in warnings {
                    self.warning(&s)
                }
                result?
            };
            match step {
                MathParserStep::Complete(Value::Read { atoms, .. }) => return Ok(atoms),
                MathParserStep::Complete(_) => {
                    return Err(Error::runtime("Invalid math parser result"));
                }
                MathParserStep::Yield(action) => {
                    returned = Some(match action {
                        Action::ExpandedToken => {
                            self.expanded_token()?;
                            Value::Unit
                        }
                        Action::Scan(request) => Value::Scan(self.scan(request)?),
                        Action::BoxOperand => {
                            Value::Box(BoxCommands::box_operand(self, None, false, false)?)
                        }
                        Action::ReadBox { operation, group } => Value::Box(BoxCommands::read_box(
                            self,
                            operation,
                            None,
                            None,
                            true,
                            true,
                            Some(group),
                        )?),
                        Action::ExecuteToken { in_box } => {
                            Value::Executed(self.command_operation(in_box, false)?)
                        }
                        Action::ReadAlignment { vertical } => {
                            Value::Alignment(self.read_alignment(vertical)?)
                        }
                        Action::RenderEquation { atoms, style } => {
                            Value::Rendered(self.render_math(atoms, style, false)?)
                        }
                    })
                }
            }
        }
    }
    fn render_math(
        &mut self,
        atoms: Vec<Atom>,
        style: MathStyle,
        penalties: bool,
    ) -> Result<Vec<i64>> {
        let mut renderer = MathRenderer::default();
        self.with_layout(|l, c| {
            renderer
                .render(
                    &mut MathContext {
                        typesetting: c,
                        layout: l,
                    },
                    atoms,
                    style,
                    penalties,
                    false,
                )
                .map(|r| r.nodes)
        })
    }
    fn place_display_alignment(&mut self, head: i64) -> Result<()> {
        let result = self
            .alignments
            .results
            .remove(&head)
            .ok_or_else(|| Error::value("Missing display alignment result"))?;
        self.with_pages(|p, v, l, c, s| {
            MathLayout::display_alignment(p, v, l, c, s, head, &result.rows, result.previous_depth)
        })
    }
    fn place_display(&mut self, pointers: &[i64]) -> Result<()> {
        let mut state = std::mem::take(self.math);
        let result = self
            .with_pages(|p, v, l, c, s| MathLayout::display(p, v, l, c, s, &mut state, pointers));
        *self.math = state;
        result
    }
    fn resume_paragraph(&mut self) -> Result<()> {
        self.start_paragraph_owned(false, true)
    }
}
struct ParserServices<'a> {
    core: &'a mut EngineCore,
}
impl ParserServices<'_> {
    fn eval<'a>(&'a mut self, c: &'a mut MathParseContext<'_, '_>) -> EvalView<'a> {
        EvalView {
            core: self.core,
            env: c.typesetting.env,
            fonts: c.typesetting.fonts,
            builder: c.typesetting.builder,
            layout: c.layout,
        }
    }
}
impl MathParserServices for ParserServices<'_> {
    fn input(&self) -> &InputProcessor {
        &self.core.input
    }
    fn input_mut(&mut self) -> &mut InputProcessor {
        &mut self.core.input
    }
    fn expanded_token(&mut self, c: &mut MathParseContext<'_, '_>) -> Result<()> {
        self.eval(c).expanded_token()
    }
    fn scan_int(&mut self, c: &mut MathParseContext<'_, '_>) -> Result<i64> {
        self.eval(c).scan_int()
    }
    fn scan_char_num(&mut self, c: &mut MathParseContext<'_, '_>) -> Result<i64> {
        self.eval(c).scan_char_num().map(i64::from)
    }
    fn scan_dimen(&mut self, c: &mut MathParseContext<'_, '_>, mu: bool) -> Result<i64> {
        self.eval(c)
            .scan(crate::language::scanning::ScanRequest::Dimen {
                mu,
                infinite: false,
            })?
            .integer()
    }
    fn scan_glue(&mut self, c: &mut MathParseContext<'_, '_>, mu: bool) -> Result<GlueSpec> {
        self.eval(c).scan_glue(mu)
    }
    fn begin_group(
        &mut self,
        c: &mut MathParseContext<'_, '_>,
        name: &str,
        g: Option<GroupKind>,
    ) -> Result<()> {
        c.typesetting.env.begin_group(name, g)?;
        let level = c.typesetting.env.cur_level;
        self.eval(c)
            .event("group_begin", json!({"group":name,"level":level}));
        Ok(())
    }
    fn end_group(&mut self, c: &mut MathParseContext<'_, '_>, name: &str) -> Result<()> {
        c.typesetting
            .builder
            .flush_lig_stack(c.typesetting.fonts, c.typesetting.env, true)?;
        let mut s = ArenaStateServices {
            builder: c.typesetting.builder,
            error: None,
            restorations: Vec::new(),
        };
        let after =
            c.typesetting
                .env
                .end_group(name, &mut c.typesetting.fonts.current, Some(&mut s))?;
        if let Some(error) = s.error {
            return Err(error);
        }
        if !after.is_empty() {
            self.core
                .input
                .begin_tokens(after.into(), self.core.input.source(), None)?
        }
        self.eval(c).event("group_end", json!({"group":name}));
        Ok(())
    }
}
