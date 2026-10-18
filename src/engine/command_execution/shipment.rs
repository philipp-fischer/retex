//! Ordered cooperative shipment using the shared typesetting traversal.
use super::*;
use crate::{
    language::{specials::Specials, stream_execution::StreamRequest, streams::display_tokens},
    typesetting::{
        placement::PlaceAction,
        shipment_execution::{ShipmentExecution, ShipmentKind, ShipmentStep},
    },
};
use std::{cell::RefCell, rc::Rc};

pub(in crate::engine) struct ShipTask {
    execution: ShipmentExecution,
    pointer: i64,
    built: bool,
    hold: Option<RootHold>,
    warnings: Rc<RefCell<Vec<String>>>,
    flush_warnings: bool,
    await_finish: bool,
    awaiting: bool,
}
impl ShipTask {
    pub(super) fn checkpoint_ready(&self) -> bool {
        self.execution.checkpoint_ready()
    }
    pub(super) fn checkpoint_holds(&self) -> usize {
        usize::from(self.hold.is_some())
    }
    pub(super) fn checkpoint_clone(
        &self,
        roots: &crate::core::runtime::OwnershipRoots,
        warnings: Rc<RefCell<Vec<String>>>,
    ) -> Option<Self> {
        Some(Self {
            execution: self.execution.checkpoint_clone()?,
            pointer: self.pointer,
            built: self.built,
            hold: self
                .hold
                .as_ref()
                .map(|hold| roots.hold(hold.roots.borrow().iter().copied())),
            warnings,
            flush_warnings: self.flush_warnings,
            await_finish: self.await_finish,
            awaiting: self.awaiting,
        })
    }
    pub(super) fn checkpoint_warnings(&self) -> &Rc<RefCell<Vec<String>>> {
        &self.warnings
    }
    pub(super) fn explicit(view: &mut EngineView<'_>, pointer: i64) -> Self {
        view.observe();
        Self::new(view, pointer, false, Default::default(), true)
    }
    pub(super) fn page(
        view: &mut EngineView<'_>,
        pointer: i64,
        built: bool,
        warnings: Rc<RefCell<Vec<String>>>,
    ) -> Self {
        Self::new(view, pointer, built, warnings, false)
    }
    fn new(
        view: &mut EngineView<'_>,
        pointer: i64,
        built: bool,
        warnings: Rc<RefCell<Vec<String>>>,
        flush_warnings: bool,
    ) -> Self {
        #[cfg(feature = "latency-diagnostics")]
        crate::local_update_audit::consumer("shipout");
        Self {
            execution: ShipmentExecution::new(
                pointer,
                if built {
                    ShipmentKind::Built
                } else {
                    ShipmentKind::Explicit
                },
            ),
            pointer,
            built,
            hold: Some(view.builder.ownership.hold([pointer])),
            warnings,
            flush_warnings,
            await_finish: false,
            awaiting: false,
        }
    }
    pub(super) fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<Option<FrameStep>> {
        let returned = if self.awaiting {
            self.awaiting = false;
            Some(
                returned
                    .ok_or_else(|| Error::runtime("Missing shipment child result"))?
                    .and_then(|value| match value {
                        CommandValue::ShipmentResult(value) if !self.await_finish => Ok(value),
                        CommandValue::Executed(true) if self.await_finish => Ok(None),
                        _ => Err(Error::runtime("Invalid shipment child result")),
                    }),
            )
        } else {
            if returned.is_some() {
                return Err(Error::runtime("Unexpected shipment child result"));
            }
            None
        };
        let result = view.with_pages_buffered(
            &mut self.warnings.borrow_mut(),
            |_, _, layout, context, services| {
                self.execution.resume(layout, context, services, returned)
            },
        );
        if result.is_err() || matches!(result, Ok(ShipmentStep::Complete)) {
            // Preserve the two original APIs' cleanup precedence.
            let cleanup = view.builder.release(self.pointer, false);
            self.hold.take();
            if self.flush_warnings {
                for warning in std::mem::take(&mut *self.warnings.borrow_mut()) {
                    view.warning(&warning);
                }
            }
            let completed = result.map(|_| ());
            if self.built {
                cleanup?;
                completed?;
            } else {
                completed?;
                cleanup?;
            }
            if self.built {
                view.pages
                    .events
                    .push(crate::typesetting::pages::PageEvent::Shipout {
                        page: view.layout.pages.len(),
                        width: view.layout.pages.last().unwrap().width_sp,
                        height: view.layout.pages.last().unwrap().height_sp,
                    });
            }
            if self.built || self.flush_warnings {
                view.pages.dead_cycles = 0;
            }
            return Ok(Some(FrameStep::Complete(CommandValue::Executed(true))));
        }
        match result? {
            ShipmentStep::Continue => Ok(None),
            ShipmentStep::Effect(action) => {
                self.awaiting = true;
                self.await_finish = false;
                Ok(Some(FrameStep::Yield(CommandRequest::Effect(
                    EffectTask::new(action),
                ))))
            }
            ShipmentStep::FinishPage(_) => {
                self.awaiting = true;
                self.await_finish = true;
                Ok(Some(FrameStep::Yield(CommandRequest::FinishPage(
                    super::forms::FinishPageTask::new(),
                ))))
            }

            ShipmentStep::Complete => unreachable!(),
        }
    }
}

pub(in crate::engine) struct EffectTask {
    action: PlaceAction,
    started: bool,
}
impl EffectTask {
    pub(super) fn new(action: PlaceAction) -> Self {
        Self {
            action,
            started: false,
        }
    }
    pub(super) fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        if !self.started {
            if returned.is_some() {
                return Err(Error::runtime("Unexpected shipment effect result"));
            }
            self.started = true;
            let request = match &self.action {
                PlaceAction::Io(effect) => Request::Stream(StreamRequest::Perform(effect.clone())),
                PlaceAction::Special(effect) => {
                    if effect.late {
                        Request::RetainedText(effect.tokens.clone())
                    } else {
                        Request::Ready(Ok(Value::Expansion(ExpansionValue::Tokens(
                            effect.tokens.clone(),
                        ))))
                    }
                }
            };
            return Ok(FrameStep::Yield(CommandRequest::Evaluation(request)));
        }
        let value = returned.ok_or_else(|| Error::runtime("Missing shipment effect result"))??;
        let result = match &self.action {
            PlaceAction::Io(_) => {
                if !matches!(
                    value,
                    CommandValue::Evaluation(Value::Expansion(ExpansionValue::Unit))
                ) {
                    return Err(Error::runtime("Invalid delayed IO result"));
                }
                None
            }
            PlaceAction::Special(effect) => {
                let CommandValue::Evaluation(Value::Expansion(ExpansionValue::Tokens(tokens))) =
                    value
                else {
                    return Err(Error::runtime("Invalid late special result"));
                };
                let text = display_tokens(view, &tokens)?;
                view.event(
                    "special",
                    json!({"late":effect.late,"mode":effect.mode.as_str(),"text":text}),
                );
                Specials::ship_text(view, &text, effect.mode)
                    .map(|(mode, text)| (mode.as_str().into(), text))
            }
        };
        Ok(FrameStep::Complete(CommandValue::ShipmentResult(result)))
    }
}
