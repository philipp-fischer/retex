use crate::core::recovery::Issue;
// Expanded numeric scanners on typed continuations.
use super::character_tokens::{Token, character_token};
use super::filename::{FilenameAction, FilenameExecution, FilenameStep};
use super::input::InputProcessor;
use super::state::Environment;
use crate::core::constants::Constants as C;
use crate::core::enums::{CharacterCategory as Cat, Command};
use crate::core::errors::{Error, ErrorKind, Result};
use crate::core::source::SourceSpan;
use crate::core::units::round_decimals;
use crate::core::values::{GlueSpec, checked};
use crate::fonts::catalog::FontCatalog;
use num_bigint::BigInt;
use num_traits::{ToPrimitive, Zero};
use std::ffi::OsString;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum InternalValue {
    Count(i64),
    Dimen(i64),
    Skip(GlueSpec),
    Muskip(GlueSpec),
    Toks(Arc<[Token]>),
    Font(usize),
}
impl InternalValue {
    pub fn magnitude(&self) -> Result<i64> {
        match self {
            Self::Count(value) | Self::Dimen(value) => Ok(*value),
            Self::Skip(spec) | Self::Muskip(spec) => Ok(spec.width),
            Self::Font(value) => Ok(*value as i64),
            Self::Toks(_) => Err(Error::value("Missing number: token register")),
        }
    }
    pub fn mu(&self) -> bool {
        matches!(self, Self::Muskip(_))
    }
}
pub struct ScanContext<'a> {
    pub input: &'a mut InputProcessor,
    pub env: &'a mut Environment,
    pub fonts: &'a mut FontCatalog,
}
#[derive(Clone, Debug)]
pub enum ScanRequest {
    Nonblank,
    OptionalEquals,
    Keyword(String),
    Signs,
    Internal,
    OptionalSpace,
    IntegerCurrent,
    Int,
    Register(Option<i64>),
    CharNum,
    FontIdent,
    Filename,
    Dimension {
        mu: bool,
        infinite: bool,
    },
    Dimen {
        mu: bool,
        infinite: bool,
    },
    DimensionCurrent {
        negative: bool,
        mu: bool,
        infinite: bool,
    },
    Glue {
        mu: bool,
    },
}
#[derive(Clone, Copy, Debug)]
pub enum ScanAction {
    ExpandedToken,
    RawToken,
    BackInput,
    ResolvePrimitive,
    ReadInternal,
    PrepareMagnification,
}
#[derive(Clone, Debug)]
pub enum ScanValue {
    Unit,
    Flag(bool),
    Integer(i64),
    Dimension(i64, u8),
    Glue(GlueSpec),
    Font(usize),
    Filename(OsString),
    Internal(Option<InternalValue>),
}
impl ScanValue {
    pub fn integer(self) -> Result<i64> {
        if let Self::Integer(value) = self {
            Ok(value)
        } else {
            Err(Error::value("Unexpected integer scanner result"))
        }
    }
    pub fn flag(self) -> Result<bool> {
        if let Self::Flag(value) = self {
            Ok(value)
        } else {
            Err(Error::value("Unexpected scanner flag result"))
        }
    }
    pub fn dimension(self) -> Result<(i64, u8)> {
        if let Self::Dimension(value, order) = self {
            Ok((value, order))
        } else {
            Err(Error::value("Unexpected dimension scanner result"))
        }
    }
    pub fn internal(self) -> Result<Option<InternalValue>> {
        if let Self::Internal(value) = self {
            Ok(value)
        } else {
            Err(Error::value("Unexpected internal quantity scanner result"))
        }
    }
}
pub enum ScanStep {
    Yield(ScanAction),
    Complete(ScanValue),
}
enum Request {
    Scan(ScanRequest),
    Action(ScanAction),
}
type Resume = Box<
    dyn for<'a> FnOnce(&mut ValueScanner, &mut ScanContext<'a>, Result<ScanValue>) -> Result<()>,
>;
enum Operation {
    Request(Request),
    Resume(Resume),
}
pub struct ValueScanner {
    operations: Vec<Operation>,
    value: Option<Result<ScanValue>>,
    awaiting: bool,
    pub value_reader_connected: bool,
    pub cur_order: u8,
}
impl Default for ValueScanner {
    fn default() -> Self {
        Self {
            operations: Vec::new(),
            value: None,
            awaiting: false,
            value_reader_connected: false,
            cur_order: 0,
        }
    }
}
fn literal(context: &ScanContext<'_>, category: Cat) -> bool {
    context.input.current.literal_category(category)
}
fn character(context: &ScanContext<'_>) -> i64 {
    context.input.current.character().unwrap_or(-1)
}
fn checked_big(value: BigInt, dimension: bool) -> Result<i64> {
    checked(
        value
            .to_i128()
            .ok_or_else(|| Error::value("Arithmetic overflow"))?,
        dimension,
    )
}
impl ValueScanner {
    pub fn start(&mut self, request: ScanRequest) -> Result<()> {
        if !self.operations.is_empty() || self.awaiting {
            return Err(Error::value("Scanner already active"));
        }
        self.value = None;
        self.operations
            .push(Operation::Request(Request::Scan(request)));
        Ok(())
    }
    pub fn resume(
        &mut self,
        context: &mut ScanContext<'_>,
        returned: Option<Result<ScanValue>>,
    ) -> Result<ScanStep> {
        if self.awaiting {
            self.value =
                Some(returned.ok_or_else(|| Error::value("Missing scanner service result"))?);
            self.awaiting = false;
        } else if returned.is_some() {
            return Err(Error::value("Unrequested scanner service result"));
        }
        while let Some(operation) = self.operations.pop() {
            let outcome = match operation {
                Operation::Request(Request::Action(action)) => {
                    self.awaiting = true;
                    return Ok(ScanStep::Yield(action));
                }
                Operation::Request(Request::Scan(request)) => self.request(context, request),
                Operation::Resume(resume) => {
                    let value = self
                        .value
                        .take()
                        .unwrap_or_else(|| Err(Error::value("Missing scanner child result")));
                    resume(self, context, value)
                }
            };
            if let Err(error) = outcome {
                self.value = Some(Err(error));
            }
        }
        self.value
            .take()
            .ok_or_else(|| Error::value("Missing scanner result"))?
            .map(ScanStep::Complete)
    }
    pub fn abort(&mut self) {
        self.operations.clear();
        self.value = None;
        self.awaiting = false;
    }
    fn defer(
        &mut self,
        request: Request,
        resume: impl for<'a> FnOnce(&mut Self, &mut ScanContext<'a>, Result<ScanValue>) -> Result<()>
        + 'static,
    ) {
        self.operations.push(Operation::Resume(Box::new(resume)));
        self.operations.push(Operation::Request(request));
    }
    fn scan(
        &mut self,
        request: ScanRequest,
        resume: impl for<'a> FnOnce(&mut Self, &mut ScanContext<'a>, Result<ScanValue>) -> Result<()>
        + 'static,
    ) {
        self.defer(Request::Scan(request), resume);
    }
    fn action(
        &mut self,
        action: ScanAction,
        resume: impl for<'a> FnOnce(&mut Self, &mut ScanContext<'a>, Result<ScanValue>) -> Result<()>
        + 'static,
    ) {
        self.defer(Request::Action(action), resume);
    }
    fn complete(&mut self, value: ScanValue) {
        self.value = Some(Ok(value));
    }
    fn request(&mut self, context: &mut ScanContext<'_>, request: ScanRequest) -> Result<()> {
        match request {
            ScanRequest::Filename => {
                self.filename_next(context, FilenameExecution::default(), None)?
            }
            ScanRequest::Nonblank => {
                self.action(ScanAction::ExpandedToken, |scanner, context, value| {
                    value?;
                    if context.input.current.is_category(Cat::Spacer) {
                        scanner
                            .operations
                            .push(Operation::Request(Request::Scan(ScanRequest::Nonblank)));
                    } else {
                        scanner.complete(ScanValue::Unit);
                    }
                    Ok(())
                })
            }
            ScanRequest::OptionalEquals => {
                self.scan(ScanRequest::Nonblank, |scanner, context, value| {
                    value?;
                    if !literal(context, Cat::OtherChar) || character(context) != 61 {
                        context.input.back_input()?;
                    }
                    scanner.complete(ScanValue::Unit);
                    Ok(())
                })
            }
            ScanRequest::Keyword(word) => {
                let source = context.input.source();
                self.scan(
                    ScanRequest::Nonblank,
                    move |scanner, context, value| match value {
                        Ok(_) if word.is_empty() => {
                            scanner.complete(ScanValue::Flag(true));
                            Ok(())
                        }
                        Ok(_) => scanner.keyword_next(
                            context,
                            word.chars().collect(),
                            0,
                            Vec::new(),
                            source,
                        ),
                        Err(error) if error.kind == ErrorKind::Eof => {
                            scanner.complete(ScanValue::Flag(false));
                            Ok(())
                        }
                        Err(error) => Err(error),
                    },
                );
            }
            ScanRequest::Signs => self.signs_next(false),
            ScanRequest::Internal => {
                let connected = self.value_reader_connected;
                self.action(ScanAction::ResolvePrimitive, move |scanner, _, value| {
                    value?;
                    if connected {
                        scanner.action(ScanAction::ReadInternal, |scanner, _, value| {
                            scanner.complete(value?);
                            Ok(())
                        });
                    } else {
                        scanner.complete(ScanValue::Internal(None));
                    }
                    Ok(())
                });
            }
            ScanRequest::OptionalSpace => {
                self.action(ScanAction::ExpandedToken, |scanner, context, value| {
                    match value {
                        Ok(_) => {
                            if !context.input.current.is_category(Cat::Spacer) {
                                context.input.back_input()?;
                            }
                        }
                        Err(error) if error.kind == ErrorKind::Eof => {}
                        Err(error) => return Err(error),
                    }
                    scanner.complete(ScanValue::Unit);
                    Ok(())
                })
            }
            ScanRequest::IntegerCurrent => self.integer_begin(),
            ScanRequest::Int => self.scan(ScanRequest::Signs, |scanner, _, value| {
                let negative = value?.flag()?;
                scanner.scan(ScanRequest::IntegerCurrent, move |scanner, _, value| {
                    let value = value?.integer()?;
                    scanner.complete(ScanValue::Integer(checked(
                        if negative {
                            -(value as i128)
                        } else {
                            value as i128
                        },
                        false,
                    )?));
                    Ok(())
                });
                Ok(())
            }),
            ScanRequest::Register(maximum) => {
                let maximum = maximum.unwrap_or(context.env.register_max);
                self.scan(ScanRequest::Int, move |scanner, context, value| {
                    let mut value = value?.integer()?;
                    if !(0..=maximum).contains(&value) {
                        context.input.recover(
                            Issue::BadNumber,
                            Error::value(format!("Bad register number: {value}")),
                            "Used register zero",
                        )?;
                        value = 0;
                    }
                    scanner.complete(ScanValue::Integer(value));
                    Ok(())
                });
            }
            ScanRequest::CharNum => {
                let profile = context.input.profile;
                self.scan(ScanRequest::Int, move |scanner, context, value| {
                    let mut value = value?.integer()?;
                    if let Err(error) = profile.validate(value) {
                        context.input.recover(
                            Issue::BadNumber,
                            error,
                            "Used character code zero",
                        )?;
                        value = 0;
                    }
                    scanner.complete(ScanValue::Integer(value));
                    Ok(())
                });
            }
            ScanRequest::FontIdent => {
                self.scan(ScanRequest::Nonblank, |scanner, context, value| {
                    value?;
                    match context.input.current.operation() {
                        Some(Command::DefFont) => {
                            scanner.complete(ScanValue::Font(context.fonts.current))
                        }
                        Some(Command::SetFont) => {
                            scanner.complete(ScanValue::Font(character(context) as usize))
                        }
                        Some(Command::MathFamily) => {
                            scanner.scan(ScanRequest::Internal, |scanner, _, value| {
                                let Some(InternalValue::Font(font)) = value?.internal()? else {
                                    return Err(Error::value("Missing math font identifier"));
                                };
                                scanner.complete(ScanValue::Font(font));
                                Ok(())
                            })
                        }
                        _ => {
                            return Err(Error::value(
                                "Missing font identifier: expected a font selection or font",
                            ));
                        }
                    }
                    Ok(())
                })
            }
            ScanRequest::Dimension { mu, infinite } => {
                self.scan(ScanRequest::Signs, move |scanner, _, value| {
                    let negative = value?.flag()?;
                    scanner.operations.push(Operation::Request(Request::Scan(
                        ScanRequest::DimensionCurrent {
                            negative,
                            mu,
                            infinite,
                        },
                    )));
                    Ok(())
                })
            }
            ScanRequest::Dimen { mu, infinite } => self.scan(
                ScanRequest::Dimension { mu, infinite },
                |scanner, _, value| {
                    let (value, order) = value?.dimension()?;
                    scanner.cur_order = order;
                    scanner.complete(ScanValue::Integer(value));
                    Ok(())
                },
            ),
            ScanRequest::DimensionCurrent {
                negative,
                mu,
                infinite,
            } => self.dimension_begin(negative, mu, infinite),
            ScanRequest::Glue { mu } => self.glue_begin(mu),
        }
        Ok(())
    }

    fn filename_next(
        &mut self,
        context: &mut ScanContext<'_>,
        mut execution: FilenameExecution,
        returned: Option<Result<()>>,
    ) -> Result<()> {
        match execution.resume(context.input, returned)? {
            FilenameStep::Complete(name) => self.complete(ScanValue::Filename(name)),
            FilenameStep::Yield(action) => {
                let action = match action {
                    FilenameAction::ExpandedToken => ScanAction::ExpandedToken,
                    FilenameAction::BackInput => ScanAction::BackInput,
                };
                self.action(action, move |scanner, context, value| {
                    scanner.filename_next(context, execution, Some(value.map(|_| ())))
                });
            }
        }
        Ok(())
    }
    fn keyword_next(
        &mut self,
        context: &mut ScanContext<'_>,
        word: Vec<char>,
        index: usize,
        mut tokens: Vec<Token>,
        source: Option<Arc<SourceSpan>>,
    ) -> Result<()> {
        tokens.push(context.input.current.token);
        let matches = context.input.current.control_sequence == 0
            && char::from_u32(character(context) as u32).is_some_and(|character| {
                character.to_lowercase().collect::<String>() == word[index].to_string()
            });
        if !matches {
            for token in tokens.into_iter().rev() {
                context.input.back_token(token, source.clone())?;
            }
            self.complete(ScanValue::Flag(false));
        } else if index + 1 == word.len() {
            self.complete(ScanValue::Flag(true));
        } else {
            self.action(
                ScanAction::ExpandedToken,
                move |scanner, context, value| match value {
                    Ok(_) => scanner.keyword_next(context, word, index + 1, tokens, source),
                    Err(error) if error.kind == ErrorKind::Eof => {
                        for token in tokens.into_iter().rev() {
                            context.input.back_token(token, source.clone())?;
                        }
                        scanner.complete(ScanValue::Flag(false));
                        Ok(())
                    }
                    Err(error) => Err(error),
                },
            );
        }
        Ok(())
    }
    fn signs_next(&mut self, mut negative: bool) {
        self.scan(ScanRequest::Nonblank, move |scanner, context, value| {
            value?;
            if literal(context, Cat::OtherChar) && matches!(character(context), 43 | 45) {
                if character(context) == 45 {
                    negative = !negative;
                }
                scanner.signs_next(negative);
            } else {
                scanner.complete(ScanValue::Flag(negative));
            }
            Ok(())
        });
    }
    fn integer_begin(&mut self) {
        self.scan(ScanRequest::Internal, |scanner, context, value| {
            if let Some(internal) = value?.internal()? {
                if matches!(internal, InternalValue::Toks(_)) {
                    return Err(Error::value("Missing number: token register"));
                }
                if matches!(internal, InternalValue::Font(_)) {
                    return Err(Error::value("Missing number: font identifier"));
                }
                if internal.mu() {
                    context.input.recover(
                        Issue::IncompatibleUnits,
                        Error::value("Incompatible glue units"),
                        "Interpreted one mu as one pt",
                    )?;
                }
                scanner.complete(ScanValue::Integer(checked(
                    internal.magnitude()? as i128,
                    false,
                )?));
                return Ok(());
            }
            if literal(context, Cat::OtherChar) && character(context) == 96 {
                scanner.action(ScanAction::RawToken, |scanner, context, value| {
                    value?;
                    let current = &context.input.current;
                    let value = if current.control_sequence != 0 {
                        let name = context.env.cs_name(current.control_sequence);
                        let mut letters = name.chars();
                        let value = letters
                            .next()
                            .ok_or_else(|| Error::value("Improper alphabetic constant"))?;
                        if letters.next().is_some() {
                            return Err(Error::value("Improper alphabetic constant"));
                        }
                        value as i64
                    } else {
                        let value = current
                            .character()
                            .ok_or_else(|| Error::value("Improper alphabetic constant"))?;
                        if current.is_category(Cat::LeftBrace) {
                            context.input.align_state -= 1;
                        } else if current.is_category(Cat::RightBrace) {
                            context.input.align_state += 1;
                        }
                        value
                    };
                    scanner.scan(ScanRequest::OptionalSpace, move |scanner, _, result| {
                        result?;
                        scanner.complete(ScanValue::Integer(value));
                        Ok(())
                    });
                    Ok(())
                });
                return Ok(());
            }
            let radix = if literal(context, Cat::OtherChar) && matches!(character(context), 39 | 34)
            {
                if character(context) == 39 { 8 } else { 16 }
            } else {
                10
            };
            if radix != 10 {
                scanner.action(ScanAction::ExpandedToken, move |scanner, context, value| {
                    value?;
                    scanner.integer_digits(context, radix, 0, false)
                });
            } else {
                scanner.integer_digits(context, radix, 0, false)?;
            }
            Ok(())
        });
    }
    fn integer_digits(
        &mut self,
        context: &mut ScanContext<'_>,
        radix: i64,
        value: i64,
        found: bool,
    ) -> Result<()> {
        let character = character(context);
        let digit = if literal(context, Cat::OtherChar) && (48..=57).contains(&character) {
            character - 48
        } else if radix == 16
            && (literal(context, Cat::Letter) || literal(context, Cat::OtherChar))
            && (65..=70).contains(&character)
        {
            character - 65 + 10
        } else {
            -1
        };
        if !(0..radix).contains(&digit) {
            if !found {
                context.input.recover(
                    Issue::MissingNumber,
                    Error::value("Missing number"),
                    "Used zero; retained the unexpected token",
                )?;
                context.input.back_input()?;
                self.complete(ScanValue::Integer(0));
                return Ok(());
            }
            if !context.input.current.is_category(Cat::Spacer) {
                context.input.back_input()?;
            }
            self.complete(ScanValue::Integer(value));
        } else {
            let value = match checked(value as i128 * radix as i128 + digit as i128, false) {
                Ok(v) => v,
                Err(e) => {
                    context
                        .input
                        .recover(Issue::BadNumber, e, "Used the maximum integer")?;
                    crate::core::values::MAX_INT
                }
            };
            self.action(
                ScanAction::ExpandedToken,
                move |scanner, context, result| match result {
                    Ok(_) => scanner.integer_digits(context, radix, value, true),
                    Err(error) if error.kind == ErrorKind::Eof => {
                        scanner.complete(ScanValue::Integer(value));
                        Ok(())
                    }
                    Err(error) => Err(error),
                },
            );
        }
        Ok(())
    }
    fn dimension_begin(&mut self, negative: bool, mu: bool, infinite: bool) {
        self.scan(ScanRequest::Internal, move |scanner, context, value| {
            if let Some(internal) = value?.internal()? {
                match internal {
                    InternalValue::Dimen(_) | InternalValue::Skip(_) | InternalValue::Muskip(_) => {
                        if mu != internal.mu() {
                            context.input.recover(
                                Issue::IncompatibleUnits,
                                Error::value("Incompatible glue units"),
                                "Interpreted one mu as one pt",
                            )?;
                        }
                        let value = internal.magnitude()?;
                        scanner.complete(ScanValue::Dimension(
                            checked(
                                if negative {
                                    -(value as i128)
                                } else {
                                    value as i128
                                },
                                true,
                            )?,
                            0,
                        ));
                    }
                    InternalValue::Count(value) => scanner.dimension_units(
                        BigInt::from(value) * C::UNITY,
                        negative,
                        mu,
                        infinite,
                    ),
                    _ => return Err(Error::value("Missing dimension")),
                }
            } else if literal(context, Cat::OtherChar) && matches!(character(context), 39 | 34 | 96)
            {
                scanner.scan(ScanRequest::IntegerCurrent, move |scanner, _, value| {
                    let number = BigInt::from(value?.integer()?) * C::UNITY;
                    scanner.dimension_units(number, negative, mu, infinite);
                    Ok(())
                });
            } else {
                scanner.dimension_digits(
                    context,
                    BigInt::zero(),
                    Vec::new(),
                    false,
                    false,
                    negative,
                    mu,
                    infinite,
                )?;
            }
            Ok(())
        });
    }
    fn dimension_digits(
        &mut self,
        context: &mut ScanContext<'_>,
        mut integer: BigInt,
        mut digits: Vec<u8>,
        mut found: bool,
        mut fraction: bool,
        negative: bool,
        mu: bool,
        infinite: bool,
    ) -> Result<()> {
        if literal(context, Cat::OtherChar) && (48..=57).contains(&character(context)) {
            found = true;
            let digit = character(context) - 48;
            if fraction {
                if digits.len() < 17 {
                    digits.push(digit as u8);
                }
            } else {
                integer = integer * 10 + digit;
            }
            self.action(ScanAction::ExpandedToken, move |scanner, context, value| {
                value?;
                scanner.dimension_digits(
                    context, integer, digits, found, fraction, negative, mu, infinite,
                )
            });
        } else if !fraction
            && literal(context, Cat::OtherChar)
            && matches!(character(context), 46 | 44)
        {
            found = true;
            fraction = true;
            self.action(ScanAction::ExpandedToken, move |scanner, context, value| {
                value?;
                scanner.dimension_digits(
                    context, integer, digits, found, fraction, negative, mu, infinite,
                )
            });
        } else {
            if !found {
                context.input.recover(
                    Issue::MissingNumber,
                    Error::value("Missing number in dimension"),
                    "Used zero for the length",
                )?;
            }
            if !context.input.current.is_category(Cat::Spacer) {
                context.input.back_input()?;
            }
            self.dimension_units(
                integer * C::UNITY + round_decimals(&digits),
                negative,
                mu,
                infinite,
            );
        }
        Ok(())
    }
    fn dimension_units(&mut self, number: BigInt, negative: bool, mu: bool, infinite: bool) {
        if infinite {
            self.scan(
                ScanRequest::Keyword("fil".into()),
                move |scanner, _, value| {
                    if value?.flag()? {
                        scanner.fil_order(number, negative, 1);
                    } else {
                        scanner.dimension_relative(number, negative, mu);
                    }
                    Ok(())
                },
            );
        } else {
            self.dimension_relative(number, negative, mu);
        }
    }
    fn finish_dimension(
        &mut self,
        number: BigInt,
        negative: bool,
        order: u8,
        trailing: bool,
    ) -> Result<()> {
        if trailing {
            self.scan(ScanRequest::OptionalSpace, move |scanner, _, result| {
                result?;
                let value = checked_big(if negative { -number } else { number }, true)?;
                scanner.complete(ScanValue::Dimension(value, order));
                Ok(())
            });
        } else {
            let value = checked_big(if negative { -number } else { number }, true)?;
            self.complete(ScanValue::Dimension(value, order));
        }
        Ok(())
    }
    fn fil_order(&mut self, number: BigInt, negative: bool, order: u8) {
        self.scan(
            ScanRequest::Keyword("l".into()),
            move |scanner, context, value| {
                if value?.flag()? {
                    if order == 3 {
                        context.input.recover(
                            Issue::BadNumber,
                            Error::value("Illegal glue order (maximum filll)"),
                            "Capped glue order at filll",
                        )?;
                    }
                    scanner.fil_order(number, negative, (order + 1).min(3));
                } else {
                    scanner.finish_dimension(number, negative, order, true)?;
                }
                Ok(())
            },
        );
    }
    fn dimension_relative(&mut self, number: BigInt, negative: bool, mu: bool) {
        self.scan(ScanRequest::Nonblank, move |scanner, _, value| {
            value?;
            scanner.scan(ScanRequest::Internal, move |scanner, context, value| {
                if let Some(internal) = value?.internal()? {
                    if matches!(internal, InternalValue::Toks(_)) || mu != internal.mu() {
                        return Err(Error::value("Incompatible dimension units"));
                    }
                    let number = number * internal.magnitude()? / C::UNITY;
                    scanner.finish_dimension(number, negative, 0, false)?;
                } else {
                    context.input.back_input()?;
                    if mu {
                        scanner.scan(
                            ScanRequest::Keyword("mu".into()),
                            move |scanner, context, value| {
                                if !value?.flag()? {
                                    context.input.recover(
                                        Issue::MissingUnit,
                                        Error::value("Math glue requires mu units"),
                                        "Assumed mu",
                                    )?;
                                }
                                scanner.finish_dimension(number, negative, 0, true)
                            },
                        );
                    } else {
                        scanner.relative_unit_next(number, negative, 0);
                    }
                }
                Ok(())
            });
            Ok(())
        });
    }
    fn relative_unit_next(&mut self, number: BigInt, negative: bool, index: usize) {
        const UNITS: [&str; 3] = ["em", "ex", "px"];
        if index == 3 {
            self.scan(
                ScanRequest::Keyword("true".into()),
                move |scanner, _, value| {
                    let true_units = value?.flag()?;
                    scanner.absolute_unit_next(number, negative, true_units, 0);
                    Ok(())
                },
            );
            return;
        }
        self.scan(
            ScanRequest::Keyword(UNITS[index].into()),
            move |scanner, context, value| {
                if value?.flag()? {
                    let relative = match index {
                        0 => context.fonts.items[context.fonts.current].get_parameter(6),
                        1 => context.fonts.items[context.fonts.current].get_parameter(5),
                        _ => InputProcessor::parameter(context.env, "pdfpxdimen", 0),
                    };
                    scanner.finish_dimension(number * relative / C::UNITY, negative, 0, true)?;
                } else {
                    scanner.relative_unit_next(number, negative, index + 1);
                }
                Ok(())
            },
        );
    }
    fn absolute_unit_next(
        &mut self,
        number: BigInt,
        negative: bool,
        true_units: bool,
        index: usize,
    ) {
        const RATIOS: [(&str, i64, i64); 11] = [
            ("pt", 1, 1),
            ("pc", 12, 1),
            ("in", 7227, 100),
            ("bp", 7227, 7200),
            ("cm", 7227, 254),
            ("mm", 7227, 2540),
            ("dd", 1238, 1157),
            ("cc", 14856, 1157),
            ("nd", 685, 642),
            ("nc", 1370, 107),
            ("sp", 1, C::UNITY),
        ];
        if index == RATIOS.len() {
            self.value = Some(Ok(ScanValue::Unit));
            self.operations
                .push(Operation::Resume(Box::new(move |scanner, context, _| {
                    if context.input.current.is_category(Cat::RightBrace)
                        || context.input.current.is_category(Cat::LeftBrace)
                    {
                        context.input.recover_with_fix(
                            Issue::MissingUnit,
                            Error::value("Unknown unit in dimension"),
                            "Assumed pt; retained the unexpected input",
                            "pt",
                        )?;
                    } else {
                        context.input.recover(
                            Issue::MissingUnit,
                            Error::value("Unknown unit in dimension"),
                            "Assumed pt; retained the unexpected input",
                        )?;
                    }
                    scanner.finish_dimension(number, negative, 0, false)
                })));
            return;
        }
        let (unit, numerator, denominator) = RATIOS[index];
        self.scan(
            ScanRequest::Keyword(unit.into()),
            move |scanner, _, value| {
                if !value?.flag()? {
                    scanner.absolute_unit_next(number, negative, true_units, index + 1);
                    return Ok(());
                }
                if true_units {
                    scanner.action(
                        ScanAction::PrepareMagnification,
                        move |scanner, _, value| {
                            let mag = value?.integer()?;
                            if mag <= 0 {
                                return Err(Error::value("Illegal magnification")
                                    .with_issue(crate::core::recovery::Issue::PageGeometry));
                            }
                            let number = number * 1000 / mag;
                            scanner.finish_dimension(
                                number * numerator / denominator,
                                negative,
                                0,
                                true,
                            )
                        },
                    );
                } else {
                    scanner.finish_dimension(
                        number * numerator / denominator,
                        negative,
                        0,
                        true,
                    )?;
                }
                Ok(())
            },
        );
    }
    fn glue_begin(&mut self, mu: bool) {
        self.scan(ScanRequest::Signs, move |scanner, _, value| {
            let negative = value?.flag()?;
            scanner.scan(ScanRequest::Internal, move |scanner, context, value| {
                match value?.internal()? {
                    Some(InternalValue::Skip(spec)) if !mu => {
                        scanner.complete(ScanValue::Glue(if negative {
                            spec.scaled(-1, 1)?
                        } else {
                            spec
                        }));
                        return Ok(());
                    }
                    Some(InternalValue::Muskip(spec)) if mu => {
                        scanner.complete(ScanValue::Glue(if negative {
                            spec.scaled(-1, 1)?
                        } else {
                            spec
                        }));
                        return Ok(());
                    }
                    Some(InternalValue::Skip(_) | InternalValue::Muskip(_)) => {
                        return Err(Error::value("Incompatible glue units"));
                    }
                    Some(InternalValue::Dimen(value)) => {
                        if mu {
                            return Err(Error::value("Incompatible glue units"));
                        }
                        scanner.glue_components(if negative { -value } else { value }, mu);
                    }
                    Some(InternalValue::Count(value)) => {
                        let spelling = if negative {
                            -(value as i128)
                        } else {
                            value as i128
                        }
                        .to_string();
                        let tokens = spelling
                            .chars()
                            .map(|code| character_token(Cat::OtherChar, code as i64))
                            .collect::<Result<Vec<_>>>()?;
                        context
                            .input
                            .begin_tokens(tokens.into(), context.input.source(), None)?;
                        scanner.scan(
                            ScanRequest::Dimen {
                                mu,
                                infinite: false,
                            },
                            move |scanner, _, value| {
                                scanner.glue_components(value?.integer()?, mu);
                                Ok(())
                            },
                        );
                    }
                    Some(_) => return Err(Error::value("Missing glue")),
                    None => scanner.scan(
                        ScanRequest::DimensionCurrent {
                            negative,
                            mu,
                            infinite: false,
                        },
                        move |scanner, _, value| {
                            let (width, _) = value?.dimension()?;
                            scanner.glue_components(width, mu);
                            Ok(())
                        },
                    ),
                }
                Ok(())
            });
            Ok(())
        });
    }
    fn glue_components(&mut self, width: i64, mu: bool) {
        self.scan(
            ScanRequest::Keyword("plus".into()),
            move |scanner, _, value| {
                if value?.flag()? {
                    scanner.scan(
                        ScanRequest::Dimension { mu, infinite: true },
                        move |scanner, _, value| {
                            let (stretch, order) = value?.dimension()?;
                            scanner.glue_shrink(width, stretch, order, mu);
                            Ok(())
                        },
                    );
                } else {
                    scanner.glue_shrink(width, 0, 0, mu);
                }
                Ok(())
            },
        );
    }
    fn glue_shrink(&mut self, width: i64, stretch: i64, order: u8, mu: bool) {
        self.scan(
            ScanRequest::Keyword("minus".into()),
            move |scanner, _, value| {
                let spec = GlueSpec {
                    width,
                    stretch,
                    stretch_order: order,
                    protrusion_barrier: true,
                    ..GlueSpec::default()
                };
                if value?.flag()? {
                    scanner.scan(
                        ScanRequest::Dimension { mu, infinite: true },
                        move |scanner, _, value| {
                            let (shrink, shrink_order) = value?.dimension()?;
                            scanner.complete(ScanValue::Glue(GlueSpec {
                                shrink,
                                shrink_order,
                                ..spec
                            }));
                            Ok(())
                        },
                    );
                } else {
                    scanner.complete(ScanValue::Glue(spec));
                }
                Ok(())
            },
        );
    }
}
