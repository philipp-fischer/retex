//! Typed e-TeX expressions, fused scaling and rounded division (`expressions.py`).
use super::records::{Quantity, QuantityKind};
use crate::core::enums::{CharacterCategory, Command};
use crate::core::errors::{Error, Result};
use crate::core::values::{GlueSpec, checked};

pub fn quotient(numerator: i128, denominator: i128) -> Result<i128> {
    if denominator == 0 {
        return Err(Error::value(
            "Arithmetic overflow: division by zero in expression",
        ));
    }
    let result = (2 * numerator.abs() + denominator.abs()) / (2 * denominator.abs());
    Ok(if (numerator < 0) != (denominator < 0) {
        -result
    } else {
        result
    })
}
pub fn normalize(mut glue: GlueSpec) -> GlueSpec {
    if glue.stretch == 0 {
        glue.stretch_order = 0;
    }
    if glue.shrink == 0 {
        glue.shrink_order = 0;
    }
    glue.protrusion_barrier = true;
    glue
}
pub fn map_components(
    value: Quantity,
    mut operation: impl FnMut(i128) -> Result<i128>,
    kind: QuantityKind,
) -> Result<Quantity> {
    match value {
        Quantity::Integer(value) => Ok(Quantity::Integer(checked(
            operation(value as i128)?,
            kind == QuantityKind::Dimen,
        )?)),
        Quantity::Glue(glue) => Ok(Quantity::Glue(GlueSpec {
            width: checked(operation(glue.width as i128)?, true)?,
            stretch: checked(operation(glue.stretch as i128)?, true)?,
            shrink: checked(operation(glue.shrink as i128)?, true)?,
            protrusion_barrier: true,
            ..glue
        })),
        _ => Err(Error::value("Invalid expression quantity")),
    }
}
pub fn add(
    left: Quantity,
    right: Quantity,
    kind: QuantityKind,
    subtract: bool,
) -> Result<Quantity> {
    let sign: i128 = if subtract { -1 } else { 1 };
    match (left, right) {
        (Quantity::Integer(left), Quantity::Integer(right)) => Ok(Quantity::Integer(checked(
            left as i128 + sign * right as i128,
            kind == QuantityKind::Dimen,
        )?)),
        (Quantity::Glue(left), Quantity::Glue(right)) => {
            let component = |a: i64, ao: u8, b: i64, bo: u8| -> Result<(i64, u8)> {
                if ao == bo {
                    Ok((checked(a as i128 + sign * b as i128, true)?, ao))
                } else if ao < bo && b != 0 {
                    Ok((b, bo))
                } else {
                    Ok((a, ao))
                }
            };
            let (stretch, stretch_order) = component(
                left.stretch,
                left.stretch_order,
                right.stretch,
                right.stretch_order,
            )?;
            let (shrink, shrink_order) = component(
                left.shrink,
                left.shrink_order,
                right.shrink,
                right.shrink_order,
            )?;
            Ok(Quantity::Glue(normalize(GlueSpec {
                width: checked(left.width as i128 + sign * right.width as i128, true)?,
                stretch,
                shrink,
                stretch_order,
                shrink_order,
                protrusion_barrier: true,
            })))
        }
        _ => Err(Error::value("Mismatched expression quantities")),
    }
}
pub trait ExpressionTokens {
    fn literal_category(&self, category: CharacterCategory) -> bool;
    fn cur_chr(&self) -> i64;
    fn cur_command(&self) -> Option<Command>;
}
pub trait ExpressionInput {
    fn literal_category(&self, category: CharacterCategory) -> bool;
    fn cur_chr(&self) -> i64;
    fn cur_command(&self) -> Option<Command>;
    fn nonblank(&mut self) -> Result<()>;
    fn back_input(&mut self);
    fn scan_int(&mut self) -> Result<i64>;
    fn scan_dimen(&mut self) -> Result<i64>;
    fn scan_glue(&mut self, mu: bool) -> Result<GlueSpec>;
}
impl<T: ExpressionInput> ExpressionTokens for T {
    fn literal_category(&self, category: CharacterCategory) -> bool {
        ExpressionInput::literal_category(self, category)
    }
    fn cur_chr(&self) -> i64 {
        ExpressionInput::cur_chr(self)
    }
    fn cur_command(&self) -> Option<Command> {
        ExpressionInput::cur_command(self)
    }
}
struct Frame {
    kind: QuantityKind,
    parenthesized: bool,
    expression: Option<Quantity>,
    term: Option<Quantity>,
    expression_op: u8,
    term_op: u8,
    numerator: i64,
}
impl Frame {
    fn new(kind: QuantityKind, parenthesized: bool) -> Self {
        Self {
            kind,
            parenthesized,
            expression: None,
            term: None,
            expression_op: 0,
            term_op: 0,
            numerator: 0,
        }
    }
}
pub struct Expressions;
impl Expressions {
    pub fn factor(input: &mut impl ExpressionInput, kind: QuantityKind) -> Result<Quantity> {
        input.nonblank()?;
        if ExpressionInput::literal_category(input, CharacterCategory::OtherChar)
            && ExpressionInput::cur_chr(input) == b'(' as i64
        {
            return Self::scan(input, kind, true);
        }
        input.back_input();
        scan_value(input, kind)
    }
    /// Parenthesized factors are handled on an explicit frame stack.
    pub fn scan(
        input: &mut impl ExpressionInput,
        kind: QuantityKind,
        parenthesized: bool,
    ) -> Result<Quantity> {
        let mut execution = ExpressionExecution::new(kind, parenthesized);
        let mut returned = None;
        loop {
            match execution.resume(input, returned.take())? {
                ExpressionStep::Complete(value) => return Ok(value),
                ExpressionStep::Yield(action) => {
                    returned = Some(match action {
                        ExpressionAction::Nonblank => {
                            input.nonblank().map(|_| ExpressionValue::Unit)
                        }
                        ExpressionAction::BackInput => {
                            input.back_input();
                            Ok(ExpressionValue::Unit)
                        }
                        ExpressionAction::Value(kind) => {
                            scan_value(input, kind).map(ExpressionValue::Quantity)
                        }
                    });
                }
            }
        }
    }
}
/// The public scanner and engine composition stack drive the same arithmetic
/// and parenthesis frames. No host slice is represented by a TeX token.
pub enum ExpressionAction {
    Nonblank,
    BackInput,
    Value(QuantityKind),
}
pub enum ExpressionValue {
    Unit,
    Quantity(Quantity),
}
pub enum ExpressionStep {
    Yield(ExpressionAction),
    Complete(Quantity),
}
enum Phase {
    Factor,
    FactorToken,
    BackFactor,
    FactorValue,
    Operator,
    BackOperator,
    Apply,
    Done,
}
pub struct ExpressionExecution {
    frames: Vec<Frame>,
    phase: Phase,
    factor: Option<Quantity>,
    operator: u8,
}
impl ExpressionExecution {
    pub fn new(kind: QuantityKind, parenthesized: bool) -> Self {
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::count("expression_invocations", 1);
        Self {
            frames: vec![Frame::new(kind, parenthesized)],
            phase: Phase::Factor,
            factor: None,
            operator: 0,
        }
    }
    fn unit(value: Option<ExpressionValue>) -> Result<()> {
        if matches!(value, Some(ExpressionValue::Unit)) {
            Ok(())
        } else {
            Err(Error::runtime("Invalid expression service result"))
        }
    }
    pub fn resume(
        &mut self,
        input: &impl ExpressionTokens,
        returned: Option<Result<ExpressionValue>>,
    ) -> Result<ExpressionStep> {
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::count("expression_steps", 1);
        #[cfg(feature = "latency-diagnostics")]
        let _timing = crate::output_diagnostics::detailed()
            .then(|| crate::output_diagnostics::operation("expression_resume"));
        let mut value = returned.transpose()?;
        loop {
            match self.phase {
                Phase::Factor => {
                    if value.is_some() {
                        return Err(Error::runtime("Unrequested expression result"));
                    }
                    self.phase = Phase::FactorToken;
                    return Ok(ExpressionStep::Yield(ExpressionAction::Nonblank));
                }
                Phase::FactorToken => {
                    Self::unit(value.take())?;
                    if input.literal_category(CharacterCategory::OtherChar)
                        && input.cur_chr() == b'(' as i64
                    {
                        let frame = self.frames.last().expect("expression frame");
                        let kind = if frame.term_op == 0 {
                            frame.kind
                        } else {
                            QuantityKind::Count
                        };
                        self.frames.push(Frame::new(kind, true));
                        self.phase = Phase::Factor;
                    } else {
                        self.phase = Phase::BackFactor;
                        return Ok(ExpressionStep::Yield(ExpressionAction::BackInput));
                    }
                }
                Phase::BackFactor => {
                    Self::unit(value.take())?;
                    let frame = self.frames.last().expect("expression frame");
                    let kind = if frame.term_op == 0 {
                        frame.kind
                    } else {
                        QuantityKind::Count
                    };
                    self.phase = Phase::FactorValue;
                    return Ok(ExpressionStep::Yield(ExpressionAction::Value(kind)));
                }
                Phase::FactorValue => {
                    let Some(ExpressionValue::Quantity(factor)) = value.take() else {
                        return Err(Error::runtime("Invalid expression factor result"));
                    };
                    self.factor = Some(factor);
                    self.phase = Phase::Operator;
                    return Ok(ExpressionStep::Yield(ExpressionAction::Nonblank));
                }
                Phase::Operator => {
                    Self::unit(value.take())?;
                    self.operator = if input.literal_category(CharacterCategory::OtherChar) {
                        match input.cur_chr() {
                            43 => 1,
                            45 => 2,
                            42 => 3,
                            47 => 4,
                            _ => 0,
                        }
                    } else {
                        0
                    };
                    let frame = self.frames.last().expect("expression frame");
                    if self.operator == 0 {
                        if frame.parenthesized {
                            if !(input.literal_category(CharacterCategory::OtherChar)
                                && input.cur_chr() == b')' as i64)
                            {
                                return Err(Error::value("Missing ) in expression"));
                            }
                        } else if input.cur_command() != Some(Command::Relax) {
                            self.phase = Phase::BackOperator;
                            return Ok(ExpressionStep::Yield(ExpressionAction::BackInput));
                        }
                    }
                    self.phase = Phase::Apply;
                }
                Phase::BackOperator => {
                    Self::unit(value.take())?;
                    self.phase = Phase::Apply;
                }
                Phase::Apply => {
                    let frame = self.frames.last_mut().expect("expression frame");
                    let factor = self.factor.take().expect("expression factor");
                    let mut operator = self.operator;
                    match frame.term_op {
                        0 => {
                            frame.term = Some(match factor {
                                Quantity::Glue(glue) if operator != 0 => {
                                    Quantity::Glue(normalize(glue))
                                }
                                other => other,
                            })
                        }
                        3 if operator == 4 => {
                            frame.numerator = integer(factor)?;
                            operator = 5;
                        }
                        3 => {
                            let factor = integer(factor)? as i128;
                            frame.term = Some(map_components(
                                frame.term.take().expect("expression term"),
                                |v| Ok(v * factor),
                                frame.kind,
                            )?);
                        }
                        4 => {
                            let factor = integer(factor)? as i128;
                            frame.term = Some(map_components(
                                frame.term.take().expect("expression term"),
                                |v| quotient(v, factor),
                                frame.kind,
                            )?);
                        }
                        5 => {
                            let factor = integer(factor)? as i128;
                            let numerator = frame.numerator as i128;
                            frame.term = Some(map_components(
                                frame.term.take().expect("expression term"),
                                |v| quotient(v * numerator, factor),
                                frame.kind,
                            )?);
                        }
                        _ => unreachable!("expression term operator"),
                    }
                    if operator > 2 {
                        frame.term_op = operator;
                    } else {
                        let term = frame.term.take().expect("completed term");
                        frame.expression = Some(if frame.expression_op == 0 {
                            term
                        } else {
                            add(
                                frame.expression.take().expect("expression left operand"),
                                term,
                                frame.kind,
                                frame.expression_op == 2,
                            )?
                        });
                        frame.expression_op = operator;
                        frame.term_op = 0;
                    }
                    if operator == 0 {
                        let result = frame.expression.take().expect("completed expression");
                        self.frames.pop();
                        if self.frames.is_empty() {
                            self.phase = Phase::Done;
                            return Ok(ExpressionStep::Complete(result));
                        }
                        self.factor = Some(result);
                        self.phase = Phase::Operator;
                        return Ok(ExpressionStep::Yield(ExpressionAction::Nonblank));
                    }
                    self.phase = Phase::Factor;
                }
                Phase::Done => return Err(Error::runtime("Expression result already consumed")),
            }
        }
    }
}

fn integer(value: Quantity) -> Result<i64> {
    if let Quantity::Integer(number) = value {
        Ok(number)
    } else {
        Err(Error::value("Integer expression factor required"))
    }
}
fn scan_value(input: &mut impl ExpressionInput, kind: QuantityKind) -> Result<Quantity> {
    match kind {
        QuantityKind::Count => Ok(Quantity::Integer(input.scan_int()?)),
        QuantityKind::Dimen => Ok(Quantity::Integer(input.scan_dimen()?)),
        QuantityKind::Skip => Ok(Quantity::Glue(input.scan_glue(false)?)),
        QuantityKind::Muskip => Ok(Quantity::Glue(input.scan_glue(true)?)),
        QuantityKind::Toks => Err(Error::value("Token lists cannot be expression operands")),
    }
}
