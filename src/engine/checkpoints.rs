//! Live, typed, in-memory state at quiescent outer command boundaries.
//! This is separate from initialized formats: groups, unfinished material,
//! diagnostics, random state, input cursors, PDF state and effects are retained.
use super::*;

/// Inspection of a rollback boundary after shipment. These observations are
/// deliberately not an equivalence key: live material, cursor provenance, PDF
/// identities and transactional streams still require complete comparison.
#[derive(Clone, Debug, serde::Serialize)]
pub struct PageBoundaryObservation {
    pub completed_pages: usize,
    pub input_offset: usize,
    pub input_line: usize,
    pub live_nodes: usize,
    pub next_node: i64,
    pub paragraph_pending: bool,
    pub contribution_nodes: usize,
    pub page_nodes: usize,
    pub group_depth: usize,
    pub read_streams: usize,
    pub write_streams: usize,
    pub reserved_pdf_objects: usize,
}
/// Exact physical-reader witness, including its buffered parent line and occurrence.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct SourceReaderObservation {
    pub path: String,
    pub slot: usize,
    pub occurrence: u64,
    pub offset: usize,
    pub buffered_end: usize,
    #[serde(skip)]
    pub bytes: Arc<[u8]>,
}
impl Engine {
    pub(crate) fn page_boundary_observation(&self, offset: usize) -> PageBoundaryObservation {
        let _read = self
            .builder
            .nodes
            .contract(crate::checked_material::Consumer::Ownership);
        PageBoundaryObservation {
            completed_pages: self.layout.pages.len(),
            input_offset: offset,
            input_line: self.core.input.cursor.line,
            live_nodes: self
                .builder
                .nodes
                .values()
                .filter(|node| node.is_some())
                .count(),
            next_node: self.builder.node_ptr,
            paragraph_pending: self.builder.context.cur_list_head != C::NULL,
            contribution_nodes: self.page_builder.queue.len(),
            page_nodes: self.page_builder.page.len(),
            group_depth: self.env.groups.len(),
            read_streams: self.core.streams.readers.len(),
            write_streams: self.core.streams.writers.len(),
            reserved_pdf_objects: self.core.backend.resources.reserved_objects,
        }
    }
    pub(crate) fn checkpoint_clone(&self) -> Result<Option<Self>> {
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase("checkpoint_clone", "", None);
        if self.recovered()
            || self.core.trace.observer.is_some()
            || !(if self.core.config.progressive_pages {
                self.execution
                    .as_ref()
                    .map_or(Some(0), |execution| execution.checkpoint_holds())
                    .is_some_and(|holds| self.builder.ownership.checkpoint_ready_for(holds + 1))
            } else {
                self.builder.ownership.checkpoint_ready()
            })
            || self.core.input.alignment_handler.is_some()
            || !self.math.list_frames.is_empty()
        {
            return Ok(None);
        }
        let builder = self.builder.checkpoint_clone();
        let execution = match &self.execution {
            Some(execution) => {
                let Some(copy) = (if self.core.config.progressive_pages {
                    execution.checkpoint_clone_owned(&builder.ownership)
                } else {
                    execution.checkpoint_clone()
                }) else {
                    return Ok(None);
                };
                Some(copy)
            }
            None => None,
        };
        let Some(input) = self.core.input.checkpoint_clone() else {
            return Ok(None);
        };
        let Some(expansion) = self.core.expansion.checkpoint_clone() else {
            return Ok(None);
        };
        let Some(conditionals) = self.core.conditionals.checkpoint_clone() else {
            return Ok(None);
        };
        let Some(assignments) = self.core.assignments.checkpoint_clone() else {
            return Ok(None);
        };
        let Some(streams) = self.core.streams.checkpoint_clone()? else {
            return Ok(None);
        };
        let roots = builder
            .ownership
            .hold(self.core.roots.roots.borrow().iter().copied());
        let trace = TraceRecorder {
            enabled: self.core.trace.enabled,
            limit: self.core.trace.limit,
            events: self.core.trace.events.clone(),
            dropped: self.core.trace.dropped,
            observer: None,
        };
        let core = EngineCore {
            local: Default::default(),
            paragraph_preview: Default::default(),
            reuse: self.core.reuse.checkpoint_clone(),
            config: self.core.config.clone(),
            input: input,
            expansion: expansion,
            conditionals: conditionals,
            assignments: assignments,
            extended: self.core.extended.clone(),
            random_numbers: self.core.random_numbers.clone(),
            streams: streams,
            pdf_utilities: self.core.pdf_utilities.clone(),
            backend: self.core.backend.clone(),
            trace: trace,
            diagnostics: self.core.diagnostics.clone(),
            ready_already: self.core.ready_already.clone(),
            failure: self.core.failure.clone(),
            processing_phase: self.core.processing_phase.clone(),
            failure_help: self.core.failure_help.clone(),
            output_pages: self.core.output_pages.clone(),
            interaction: self.core.interaction.clone(),
            format_snapshot: match &self.core.format_snapshot {
                Some(state) => Some(state.detached_clone()?),
                None => None,
            },
            setbox_allowed: self.core.setbox_allowed.clone(),
            box_after_assignment: self.core.box_after_assignment.clone(),
            roots: roots,
            page_observation: self.core.page_observation.clone(),
            math_observation: self.core.math_observation.concrete_snapshot(),
            marks: self.core.marks.clone(),
            stream_eof: self.core.stream_eof.clone(),
            alignment_depth: self.core.alignment_depth.clone(),
        };
        let env = self.env.clone();
        env.reuse_boundary.set(false);
        env.reuse_stateful.set(false);
        env.reuse_paragraph_level.set(None);
        env.reuse_font_construction_allowed.set(false);
        env.reuse_physical_input.set(None);
        env.reuse_input_lookups.borrow_mut().clear();
        env.reuse_rejected_by.set(None);
        env.reuse_expansions.set(None);
        env.reuse_symbols.borrow_mut().clear();
        env.reuse_symbol_keys.borrow_mut().clear();
        env.reuse_effects.borrow_mut().take();
        env.reuse_sequence.borrow_mut().take();
        env.reuse_ignore_meanings.set(false);
        env.reuse_font_names.borrow_mut().clear();
        env.reuse_font_writes.borrow_mut().clear();
        env.reuse_font_symbol_inputs.borrow_mut().clear();
        env.reuse_font_symbol_outputs.borrow_mut().clear();
        env.reuse_outer_prev.borrow_mut().clear();
        Ok(Some(Self {
            host: self.host.clone(),
            execution,
            replay: self.replay.clone(),
            core,
            env,
            fonts: self.fonts.detached_clone()?,
            builder,
            layout: self.layout.concrete_snapshot(),
            page_builder: self.page_builder.clone(),
            vertical: self.vertical.clone(),
            hyphenation: self.hyphenation.clone(),
            math: self.math.concrete_snapshot(),
            alignments: self.alignments.clone(),
        }))
    }
    pub(crate) fn source_checkpoint_blocker(&self) -> &'static str {
        if let Some(execution) = &self.execution {
            let Some(holds) = execution.checkpoint_holds() else {
                return execution.checkpoint_blocker();
            };
            if !self.builder.ownership.checkpoint_ready_for(holds + 1) {
                return "ownership";
            }
        }
        if self.core.input.checkpoint_clone().is_none() {
            return "input";
        }
        if self.core.expansion.checkpoint_clone().is_none() {
            return "expansion";
        }
        "physical_boundary_or_component"
    }
    pub(crate) fn checkpoint_offset(&self, main: &str) -> Option<usize> {
        let execution = self.execution.as_ref()?;
        if self.core.config.progressive_pages {
            execution.checkpoint_holds()?;
        } else {
            execution.checkpoint_clone()?;
        }
        let input = &self.core.input;
        if input.in_open == 0
            || (!self.core.config.progressive_pages && input.in_open != 1)
            || input.end_input_requested
            || input.read_line_only
        {
            return None;
        }
        // The main physical frame may be suspended under immutable macro/token
        // frames. Every byte of its currently buffered line must stay unchanged.
        // Capture only after the outer evaluator returns, preserving its pending
        // command rather than replaying any expansion or scanner side effect.
        if !std::iter::once(&input.cursor)
            .chain(input.input_stack.iter())
            .any(|frame| {
                frame.index == 0
                    && frame.name.as_deref() == Some(main)
                    && frame.token_list.is_none()
            })
        {
            return None;
        }
        input.input_file.first()?.as_ref()?.byte_position()
    }
    pub(crate) fn checkpoint_sources(&self) -> Option<Vec<SourceReaderObservation>> {
        let input = &self.core.input;
        let mut sources = Vec::new();
        for slot in 0..input.in_open {
            let reader = input.input_file.get(slot)?.as_ref()?;
            let path = reader
                .physical_path()
                .map(|p| p.to_string_lossy().into_owned())
                .or_else(|| {
                    std::iter::once(&input.cursor)
                        .chain(input.input_stack.iter())
                        .find(|frame| {
                            frame.index == slot
                                && frame.token_list.is_none()
                                && frame.name.is_some()
                        })
                        .and_then(|frame| frame.name.clone())
                })?;
            sources.push(SourceReaderObservation {
                path,
                slot,
                occurrence: *input.input_occurrences.get(slot)?,
                offset: reader.byte_position()?,
                buffered_end: reader.buffered_end()?,
                bytes: reader.catalog_bytes()?,
            });
        }
        Some(sources)
    }
    pub(crate) fn replace_source_bytes(
        &mut self,
        sources: &[(usize, String, Arc<[u8]>)],
    ) -> Result<()> {
        use sha2::{Digest, Sha256};
        for (slot, path, bytes) in sources {
            self.core
                .input
                .input_file
                .get_mut(*slot)
                .and_then(Option::as_mut)
                .ok_or_else(|| Error::value("Checkpoint source input unavailable"))?
                .replace_bytes(bytes.clone())?;
            self.core
                .input
                .source_resources
                .insert(path.clone(), format!("{:x}", Sha256::digest(bytes)));
        }
        Ok(())
    }
}
