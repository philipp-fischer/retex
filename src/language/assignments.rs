//! Scoped references and atomic TeX assignments (`language/assignments.py`).
use super::character_tokens::Token;
use super::code_validation::{CODE_NAMES, CodeTable, validate_spacefactor};
use super::input::InputProcessor;
use super::records::{
    CommandOperand, MeaningCommand, Quantity, QuantityKind, QuantityReference, ValueIndex, ValueKey,
};
use super::scanning::{InternalValue, ScanRequest, ScanValue};
use crate::core::characters::{LanguageProfile, unicode_case};
use crate::core::constants::Constants as C;
use crate::core::enums::{Command, ListMode};
use crate::core::errors::{Error, Result};
use crate::core::source::SourceSpan;
use crate::core::values::{GlueSpec, checked, trunc_div};
use crate::typesetting::layout::TypesettingContext;
use std::sync::Arc;

pub const KINDS: [QuantityKind; 5] = [
    QuantityKind::Count,
    QuantityKind::Dimen,
    QuantityKind::Skip,
    QuantityKind::Muskip,
    QuantityKind::Toks,
];
pub struct AssignmentContext<'a, 'b> {
    pub typesetting: &'a mut TypesettingContext<'b>,
    pub input: &'a mut InputProcessor,
}
#[derive(Clone, Debug)]
pub struct AssignmentEvent {
    pub key: ValueKey,
    pub value: Quantity,
    pub global: bool,
    pub source: Option<Arc<SourceSpan>>,
}
#[derive(Clone, Copy, Debug)]
pub enum AssignmentRequest {
    Reference,
    Read,
    ScanValue(QuantityKind),
    Assign { global: bool },
    Shorthand { code: usize, global: bool },
    Arithmetic { operation: u8, global: bool },
}
pub enum AssignmentAction {
    Scan(ScanRequest),
    GeneralText,
    GetRToken,
}
pub enum AssignmentValue {
    Unit,
    Scan(ScanValue),
    Tokens(Arc<[Token]>),
    Reference(Option<QuantityReference>),
    Quantity(Option<(QuantityKind, Quantity)>),
}
impl AssignmentValue {
    fn scan(self) -> Result<ScanValue> {
        if let Self::Scan(value) = self {
            Ok(value)
        } else {
            Err(Error::value("Unexpected assignment scanner result"))
        }
    }
    fn reference(self) -> Result<Option<QuantityReference>> {
        if let Self::Reference(value) = self {
            Ok(value)
        } else {
            Err(Error::value("Unexpected assignment reference result"))
        }
    }
    fn quantity(self) -> Result<(QuantityKind, Quantity)> {
        if let Self::Quantity(Some(value)) = self {
            Ok(value)
        } else {
            Err(Error::value("Unexpected assignment value result"))
        }
    }
}
pub enum AssignmentStep {
    Yield(AssignmentAction),
    Complete(AssignmentValue),
}
enum Request {
    Native(AssignmentRequest),
    External(AssignmentAction),
}
type Resume = Box<
    dyn for<'a, 'b> FnOnce(
        &mut Assignments,
        &mut AssignmentContext<'a, 'b>,
        AssignmentValue,
    ) -> Result<()>,
>;
enum Operation {
    Request(Request),
    Resume(Resume),
}
#[derive(Default)]
pub struct Assignments {
    pub after_assignment: Option<Token>,
    pub events: Vec<AssignmentEvent>,
    operations: Vec<Operation>,
    value: Option<AssignmentValue>,
    awaiting: bool,
}
fn kind(index: usize) -> Result<QuantityKind> {
    KINDS
        .get(index)
        .copied()
        .ok_or_else(|| Error::value("Invalid quantity kind"))
}
fn code_table(name: &str) -> Option<CodeTable> {
    Some(match name {
        "catcode" => CodeTable::Catcode,
        "lccode" => CodeTable::Lccode,
        "uccode" => CodeTable::Uccode,
        "sfcode" => CodeTable::Sfcode,
        "mathcode" => CodeTable::Mathcode,
        "delcode" => CodeTable::Delcode,
        _ => return None,
    })
}
fn scalar(value: &Quantity) -> Result<i64> {
    if let Quantity::Integer(value) = value {
        Ok(*value)
    } else {
        Err(Error::value("Expected scalar assignment value"))
    }
}
impl Assignments {
    pub(crate) fn checkpoint_clone(&self) -> Option<Self> {
        if self.awaiting || !self.operations.is_empty() {
            return None;
        }
        Some(Self {
            after_assignment: self.after_assignment,
            events: self.events.clone(),
            value: None,
            ..Self::default()
        })
    }

    pub fn parameter(
        context: &mut AssignmentContext<'_, '_>,
        name: &str,
        kind: QuantityKind,
        default: Quantity,
    ) -> QuantityReference {
        let key = ValueKey::parameter(name);
        context.typesetting.env.values.insert(key.clone(), default);
        QuantityReference { kind, key }
    }
    pub fn get(
        context: &AssignmentContext<'_, '_>,
        kind: QuantityKind,
        key: &ValueKey,
    ) -> Result<Quantity> {
        let result = Self::get_inner(context, kind, key);
        #[cfg(feature = "latency-diagnostics")]
        if crate::output_diagnostics::detailed() {
            let value = match &result {
                Ok(Quantity::Tokens(tokens)) => format!(
                    "tokens={:016x}",
                    crate::output_diagnostics::tokens(tokens, context.typesetting.env)
                ),
                other => format!("{other:?}"),
            };
            crate::output_diagnostics::event("quantity_read", format!("{key:?}|{value}"));
        }
        result
    }
    fn get_inner(
        context: &AssignmentContext<'_, '_>,
        kind: QuantityKind,
        key: &ValueKey,
    ) -> Result<Quantity> {
        if key.table == "fontdimen" {
            let ValueIndex::Pair(font, parameter) = key.index else {
                return Err(Error::value("Invalid font parameter reference"));
            };
            context
                .typesetting
                .fonts
                .observe_construction_read(font as usize);
            return Ok(Quantity::Integer(
                context.typesetting.fonts.items[font as usize].get_parameter(parameter as usize),
            ));
        }
        if key.table == "prevdepth" {
            return Ok(Quantity::Integer(
                context
                    .typesetting
                    .builder
                    .context
                    .prev_depth
                    .unwrap_or(-1000 * C::UNITY),
            ));
        }
        if key.table == "spacefactor" {
            return Ok(Quantity::Integer(
                context.typesetting.builder.context.space_factor,
            ));
        }
        if code_table(&key.table).is_some() {
            if let Some(value) = context.typesetting.env.values.get(key) {
                return Ok(value.clone());
            }
            let ValueIndex::Number(code) = key.index else {
                return Err(Error::value("Invalid character code reference"));
            };
            let value = match key.table.as_str() {
                "catcode" => context
                    .input
                    .category_code(context.typesetting.env, code as u32)?
                    as i64,
                "lccode" | "uccode" if context.input.profile == LanguageProfile::Unicode => {
                    unicode_case(code as u32, key.table == "uccode") as i64
                }
                "lccode" => {
                    if (65..=90).contains(&code) {
                        code + 32
                    } else if (97..=122).contains(&code) {
                        code
                    } else {
                        0
                    }
                }
                "uccode" => {
                    if (97..=122).contains(&code) {
                        code - 32
                    } else if (65..=90).contains(&code) {
                        code
                    } else {
                        0
                    }
                }
                "sfcode" => {
                    if (65..=90).contains(&code) {
                        999
                    } else {
                        1000
                    }
                }
                "mathcode" => {
                    if (65..=90).contains(&code) || (97..=122).contains(&code) {
                        0x7100 + code
                    } else if (48..=57).contains(&code) {
                        0x7000 + code
                    } else {
                        code
                    }
                }
                "delcode" => {
                    if code == 46 {
                        0
                    } else {
                        -1
                    }
                }
                _ => unreachable!(),
            };
            return Ok(Quantity::Integer(value));
        }
        Ok(context
            .typesetting
            .env
            .values
            .get(key)
            .cloned()
            .unwrap_or_else(|| match kind {
                QuantityKind::Count | QuantityKind::Dimen => Quantity::Integer(0),
                QuantityKind::Skip | QuantityKind::Muskip => Quantity::Glue(GlueSpec::default()),
                QuantityKind::Toks => Quantity::Tokens(Arc::from([])),
            }))
    }
    pub fn set(
        &mut self,
        context: &mut AssignmentContext<'_, '_>,
        kind: QuantityKind,
        key: ValueKey,
        mut value: Quantity,
        global: bool,
    ) -> Result<()> {
        if let Some(table) = code_table(&key.table) {
            let scalar = scalar(&value)?;
            if matches!(table, CodeTable::Lccode | CodeTable::Uccode)
                && context.input.profile == LanguageProfile::Unicode
            {
                context.input.profile.validate(scalar)?;
            } else {
                table.validate(scalar)?;
            }
        } else if key.table == "prevdepth" {
            let value = checked(scalar(&value)? as i128, true)?;
            context.typesetting.builder.context.prev_depth = Some(value);
            return Ok(());
        } else if key.table == "spacefactor" {
            let value = scalar(&value)?;
            validate_spacefactor(value)?;
            context.typesetting.builder.context.space_factor = value;
            return Ok(());
        } else if key.table == "fontdimen" {
            // A quantity alias must not bypass the command-level whitelist.
            let ValueIndex::Pair(font, parameter) = key.index else {
                return Err(Error::value("Invalid font parameter reference"));
            };
            if context.typesetting.env.reuse_stateful.get()
                && (!context
                    .typesetting
                    .env
                    .reuse_font_construction_allowed
                    .get()
                    || !context
                        .typesetting
                        .fonts
                        .can_mutate_constructed(font as usize))
            {
                context.typesetting.env.reject_reuse_observation();
            }
            context.typesetting.fonts.items[font as usize]
                .parameter_overrides
                .insert(parameter as usize, scalar(&value)?);
            return Ok(());
        }
        if matches!(kind, QuantityKind::Count | QuantityKind::Dimen) {
            checked(scalar(&value)? as i128, kind == QuantityKind::Dimen)?;
        } else if matches!(kind, QuantityKind::Skip | QuantityKind::Muskip) {
            let Quantity::Glue(spec) = &mut value else {
                return Err(Error::value("Expected glue assignment"));
            };
            if spec.width == 0 && spec.stretch == 0 && spec.shrink == 0 {
                *spec = GlueSpec::default();
            }
        }
        let global = context.typesetting.env.assignment_global(global);
        context
            .typesetting
            .env
            .assign_value(key.clone(), value.clone(), global);
        if key == ValueKey::parameter("pdfdecimaldigits") {
            context.typesetting.env.assign_value(
                ValueKey {
                    table: "pdf_policy".into(),
                    index: ValueIndex::Name("decimal_assigned".into()),
                },
                Quantity::Integer(1),
                global,
            );
        }
        self.events.push(AssignmentEvent {
            key,
            value,
            global,
            source: context.input.source(),
        });
        Ok(())
    }
    pub fn internal(kind: QuantityKind, quantity: Quantity) -> Result<InternalValue> {
        match (kind, quantity) {
            (QuantityKind::Count, Quantity::Integer(value)) => Ok(InternalValue::Count(value)),
            (QuantityKind::Dimen, Quantity::Integer(value)) => Ok(InternalValue::Dimen(value)),
            (QuantityKind::Skip, Quantity::Glue(value)) => Ok(InternalValue::Skip(value)),
            (QuantityKind::Muskip, Quantity::Glue(value)) => Ok(InternalValue::Muskip(value)),
            (QuantityKind::Toks, Quantity::Tokens(value)) => Ok(InternalValue::Toks(value)),
            _ => Err(Error::value("Incompatible internal quantity")),
        }
    }
    pub fn start(&mut self, request: AssignmentRequest) -> Result<()> {
        if !self.operations.is_empty() || self.awaiting {
            return Err(Error::value("Assignment operation already active"));
        }
        self.value = None;
        self.operations
            .push(Operation::Request(Request::Native(request)));
        Ok(())
    }
    pub fn resume(
        &mut self,
        context: &mut AssignmentContext<'_, '_>,
        returned: Option<AssignmentValue>,
    ) -> Result<AssignmentStep> {
        if self.awaiting {
            self.value =
                Some(returned.ok_or_else(|| Error::value("Missing assignment service result"))?);
            self.awaiting = false;
        } else if returned.is_some() {
            return Err(Error::value("Unrequested assignment service result"));
        }
        let result = (|| {
            while let Some(operation) = self.operations.pop() {
                match operation {
                    Operation::Request(Request::External(action)) => {
                        self.awaiting = true;
                        return Ok(AssignmentStep::Yield(action));
                    }
                    Operation::Request(Request::Native(request)) => {
                        self.request(context, request)?
                    }
                    Operation::Resume(resume) => {
                        let value = self
                            .value
                            .take()
                            .ok_or_else(|| Error::value("Missing assignment child result"))?;
                        resume(self, context, value)?;
                    }
                }
            }
            Ok(AssignmentStep::Complete(self.value.take().ok_or_else(
                || Error::value("Missing assignment result"),
            )?))
        })();
        if result.is_err() {
            self.abort();
        }
        result
    }
    pub fn abort(&mut self) {
        self.operations.clear();
        self.awaiting = false;
        self.value = None;
    }
    fn defer(
        &mut self,
        request: Request,
        resume: impl for<'a, 'b> FnOnce(
            &mut Self,
            &mut AssignmentContext<'a, 'b>,
            AssignmentValue,
        ) -> Result<()>
        + 'static,
    ) {
        self.operations.push(Operation::Resume(Box::new(resume)));
        self.operations.push(Operation::Request(request));
    }
    fn scan(
        &mut self,
        request: ScanRequest,
        resume: impl for<'a, 'b> FnOnce(
            &mut Self,
            &mut AssignmentContext<'a, 'b>,
            AssignmentValue,
        ) -> Result<()>
        + 'static,
    ) {
        self.defer(Request::External(AssignmentAction::Scan(request)), resume);
    }
    fn complete(&mut self, value: AssignmentValue) {
        self.value = Some(value);
    }
    fn request(
        &mut self,
        context: &mut AssignmentContext<'_, '_>,
        request: AssignmentRequest,
    ) -> Result<()> {
        match request {
            AssignmentRequest::Reference => self.reference_begin(context)?,
            AssignmentRequest::Read => self.defer(
                Request::Native(AssignmentRequest::Reference),
                |assignments, context, value| {
                    let result = value
                        .reference()?
                        .map(|reference| {
                            Self::get(context, reference.kind, &reference.key)
                                .map(|value| (reference.kind, value))
                        })
                        .transpose()?;
                    assignments.complete(AssignmentValue::Quantity(result));
                    Ok(())
                },
            ),
            AssignmentRequest::ScanValue(kind) => self.scan_value_begin(kind),
            AssignmentRequest::Assign { global } => self.defer(
                Request::Native(AssignmentRequest::Reference),
                move |assignments, _, value| {
                    let reference = value
                        .reference()?
                        .ok_or_else(|| Error::value("Expected assignment reference"))?;
                    assignments.scan(ScanRequest::OptionalEquals, move |assignments, _, value| {
                        value.scan()?;
                        assignments.defer(
                            Request::Native(AssignmentRequest::ScanValue(reference.kind)),
                            move |assignments, context, value| {
                                let (_, value) = value.quantity()?;
                                assignments.set(
                                    context,
                                    reference.kind,
                                    reference.key,
                                    value,
                                    global,
                                )?;
                                assignments.complete(AssignmentValue::Unit);
                                Ok(())
                            },
                        );
                        Ok(())
                    });
                    Ok(())
                },
            ),
            AssignmentRequest::Shorthand { code, global } => self.shorthand_begin(code, global),
            AssignmentRequest::Arithmetic { operation, global } => {
                self.arithmetic_begin(operation, global)
            }
        }
        Ok(())
    }
    fn reference_begin(&mut self, context: &mut AssignmentContext<'_, '_>) -> Result<()> {
        let current = context.input.current.clone();
        let number = current.character().unwrap_or(-1);
        match current.operation() {
            Some(Command::Register) => {
                let kind = kind(number as usize)?;
                self.scan(ScanRequest::Register(None), move |assignments, _, value| {
                    let number = value.scan()?.integer()?;
                    assignments.complete(AssignmentValue::Reference(Some(QuantityReference {
                        kind,
                        key: ValueKey::register(kind, number),
                    })));
                    Ok(())
                });
            }
            Some(Command::RegAlias | Command::Parameter) => {
                let CommandOperand::Quantity(reference) = current.value else {
                    return Err(Error::value("Invalid quantity reference meaning"));
                };
                self.complete(AssignmentValue::Reference(Some(reference)));
            }
            Some(Command::CodeAssign) => {
                let name = CODE_NAMES
                    .get(number as usize)
                    .copied()
                    .ok_or_else(|| Error::value("Invalid code table"))?;
                self.scan(ScanRequest::CharNum, move |assignments, _, value| {
                    let number = value.scan()?.integer()?;
                    assignments.complete(AssignmentValue::Reference(Some(QuantityReference {
                        kind: QuantityKind::Count,
                        key: ValueKey {
                            table: name.into(),
                            index: ValueIndex::Number(number),
                        },
                    })));
                    Ok(())
                });
            }
            Some(Command::FontDimen) => self.scan(ScanRequest::Int, |assignments, _, value| {
                let index = value.scan()?.integer()?;
                assignments.scan(
                    ScanRequest::FontIdent,
                    move |assignments, context, value| {
                        let ScanValue::Font(font) = value.scan()? else {
                            return Err(Error::value("Expected font identifier"));
                        };
                        context.typesetting.fonts.check_parameter(font, index)?;
                        assignments.complete(AssignmentValue::Reference(Some(QuantityReference {
                            kind: QuantityKind::Dimen,
                            key: ValueKey {
                                table: "fontdimen".into(),
                                index: ValueIndex::Pair(font as i64, index),
                            },
                        })));
                        Ok(())
                    },
                );
                Ok(())
            }),
            Some(Command::PrevDepth) => {
                if context.typesetting.builder.context.mode != ListMode::Vertical {
                    return Err(Error::value("Previous depth outside vertical mode"));
                }
                self.complete(AssignmentValue::Reference(Some(QuantityReference {
                    kind: QuantityKind::Dimen,
                    key: ValueKey {
                        table: "prevdepth".into(),
                        index: ValueIndex::Number(0),
                    },
                })));
            }
            Some(Command::SpaceFactor) => {
                if context.typesetting.builder.context.mode != ListMode::Horizontal {
                    return Err(Error::value("Space factor outside horizontal mode"));
                }
                self.complete(AssignmentValue::Reference(Some(QuantityReference {
                    kind: QuantityKind::Count,
                    key: ValueKey {
                        table: "spacefactor".into(),
                        index: ValueIndex::Number(0),
                    },
                })));
            }
            _ => self.complete(AssignmentValue::Reference(None)),
        }
        Ok(())
    }
    fn scan_value_begin(&mut self, kind: QuantityKind) {
        if kind != QuantityKind::Toks {
            let request = match kind {
                QuantityKind::Count => ScanRequest::Int,
                QuantityKind::Dimen => ScanRequest::Dimen {
                    mu: false,
                    infinite: false,
                },
                QuantityKind::Skip | QuantityKind::Muskip => ScanRequest::Glue {
                    mu: kind == QuantityKind::Muskip,
                },
                _ => unreachable!(),
            };
            self.scan(request, move |assignments, _, value| {
                let quantity = match value.scan()? {
                    ScanValue::Integer(value) => Quantity::Integer(value),
                    ScanValue::Glue(value) => Quantity::Glue(value),
                    _ => return Err(Error::value("Unexpected assignment quantity")),
                };
                assignments.complete(AssignmentValue::Quantity(Some((kind, quantity))));
                Ok(())
            });
            return;
        }
        self.scan(ScanRequest::Nonblank, |assignments, _, value| {
            value.scan()?;
            assignments.defer(
                Request::Native(AssignmentRequest::Reference),
                |assignments, context, value| {
                    if let Some(reference) = value.reference()? {
                        if reference.kind != QuantityKind::Toks {
                            return Err(Error::value("Expected a token register"));
                        }
                        let value = Self::get(context, reference.kind, &reference.key)?;
                        assignments
                            .complete(AssignmentValue::Quantity(Some((QuantityKind::Toks, value))));
                    } else {
                        context.input.back_input()?;
                        assignments.defer(
                            Request::External(AssignmentAction::GeneralText),
                            |assignments, _, value| {
                                let AssignmentValue::Tokens(tokens) = value else {
                                    return Err(Error::value(
                                        "Expected token-list assignment text",
                                    ));
                                };
                                assignments.complete(AssignmentValue::Quantity(Some((
                                    QuantityKind::Toks,
                                    Quantity::Tokens(tokens),
                                ))));
                                Ok(())
                            },
                        );
                    }
                    Ok(())
                },
            );
            Ok(())
        });
    }
    fn shorthand_begin(&mut self, code: usize, global: bool) {
        self.defer(
            Request::External(AssignmentAction::GetRToken),
            move |assignments, context, _| {
                let symbol = context.input.current.control_sequence;
                let global = context.typesetting.env.assignment_global(global);
                context.typesetting.env.define(
                    symbol,
                    MeaningCommand::Operation(Command::Relax),
                    CommandOperand::Number(0),
                    global,
                );
                assignments.scan(ScanRequest::OptionalEquals, move |assignments, _, value| {
                    value.scan()?;
                    if code >= KINDS.len() {
                        assignments.scan(ScanRequest::Int, move |assignments, context, value| {
                            let value = value.scan()?.integer()?;
                            let limit = if code == KINDS.len() { 255 } else { 32767 };
                            if !(0..=limit).contains(&value) {
                                return Err(Error::value("Bad character code"));
                            }
                            context.typesetting.env.define(
                                symbol,
                                MeaningCommand::Operation(if code == KINDS.len() {
                                    Command::CharGiven
                                } else {
                                    Command::MathCharNum
                                }),
                                CommandOperand::Number(value),
                                global,
                            );
                            assignments.complete(AssignmentValue::Unit);
                            Ok(())
                        });
                    } else {
                        let kind = kind(code)?;
                        assignments.scan(
                            ScanRequest::Register(None),
                            move |assignments, context, value| {
                                let number = value.scan()?.integer()?;
                                context.typesetting.env.define(
                                    symbol,
                                    MeaningCommand::Operation(Command::RegAlias),
                                    CommandOperand::Quantity(QuantityReference {
                                        kind,
                                        key: ValueKey::register(kind, number),
                                    }),
                                    global,
                                );
                                assignments.complete(AssignmentValue::Unit);
                                Ok(())
                            },
                        );
                    }
                    Ok(())
                });
                Ok(())
            },
        );
    }
    fn arithmetic_begin(&mut self, operation: u8, global: bool) {
        self.scan(ScanRequest::Nonblank, move |assignments, _, value| {
            value.scan()?;
            assignments.defer(
                Request::Native(AssignmentRequest::Reference),
                move |assignments, context, value| {
                    let reference = value.reference()?.ok_or_else(|| {
                        Error::value("Arithmetic requires a register or parameter")
                    })?;
                    if reference.kind == QuantityKind::Toks
                        || code_table(&reference.key.table).is_some()
                        || matches!(reference.key.table.as_str(), "fontdimen" | "spacefactor")
                    {
                        return Err(Error::value(
                            "Arithmetic requires a numeric register or parameter",
                        ));
                    }
                    let old = Self::get(context, reference.kind, &reference.key)?;
                    assignments.scan(
                        ScanRequest::Keyword("by".into()),
                        move |assignments, _, value| {
                            value.scan()?;
                            let operand_kind = reference.kind;
                            let finish = move |assignments: &mut Self,
                                               context: &mut AssignmentContext<'_, '_>,
                                               operand: Quantity|
                                  -> Result<()> {
                                let calculated = (|| -> Result<Quantity> { Ok(match old {
                                    Quantity::Glue(old) => Quantity::Glue(if operation == 0 {
                                        let Quantity::Glue(operand) = operand else {
                                            return Err(Error::value("Expected glue operand"));
                                        };
                                        old.added(operand)?
                                    } else if operation == 1 {
                                        old.scaled(scalar(&operand)?, 1)?
                                    } else {
                                        old.scaled(1, scalar(&operand)?)?
                                    }),
                                    Quantity::Integer(old) => {
                                        let operand = scalar(&operand)?;
                                        Quantity::Integer(checked(
                                            if operation == 0 {
                                                old as i128 + operand as i128
                                            } else if operation == 1 {
                                                old as i128 * operand as i128
                                            } else {
                                                trunc_div(old as i128, operand as i128)?
                                            },
                                            reference.kind == QuantityKind::Dimen,
                                        )?)
                                    }
                                    _ => {
                                        return Err(Error::value(
                                            "Arithmetic requires a numeric register or parameter",
                                        ));
                                    }
                                }) })();
                                let new=match calculated { Ok(v)=>v,Err(error)=>{
                                    context.input.recover(crate::core::recovery::Issue::Arithmetic,error,"Kept the previous register value")?;
                                    assignments.complete(AssignmentValue::Unit); return Ok(());
                                }};
                                assignments.set(
                                    context,
                                    reference.kind,
                                    reference.key,
                                    new,
                                    global,
                                )?;
                                assignments.complete(AssignmentValue::Unit);
                                Ok(())
                            };
                            if operation == 0 {
                                assignments.defer(
                                    Request::Native(AssignmentRequest::ScanValue(operand_kind)),
                                    move |assignments, context, value| {
                                        let (_, operand) = value.quantity()?;
                                        finish(assignments, context, operand)
                                    },
                                );
                            } else {
                                assignments.scan(
                                    ScanRequest::Int,
                                    move |assignments, context, value| {
                                        finish(
                                            assignments,
                                            context,
                                            Quantity::Integer(value.scan()?.integer()?),
                                        )
                                    },
                                );
                            }
                            Ok(())
                        },
                    );
                    Ok(())
                },
            );
            Ok(())
        });
    }
}
