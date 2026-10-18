//! Cooperative outer execution. Nested command, resource and layout services
//! still run synchronously; max_steps bounds continuation work, not wall time.
use super::command_execution::CommandStep;
use super::{
    Engine, EngineCore,
    command_execution::{
        CommandExecution, CommandProgress, CommandRequest, PageTask, ParagraphTask,
    },
    evaluation::{Evaluation, EvaluationProgress, Request, Value},
};
use crate::{
    core::{
        enums::Command,
        errors::{Error, ErrorKind, Result},
    },
    language::expansion::{ExpansionRequest, ExpansionValue},
    typesetting::box_commands::BoxServices,
};
use serde_json::json;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunProgress {
    Yielded,
    Complete,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Main,
    FinalPages,
    PartialPages,
}

pub(super) struct Execution {
    evaluation: Option<Evaluation>,
    command: Option<CommandExecution>,
    command_ready: bool,
    explicit: bool,
    phase: Phase,
}
impl Execution {
    pub(super) fn checkpoint_blocker(&self) -> &'static str {
        if self.phase == Phase::PartialPages {
            return "partial_pages";
        }
        if self.evaluation.is_some() {
            return "outer_evaluation";
        }
        self.command
            .as_ref()
            .map_or("none", |c| c.checkpoint_blocker())
    }
    pub(super) fn local_literal_prefix(&self) -> bool {
        self.phase == Phase::Main
            && self.evaluation.is_none()
            && self
                .command
                .as_ref()
                .is_none_or(|c| c.local_literal_prefix())
    }
    pub(super) fn checkpoint_holds(&self) -> Option<usize> {
        if self.phase == Phase::PartialPages || self.evaluation.is_some() {
            return None;
        }
        self.command
            .as_ref()
            .map_or(Some(0), |command| command.checkpoint_holds())
    }
    pub(super) fn checkpoint_clone_owned(
        &self,
        roots: &crate::core::runtime::OwnershipRoots,
    ) -> Option<Self> {
        self.checkpoint_holds()?;
        Some(Self {
            evaluation: None,
            command: match &self.command {
                None => None,
                Some(command) => Some(command.checkpoint_clone(roots)?),
            },
            command_ready: self.command_ready,
            explicit: self.explicit,
            phase: self.phase,
        })
    }
    pub(super) fn checkpoint_clone(&self) -> Option<Self> {
        if self.phase != Phase::Main || self.evaluation.is_some() || self.command.is_some() {
            return None;
        }
        Some(Self {
            evaluation: None,
            command: None,
            command_ready: self.command_ready,
            explicit: self.explicit,
            phase: self.phase,
        })
    }

    fn new() -> Self {
        Self {
            evaluation: None,
            command: None,
            command_ready: false,
            explicit: false,
            phase: Phase::Main,
        }
    }
    fn prepare_token(&mut self, core: &EngineCore) -> Result<()> {
        if self.evaluation.is_none() {
            self.evaluation = Some(Evaluation::new(
                Request::Expand(ExpansionRequest::XToken { protected: false }),
                core,
            )?);
        }
        Ok(())
    }
}

impl Engine {
    #[cfg(feature = "latency-diagnostics")]
    pub(crate) fn publication_checkpoint_blocker(&self) -> &'static str {
        let Some(execution) = self.execution.as_ref() else {
            return "complete";
        };
        let Some(holds) = execution.checkpoint_holds() else {
            return execution.checkpoint_blocker();
        };
        if !self.builder.ownership.checkpoint_ready_for(holds + 1) {
            return "ownership";
        }
        if self.core.trace.observer.is_some()
            || self.core.input.alignment_handler.is_some()
            || !self.math.list_frames.is_empty()
        {
            return "component";
        }
        "input_expansion_or_streams"
    }
    /// Start once, then advance while the same engine and its input streams
    /// remain resident. A yielded run cannot accept replacement input or export.
    pub fn begin_run(&mut self) -> Result<()> {
        let _host = crate::resources::host::enter(self.host.clone());
        if self.execution.is_some() {
            return Err(Error::value("Engine run already active"));
        }
        self.view().event("run_start", json!({}));
        if self.core.input.in_open == 0 {
            return Err(Error::value("No input file has been opened"));
        }
        if self.core.ready_already == 0 {
            self.init_prim()?;
            self.core.ready_already = 314159;
        }
        self.core.input.get_first_line();
        self.core.processing_phase = "execution".into();
        self.view().run_hook("everyjob")?;
        self.execution = Some(Execution::new());
        Ok(())
    }

    /// Zero steps yields without executing a command. Completion/error cleanup
    /// is shared with main_body. Host callers must measure long individual
    /// services; this API alone does not promise a 5–10 ms slice or safe edit
    /// cancellation, and does not yet isolate speculative filesystem effects.
    pub fn advance_run(&mut self, max_steps: usize) -> Result<RunProgress> {
        let _host = crate::resources::host::enter(self.host.clone());
        let mut execution = self
            .execution
            .take()
            .ok_or_else(|| Error::value("No active engine run"))?;
        let result = (|| -> Result<bool> {
            for _ in 0..max_steps {
                if execution.phase != Phase::Main {
                    match execution.command.as_mut().unwrap().advance(
                        &mut self.view(),
                        if max_steps == usize::MAX {
                            usize::MAX
                        } else {
                            1
                        },
                    ) {
                        CommandProgress::Yielded => continue,
                        CommandProgress::Complete(result) => {
                            execution.command = None;
                            match (execution.phase, result) {
                                (Phase::FinalPages, Err(error)) => {
                                    self.start_partial_finalization(&mut execution, error)?;
                                    continue;
                                }
                                (Phase::PartialPages, Err(error)) => {
                                    self.view().event(
                                        "partial_page_discard",
                                        json!({"message":error.message}),
                                    );
                                    self.layout.page_contents.clear();
                                }
                                (_, Ok(true)) => {}
                                _ => {
                                    return Err(Error::runtime("Invalid page finalization result"));
                                }
                            }
                            return Ok(true);
                        }
                    }
                }

                if !execution.command_ready
                    && execution.command.is_none()
                    && execution.evaluation.is_none()
                {
                    if let Some(hit) = self.view().try_paragraph_reuse()? {
                        if let Some(frontier) = &mut self.core.local.frontier {
                            frontier.rejected = true;
                        }
                        match hit {
                            super::reuse::ReuseHit::Prepared => {
                                execution.evaluation = Some(Evaluation::new(
                                    Request::Expand(ExpansionRequest::PreparedXToken {
                                        protected: false,
                                    }),
                                    &self.core,
                                )?)
                            }
                            super::reuse::ReuseHit::LaidOut(material) => {
                                execution.command = Some(CommandExecution::from_request(
                                    CommandRequest::Paragraph(ParagraphTask::reused(material)),
                                    &self.core,
                                )?)
                            }
                        }
                        continue;
                    }
                }
                if !execution.command_ready && execution.command.is_none() {
                    let progress = if execution.evaluation.is_none()
                        && self.core.input.next_is_literal_character(&self.env)
                    {
                        // The peek only proves that expansion would return Unit;
                        // scanner/provenance/counting and event draining still use
                        // the ordinary token and evaluation services.
                        self.core.input.tracing = self.core.trace.enabled
                            || self.core.trace.observer.is_some()
                            || self.core.diagnostics.debug_tokens;
                        let result = self
                            .core
                            .input
                            .get_token(&mut self.env)
                            .map(|()| Value::Expansion(ExpansionValue::Unit));
                        EvaluationProgress::Complete(self.view().eval().drain_events().and(result))
                    } else {
                        execution.prepare_token(&self.core)?;
                        execution.evaluation.as_mut().unwrap().advance(
                            &mut self.view().eval(),
                            if max_steps == usize::MAX {
                                usize::MAX
                            } else {
                                1
                            },
                        )
                    };
                    match progress {
                        EvaluationProgress::Yielded => {
                            if self.env.reuse_boundary.replace(false) {
                                self.view().cache_prepared_paragraph();
                            }
                            continue;
                        }
                        EvaluationProgress::Complete(result) => {
                            execution.evaluation = None;
                            match result {
                                Ok(Value::Expansion(ExpansionValue::Unit)) => {
                                    execution.command_ready = true;
                                    execution.explicit =
                                        self.core.input.current.is_command(Command::EndJob);
                                }
                                Ok(_) => {
                                    return Err(Error::runtime(
                                        "Unexpected outer expansion result",
                                    ));
                                }
                                Err(error) if error.kind == ErrorKind::Eof => {
                                    execution.command = Some(CommandExecution::from_request(
                                        CommandRequest::Paragraph(ParagraphTask::end()),
                                        &self.core,
                                    )?);
                                    continue;
                                }
                                Err(error) => return Err(error),
                            }
                        }
                    }
                } else {
                    if execution.command.is_none() {
                        execution.command_ready = false;
                        self.view().finish_stateful_paragraph();
                        // Expansion still runs for every token. Already-expanded
                        // ordinary text can use the same dispatcher directly,
                        // avoiding a large heap-backed command frame per glyph.
                        let simple = self.core.input.current.control_sequence == 0
                            && matches!(
                                self.core.input.current.category(),
                                Some(
                                    crate::core::enums::CharacterCategory::Letter
                                        | crate::core::enums::CharacterCategory::OtherChar
                                        | crate::core::enums::CharacterCategory::Spacer
                                )
                            )
                            && self.builder.context.mode
                                == crate::core::enums::ListMode::Horizontal
                            && !self.builder.context.restricted;
                        if simple {
                            let begin = self.view().start_command(false, false)?;
                            if matches!(begin.step, CommandStep::Complete(true))
                                && begin.pending.is_none()
                            {
                                self.view().eval().drain_typesetting_events()?;
                                continue;
                            }
                            execution.command = Some(CommandExecution::from_started(
                                false, false, begin, &self.core,
                            )?);
                            continue;
                        }
                        execution.command = Some(CommandExecution::new(false, false, &self.core)?);
                    }
                    match execution.command.as_mut().unwrap().advance(
                        &mut self.view(),
                        if max_steps == usize::MAX {
                            usize::MAX
                        } else {
                            1
                        },
                    ) {
                        CommandProgress::Yielded => {}
                        CommandProgress::Complete(result) => {
                            execution.command = None;
                            match result {
                                Ok(false) => {
                                    match self.view().finish_main_control(execution.explicit) {
                                        Ok(()) => {
                                            self.core.processing_phase = "layout".into();
                                            execution.phase = Phase::FinalPages;
                                            execution.command =
                                                Some(CommandExecution::from_request(
                                                    CommandRequest::Pages(PageTask::new(
                                                        false, true,
                                                    )),
                                                    &self.core,
                                                )?);
                                        }
                                        Err(error) => {
                                            self.start_partial_finalization(&mut execution, error)?
                                        }
                                    }
                                }
                                Ok(true) => {}
                                Err(error) if error.kind == ErrorKind::Eof => {
                                    return Err(Error::value(
                                        "Unexpected EOF while executing a command",
                                    )
                                    .with_issue(crate::core::recovery::Issue::RunawayArgument));
                                }
                                Err(error) => return Err(error),
                            }
                        }
                    }
                }
            }
            Ok(false)
        })();
        match result {
            Ok(false) => {
                self.execution = Some(execution);
                Ok(RunProgress::Yielded)
            }
            Ok(true) => {
                if self.cleanup_run() {
                    Ok(RunProgress::Complete)
                } else if self.retry_recovery(false)? {
                    Ok(RunProgress::Yielded)
                } else {
                    Ok(RunProgress::Failed)
                }
            }
            Err(error) => {
                self.start_partial_finalization(&mut execution, error)?;
                self.execution = Some(execution);
                Ok(RunProgress::Yielded)
            }
        }
    }
    fn start_partial_finalization(
        &mut self,
        execution: &mut Execution,
        error: Error,
    ) -> Result<()> {
        execution.evaluation = None;
        execution.command = None;
        execution.command_ready = false;
        self.record_failure(error);
        execution.phase = Phase::PartialPages;
        execution.command = Some(CommandExecution::from_request(
            CommandRequest::Pages(PageTask::new(false, true)),
            &self.core,
        )?);
        Ok(())
    }
}
