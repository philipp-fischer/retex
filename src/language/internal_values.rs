//! Internal quantities and unexpanded `the`/detokenize text (`internal_values.py`).
use super::assignments::Assignments;
use super::character_tokens::Token;
use super::character_tokens::character_token;
use super::expansion::ExpansionValue;
use super::font_commands::{FontCommands, FontInteger};
use super::records::{CommandOperand, Quantity, QuantityKind, ValueIndex, ValueKey};
use super::scanning::InternalValue;
use super::scanning::{ScanRequest, ScanValue};
use super::services::ListServices;
use crate::core::constants::Constants as C;
use crate::core::enums::{CharacterCategory as Cat, Command, ListMode};
use crate::core::errors::{Error, Result};
use crate::core::units::print_scaled;
use crate::fonts::microtypography::code_value;
use crate::typesetting::math_families::MathFamilies;
use std::sync::Arc;
pub trait InternalServices: ListServices {
    fn resolve_primitive(&mut self) -> Result<()>;
    fn read_penalty(&mut self, code: usize) -> Result<i64>;
    fn read_extended(&mut self, code: usize) -> Result<InternalValue>;
    fn read_expression(&mut self, code: usize) -> Result<InternalValue>;
    fn read_last_item(&mut self, code: i64) -> Result<(QuantityKind, Quantity)>;
    fn read_pdf_item(&self, name: &str) -> Result<i64>;
    fn page_dimension(&self, code: usize) -> Result<i64>;
    fn page_dead_cycles(&self) -> i64;
    fn page_insert_penalties(&self) -> i64;
    fn interaction(&self) -> u8;
    fn vertical_prev_graf(&self) -> i64;
    fn read_assignment(&mut self) -> Result<Option<(QuantityKind, Quantity)>>;
}
pub struct InternalValues;
impl InternalValues {
    pub fn read(services: &mut impl InternalServices) -> Result<Option<InternalValue>> {
        match Self::drive(services, InternalRequest::Read)? {
            InternalResult::Quantity(value) => Ok(value),
            _ => Err(Error::runtime("Invalid internal read result")),
        }
    }
    pub fn the_text(services: &mut impl InternalServices) -> Result<ExpansionValue> {
        match Self::drive(services, InternalRequest::The)? {
            InternalResult::Text(value) => Ok(value),
            _ => Err(Error::runtime("Invalid internal text result")),
        }
    }
    fn drive(
        services: &mut impl InternalServices,
        request: InternalRequest,
    ) -> Result<InternalResult> {
        let mut execution = InternalExecution::new(request);
        let mut returned = None;
        loop {
            match execution.resume(services, returned.take())? {
                InternalStep::Complete(value) => return Ok(value),
                InternalStep::Yield(action) => {
                    returned = Some(match action {
                        InternalAction::Scan(request) => {
                            services.scan(request).map(InternalValueResult::Scan)
                        }
                        InternalAction::AssignmentRead => services
                            .read_assignment()
                            .map(InternalValueResult::Assignment),
                        InternalAction::Extended(code) => services
                            .read_extended(code)
                            .map(InternalValueResult::Internal),
                        InternalAction::Expression(code) => services
                            .read_expression(code)
                            .map(InternalValueResult::Internal),
                        InternalAction::GeneralText => services
                            .general_text(false)
                            .map(InternalValueResult::Tokens),
                        InternalAction::Read => Self::read(services).map(InternalValueResult::Read),
                    })
                }
            }
        }
    }
    fn format(
        services: &mut impl InternalServices,
        value: InternalValue,
    ) -> Result<ExpansionValue> {
        Ok(match value {
            InternalValue::Font(number) => {
                ExpansionValue::Tokens(vec![services.env_mut().font_token(number, None)].into())
            }
            value @ InternalValue::Dimen(_) => {
                ExpansionValue::Text(format!("{}pt", print_scaled(value.magnitude()?)))
            }
            InternalValue::Skip(glue) => ExpansionValue::Text(glue.text(false)),
            InternalValue::Muskip(glue) => ExpansionValue::Text(glue.text(true)),
            InternalValue::Toks(tokens) => ExpansionValue::Tokens(tokens),
            InternalValue::Count(value) => ExpansionValue::Text(value.to_string()),
        })
    }
}

pub enum InternalRequest {
    Read,
    The,
}
pub enum InternalAction {
    Scan(ScanRequest),
    AssignmentRead,
    Extended(usize),
    Expression(usize),
    GeneralText,
    Read,
}
pub enum InternalValueResult {
    Scan(ScanValue),
    Assignment(Option<(QuantityKind, Quantity)>),
    Internal(InternalValue),
    Tokens(Arc<[Token]>),
    Read(Option<InternalValue>),
}
pub enum InternalResult {
    Quantity(Option<InternalValue>),
    Text(ExpansionValue),
}
pub enum InternalStep {
    Yield(InternalAction),
    Complete(InternalResult),
}
enum Phase {
    Start,
    Internal,
    Penalty,
    Family,
    FontInteger,
    FontCodeFont,
    FontCodeCharacter,
    Assignment,
    BoxRegister,
    TheToken,
    TheRead,
    TheText,
    Done,
}
pub struct InternalExecution {
    request: InternalRequest,
    phase: Phase,
    number: i64,
    font: usize,
    setting: Option<crate::fonts::microtypography::FontCode>,
    integer: Option<FontInteger>,
}
fn scan(value: Option<InternalValueResult>) -> Result<ScanValue> {
    if let Some(InternalValueResult::Scan(value)) = value {
        Ok(value)
    } else {
        Err(Error::runtime("Invalid internal scanner result"))
    }
}
impl InternalExecution {
    pub fn new(request: InternalRequest) -> Self {
        Self {
            request,
            phase: Phase::Start,
            number: 0,
            font: 0,
            setting: None,
            integer: None,
        }
    }
    fn request(&mut self, phase: Phase, action: InternalAction) -> InternalStep {
        self.phase = phase;
        InternalStep::Yield(action)
    }
    fn quantity(&mut self, value: Option<InternalValue>) -> InternalStep {
        self.phase = Phase::Done;
        InternalStep::Complete(InternalResult::Quantity(value))
    }
    fn text(&mut self, value: ExpansionValue) -> InternalStep {
        self.phase = Phase::Done;
        InternalStep::Complete(InternalResult::Text(value))
    }
    pub fn resume(
        &mut self,
        services: &mut impl InternalServices,
        returned: Option<Result<InternalValueResult>>,
    ) -> Result<InternalStep> {
        let value = returned.transpose()?;
        use InternalValue as V;
        Ok(match self.phase {
            Phase::Start => {
                if value.is_some() {
                    return Err(Error::runtime("Unrequested internal result"));
                }
                if matches!(self.request, InternalRequest::The) {
                    self.number = services.input().current.character().unwrap_or(0);
                    if self.number == 1 || self.number == 3 {
                        self.request(Phase::TheText, InternalAction::GeneralText)
                    } else {
                        self.request(Phase::TheToken, InternalAction::Scan(ScanRequest::Nonblank))
                    }
                } else {
                    services.resolve_primitive()?;
                    let current = services.input().current.clone();
                    if services.env().reuse_stateful.get() {
                        if current.is_command(Command::LastItem) && current.character() == Some(3) {
                            services.env().reuse_line_sensitive.set(true);
                        }
                        let allowed = (services.env().reuse_font_construction_allowed.get()
                            && matches!(
                                current.operation(),
                                Some(Command::FontInt | Command::FontDimen)
                            ))
                            || current.operation().is_none()
                            || current.is_command(Command::Relax)
                            || (current.is_command(Command::LastItem)
                                && matches!(current.character(), Some(0..=3 | 5)))
                            || (current.is_command(Command::EtexItem)
                                && matches!(current.character(), Some(0..=11 | 16..=17 | 19..=21)))
                            || matches!(
                                current.operation(),
                                Some(
                                    Command::CodeAssign
                                        | Command::Parameter
                                        | Command::Register
                                        | Command::RegAlias
                                        | Command::Expression
                                        | Command::BoxDimen
                                        | Command::PrevGraf
                                        | Command::SpaceFactor
                                        | Command::PrevDepth
                                        | Command::CharGiven
                                        | Command::MathCharNum
                                        | Command::ParShape
                                        | Command::Penalties
                                )
                            )
                            || (current.is_command(Command::PdfItem)
                                && matches!(&current.value,CommandOperand::Text(name)
                                if ["pdflastximage","pdflastximagepages","pdflastximagecolordepth"].contains(&name.as_str())));
                        if !allowed {
                            services.env().reuse_rejected_by.set(Some(
                                current
                                    .operation()
                                    .map_or("internal-value", Command::as_str),
                            ));
                            services.env().reject_reuse_observation();
                        }
                    }
                    self.number = current.character().unwrap_or(-1);
                    match current.operation() {
                        Some(Command::Penalties) => {
                            self.request(Phase::Penalty, InternalAction::Scan(ScanRequest::Int))
                        }
                        Some(Command::EtexItem) => self.request(
                            Phase::Internal,
                            InternalAction::Extended(self.number as usize),
                        ),
                        Some(Command::Expression) => self.request(
                            Phase::Internal,
                            InternalAction::Expression(self.number as usize),
                        ),
                        Some(Command::LastItem) => {
                            let (kind, value) = services.read_last_item(self.number)?;
                            self.quantity(Some(Assignments::internal(kind, value)?))
                        }
                        Some(Command::PdfItem) => {
                            let CommandOperand::Text(name) = current.value else {
                                return Err(Error::value("Invalid PDF internal item"));
                            };
                            self.quantity(Some(V::Count(services.read_pdf_item(&name)?)))
                        }
                        Some(Command::PageDimen) => self.quantity(Some(V::Dimen(
                            services.page_dimension(self.number as usize)?,
                        ))),
                        Some(Command::PageInt) => {
                            self.quantity(Some(V::Count(match self.number {
                                2 => services.interaction() as i64,
                                0 => services.page_dead_cycles(),
                                _ => services.page_insert_penalties(),
                            })))
                        }
                        Some(Command::MathFamily) => self.request(
                            Phase::Family,
                            InternalAction::Scan(ScanRequest::Register(Some(15))),
                        ),
                        Some(Command::ParShape) => self.quantity(Some(V::Count(
                            match services.env().values.get(&ValueKey {
                                table: "parshape".into(),
                                index: ValueIndex::Number(0),
                            }) {
                                Some(Quantity::Shape(shape)) => shape.len() as i64,
                                _ => 0,
                            },
                        ))),
                        Some(Command::PrevGraf) => self.quantity(Some(V::Count(
                            if services.builder().context.mode == ListMode::Disabled {
                                0
                            } else {
                                services.vertical_prev_graf()
                            },
                        ))),
                        Some(Command::FontInt) => {
                            self.integer = Some(FontCommands::integer_attribute(self.number)?);
                            self.request(
                                Phase::FontInteger,
                                InternalAction::Scan(ScanRequest::FontIdent),
                            )
                        }
                        Some(Command::FontCode) => {
                            let CommandOperand::FontCode(setting) = current.value else {
                                return Err(Error::value("Invalid font character code"));
                            };
                            self.setting = Some(setting);
                            self.request(
                                Phase::FontCodeFont,
                                InternalAction::Scan(ScanRequest::FontIdent),
                            )
                        }
                        Some(Command::CharGiven | Command::MathCharNum) => {
                            self.quantity(Some(V::Count(self.number)))
                        }
                        _ => self.request(Phase::Assignment, InternalAction::AssignmentRead),
                    }
                }
            }
            Phase::Internal => {
                let Some(InternalValueResult::Internal(value)) = value else {
                    return Err(Error::runtime("Invalid internal service result"));
                };
                self.quantity(Some(value))
            }
            Phase::Penalty => {
                let index = scan(value)?.integer()?;
                self.quantity(Some(V::Count(
                    crate::typesetting::penalties::Penalties::at(
                        services.env(),
                        self.number,
                        index,
                    ),
                )))
            }
            Phase::Family => {
                let family = scan(value)?.integer()?;
                self.quantity(Some(V::Font(MathFamilies::family(
                    services.env(),
                    self.number as usize,
                    family,
                ))))
            }
            Phase::FontInteger => {
                let ScanValue::Font(number) = scan(value)? else {
                    return Err(Error::runtime("Invalid internal font result"));
                };
                services.fonts().observe_construction_read(number);
                let font = &services.fonts().items[number];
                self.quantity(Some(V::Count(match self.integer {
                    Some(FontInteger::Hyphenchar) => font.hyphenchar,
                    Some(FontInteger::Skewchar) => font.skewchar,
                    Some(FontInteger::NoLigatures) => i64::from(font.no_ligatures()),
                    None => return Err(Error::runtime("Missing internal font setting")),
                })))
            }
            Phase::FontCodeFont => {
                let ScanValue::Font(number) = scan(value)? else {
                    return Err(Error::runtime("Invalid internal font result"));
                };
                self.font = number;
                self.request(
                    Phase::FontCodeCharacter,
                    InternalAction::Scan(ScanRequest::CharNum),
                )
            }
            Phase::FontCodeCharacter => {
                let character = scan(value)?.integer()? as u32;
                self.quantity(Some(V::Count(code_value(
                    &services.fonts().items[self.font],
                    self.setting.expect("internal font code"),
                    character,
                ))))
            }
            Phase::Assignment => {
                let Some(InternalValueResult::Assignment(value)) = value else {
                    return Err(Error::runtime("Invalid internal assignment result"));
                };
                if let Some((kind, value)) = value {
                    self.quantity(Some(Assignments::internal(kind, value)?))
                } else if services.input().current.is_command(Command::BoxDimen) {
                    self.number = services.input().current.character().unwrap();
                    self.request(
                        Phase::BoxRegister,
                        InternalAction::Scan(ScanRequest::Register(None)),
                    )
                } else {
                    self.quantity(None)
                }
            }
            Phase::BoxRegister => {
                let register = scan(value)?.integer()?;
                let pointer = services
                    .env()
                    .box_registers
                    .get(&register)
                    .copied()
                    .unwrap_or(C::NULL);
                let dimension = if pointer == C::NULL {
                    0
                } else {
                    let node = services.builder().node(pointer)?;
                    match self.number {
                        0 => node.width,
                        1 => node.height,
                        2 => node.depth,
                        _ => return Err(Error::value("Invalid box dimension")),
                    }
                };
                #[cfg(feature = "latency-diagnostics")]
                if crate::output_diagnostics::detailed() {
                    crate::output_diagnostics::event(
                        "box_dimension_read",
                        format!(
                            "register={register}|dimension={}|value={dimension}",
                            self.number
                        ),
                    );
                }
                self.quantity(Some(V::Dimen(dimension)))
            }
            Phase::TheToken => {
                scan(value)?;
                services.resolve_primitive()?;
                if services.input().current.is_command(Command::DefFont)
                    || services.input().current.is_command(Command::SetFont)
                {
                    let number = if services.input().current.is_command(Command::DefFont) {
                        services.fonts().current
                    } else {
                        services.input().current.character().unwrap() as usize
                    };
                    self.text(ExpansionValue::Tokens(
                        vec![services.env_mut().font_token(number, None)].into(),
                    ))
                } else {
                    self.request(Phase::TheRead, InternalAction::Read)
                }
            }
            Phase::TheRead => {
                let Some(InternalValueResult::Read(value)) = value else {
                    return Err(Error::runtime("Invalid internal text read result"));
                };
                let value =
                    value.ok_or_else(|| Error::value("Expected an internal quantity after the"))?;
                self.text(InternalValues::format(services, value)?)
            }
            Phase::TheText => {
                let Some(InternalValueResult::Tokens(tokens)) = value else {
                    return Err(Error::runtime("Invalid internal text tokens"));
                };
                if self.number == 1 {
                    self.text(ExpansionValue::Tokens(tokens))
                } else {
                    let mut text = String::new();
                    for &token in tokens.iter() {
                        text.push_str(&services.display_token(token)?);
                    }
                    let tokens: Result<Vec<_>> = text
                        .chars()
                        .map(|character| {
                            character_token(
                                if character == ' ' {
                                    Cat::Spacer
                                } else {
                                    Cat::OtherChar
                                },
                                character as i64,
                            )
                        })
                        .collect();
                    self.text(ExpansionValue::Tokens(tokens?.into()))
                }
            }
            Phase::Done => return Err(Error::runtime("Internal result already consumed")),
        })
    }
}
