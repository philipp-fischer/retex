//! Service composition for shared internal quantity, extended and expression IR.
use super::*;
use crate::core::enums::{CharacterCategory as Cat, Command as Cmd};
use crate::language::expressions::ExpressionTokens;

pub(super) fn read(
    execution: &mut InternalExecution,
    view: &mut EvalView<'_>,
    returned: Option<Result<Value>>,
) -> Result<Step> {
    let returned = returned.map(|value| {
        value.and_then(|value| match value {
            Value::Scan(value) => Ok(IV::Scan(value)),
            Value::Assignment(AV::Quantity(value)) => Ok(IV::Assignment(value)),
            Value::InternalValue(value) => Ok(IV::Internal(value)),
            Value::Expansion(EV::Tokens(tokens)) => Ok(IV::Tokens(tokens)),
            Value::Internal(InternalResult::Quantity(value)) => Ok(IV::Read(value)),
            _ => Err(Error::runtime("Invalid internal service result")),
        })
    });
    execution.resume(view, returned).map(|step| match step {
        InternalStep::Complete(value) => Step::Complete(Value::Internal(value)),
        InternalStep::Yield(action) => Step::Yield(Action::Evaluation(match action {
            IA::Scan(request) => Request::Scan(request),
            IA::AssignmentRead => Request::Assignment(AR::Read),
            IA::Extended(code) => Request::Extended(code),
            IA::Expression(code) => Request::Expression(code),
            IA::GeneralText => Request::Expand(ER::GeneralText { expanded: false }),
            IA::Read => Request::Internal(IR::Read),
        })),
    })
}
pub(super) fn extended(
    execution: &mut ExtendedExecution,
    view: &mut EvalView<'_>,
    returned: Option<Result<Value>>,
) -> Result<Step> {
    let returned = returned.map(|value| {
        value.and_then(|value| match value {
            Value::Scan(value) => Ok(value),
            _ => Err(Error::runtime("Invalid extended service result")),
        })
    });
    execution.resume(view, returned).map(|step| match step {
        ExtendedStep::Complete(value) => Step::Complete(Value::InternalValue(value)),
        ExtendedStep::Scan(request) => Step::Yield(Action::Evaluation(Request::Scan(request))),
    })
}
// Only inspect current tokens here. Scanning actions go through owned children;
// the synchronous reader's error retention policy remains in its public driver.
struct TokenView<'a>(&'a crate::language::input::InputProcessor);
impl ExpressionTokens for TokenView<'_> {
    fn literal_category(&self, category: Cat) -> bool {
        self.0.current.literal_category(category)
    }
    fn cur_chr(&self) -> i64 {
        self.0.current.character().unwrap_or(-1)
    }
    fn cur_command(&self) -> Option<Cmd> {
        self.0.current.operation()
    }
}
pub(super) fn expression(
    execution: &mut ExpressionExecution,
    kind: QuantityKind,
    view: &mut EvalView<'_>,
    returned: Option<Result<Value>>,
) -> Result<Step> {
    let returned = returned.map(|value| {
        value.and_then(|value| match value {
            Value::Assignment(AV::Quantity(Some((_, value)))) => Ok(XV::Quantity(value)),
            Value::Scan(SV::Unit) => Ok(XV::Unit),
            _ => Err(Error::runtime("Invalid expression service result")),
        })
    });
    match execution.resume(&TokenView(&view.core.input), returned)? {
        ExpressionStep::Complete(value) => Ok(Step::Complete(Value::InternalValue(
            Assignments::internal(kind, value)?,
        ))),
        ExpressionStep::Yield(action) => Ok(Step::Yield(Action::Evaluation(match action {
            XA::Nonblank => Request::Scan(SR::Nonblank),
            XA::BackInput => {
                Request::Ready(view.core.input.back_input().map(|_| Value::Scan(SV::Unit)))
            }
            XA::Value(kind) => {
                // Expression operands disallow token lists before scanning.
                if kind == QuantityKind::Toks {
                    Request::Ready(Err(Error::value(
                        "Token lists cannot be expression operands",
                    )))
                } else {
                    Request::Assignment(AR::ScanValue(kind))
                }
            }
        }))),
    }
}
