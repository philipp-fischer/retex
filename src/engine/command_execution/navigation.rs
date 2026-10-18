//! Shared navigation/structure IR composed with resident TeX evaluation.
use super::*;
use crate::language::{
    image_execution::{ImageAction, ImageExecution, ImageStep, ImageValue},
    navigation_execution::{
        NavigationAction, NavigationExecution, NavigationStep, NavigationValue,
    },
    pdf_scan_execution::{
        PdfScanAction, PdfScanExecution, PdfScanRequest, PdfScanStep, PdfScanValue,
    },
    streams::display_tokens,
};

pub(in crate::engine) struct ImageTask {
    execution: ImageExecution,
    reuse_sequence: Option<u64>,
    reuse_started: bool,
}
impl ImageTask {
    pub(in crate::engine) fn new(reference: bool, immediate: bool) -> Self {
        Self {
            execution: ImageExecution::new(reference, immediate),
            reuse_sequence: None,
            reuse_started: false,
        }
    }
    pub(super) fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        if !self.reuse_started {
            self.reuse_started = true;
            self.reuse_sequence = view.grouped_image_start();
        }
        let returned = returned.map(|result| {
            result.and_then(|value| match value {
                CommandValue::Evaluation(Value::Scan(value)) => Ok(ImageValue::Scan(value)),
                CommandValue::Evaluation(Value::Expansion(ExpansionValue::Tokens(value))) => {
                    display_tokens(view, &value).map(ImageValue::Text)
                }
                CommandValue::PdfScan(PdfScanValue::Dimensions(value)) => {
                    Ok(ImageValue::Dimensions(value))
                }
                _ => Err(Error::runtime("Invalid image operand child result")),
            })
        });
        match self.execution.resume(view, returned)? {
            ImageStep::Complete => {
                view.grouped_image_end(self.reuse_sequence);
                Ok(FrameStep::Complete(CommandValue::Executed(true)))
            }
            ImageStep::Yield(action) => Ok(FrameStep::Yield(match action {
                ImageAction::Scan(request) => eval_scan(request),
                ImageAction::Text => {
                    CommandRequest::Evaluation(Request::Expand(ExpansionRequest::GeneralText {
                        expanded: true,
                    }))
                }
                ImageAction::Dimensions => {
                    CommandRequest::PdfScan(PdfScanTask::new(PdfScanRequest::Dimensions))
                }
            })),
        }
    }
}

pub(in crate::engine) struct PdfScanTask {
    execution: PdfScanExecution,
}
impl PdfScanTask {
    pub(in crate::engine) fn new(request: PdfScanRequest) -> Self {
        Self {
            execution: PdfScanExecution::new(request),
        }
    }
    pub(super) fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        let returned = returned.map(|result| {
            result.and_then(|value| match value {
                CommandValue::Evaluation(Value::Scan(value)) => Ok(PdfScanValue::Scan(value)),
                CommandValue::Evaluation(Value::Expansion(ExpansionValue::Tokens(value))) => {
                    display_tokens(view, &value).map(PdfScanValue::Text)
                }
                CommandValue::Evaluation(Value::Expansion(ExpansionValue::Unit)) => {
                    Ok(PdfScanValue::Unit)
                }
                CommandValue::PdfScan(value) => Ok(value),
                _ => Err(Error::runtime("Invalid PDF structure child result")),
            })
        });
        match self.execution.resume(view, returned)? {
            PdfScanStep::Complete(value) => Ok(FrameStep::Complete(CommandValue::PdfScan(value))),
            PdfScanStep::Yield(action) => Ok(FrameStep::Yield(match action {
                PdfScanAction::Scan(request) => eval_scan(request),
                PdfScanAction::Text => {
                    CommandRequest::Evaluation(Request::Expand(ExpansionRequest::GeneralText {
                        expanded: true,
                    }))
                }
                PdfScanAction::ExpandedToken => expanded(),
                PdfScanAction::Child(request) => CommandRequest::PdfScan(PdfScanTask::new(request)),
            })),
        }
    }
}
pub(in crate::engine) struct NavigationTask {
    execution: NavigationExecution,
}
impl NavigationTask {
    pub(in crate::engine) fn new(operation: String) -> Self {
        Self {
            execution: NavigationExecution::new(operation),
        }
    }
    pub(super) fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        let returned = returned.map(|result| {
            result.and_then(|value| match value {
                CommandValue::Evaluation(Value::Scan(value)) => Ok(NavigationValue::Scan(value)),
                CommandValue::Evaluation(Value::Expansion(ExpansionValue::Tokens(value))) => {
                    display_tokens(view, &value).map(NavigationValue::Text)
                }
                CommandValue::Evaluation(Value::Expansion(ExpansionValue::Unit)) => {
                    Ok(NavigationValue::Unit)
                }
                CommandValue::PdfScan(value) => Ok(NavigationValue::Child(value)),
                _ => Err(Error::runtime("Invalid navigation child result")),
            })
        });
        match self.execution.resume(view, returned)? {
            NavigationStep::Complete => Ok(FrameStep::Complete(CommandValue::Executed(true))),
            NavigationStep::Yield(action) => Ok(FrameStep::Yield(match action {
                NavigationAction::Scan(request) => eval_scan(request),
                NavigationAction::Text => {
                    CommandRequest::Evaluation(Request::Expand(ExpansionRequest::GeneralText {
                        expanded: true,
                    }))
                }
                NavigationAction::ExpandedToken => expanded(),
                NavigationAction::Child(request) => {
                    CommandRequest::PdfScan(PdfScanTask::new(request))
                }
            })),
        }
    }
}
