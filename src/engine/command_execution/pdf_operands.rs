//! Composition of shared PDF operands with the resident evaluation stack.
use super::*;
use crate::language::{
    pdf_operand_execution::{
        PdfOperandAction, PdfOperandExecution, PdfOperandStep, PdfOperandValue,
    },
    streams::display_tokens,
};

pub(in crate::engine) struct PdfOperandTask {
    execution: PdfOperandExecution,
}
impl PdfOperandTask {
    pub(in crate::engine) fn new(operation: String, immediate: bool) -> Self {
        Self {
            execution: PdfOperandExecution::new(operation, immediate),
        }
    }
    pub(super) fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        let returned = returned.map(|result| {
            result.and_then(|value| match value {
                CommandValue::Evaluation(Value::Scan(value)) => Ok(PdfOperandValue::Scan(value)),
                CommandValue::Evaluation(Value::Expansion(ExpansionValue::Tokens(value))) => {
                    display_tokens(view, &value).map(PdfOperandValue::Text)
                }
                CommandValue::Evaluation(Value::Expansion(ExpansionValue::Unit)) => {
                    Ok(PdfOperandValue::Unit)
                }
                CommandValue::PdfScan(
                    crate::language::pdf_scan_execution::PdfScanValue::Action(value),
                ) => Ok(PdfOperandValue::Action(value)),
                _ => Err(Error::runtime("Invalid PDF operand child result")),
            })
        });
        match self.execution.resume(view, returned)? {
            PdfOperandStep::Complete => Ok(FrameStep::Complete(CommandValue::Executed(true))),
            PdfOperandStep::Yield(action) => Ok(FrameStep::Yield(match action {
                PdfOperandAction::Scan(request) => eval_scan(request),
                PdfOperandAction::Text => {
                    CommandRequest::Evaluation(Request::Expand(ExpansionRequest::GeneralText {
                        expanded: true,
                    }))
                }
                PdfOperandAction::ExpandedToken => expanded(),
                PdfOperandAction::NavigationAction => CommandRequest::PdfScan(PdfScanTask::new(
                    crate::language::pdf_scan_execution::PdfScanRequest::Action,
                )),
            })),
        }
    }
}
