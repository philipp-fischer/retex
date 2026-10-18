//! Owned font command syntax, preserving font_commands.py's operand/effect order.
use super::{
    font_commands::{FontCommands, FontInteger},
    records::{CommandOperand, MeaningCommand},
    scanning::{ScanRequest, ScanValue},
    services::LanguageServices,
};
use crate::{
    core::{
        constants::Constants as C,
        enums::Command,
        errors::{Error, Result},
        source::SourceSpan,
    },
    fonts::catalog::FontCatalog,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

pub enum FontKind {
    Define,
    Copy,
    Letterspace,
    Integer(i64),
    Code(CommandOperand),
    Pdf(u8),
}
pub enum FontAction {
    Destination,
    Scan(ScanRequest),
    Text,
    UseFont(usize),
}
pub enum FontValue {
    Unit,
    Scan(ScanValue),
    Text(String),
}
pub enum FontStep {
    Yield(FontAction),
    Complete(usize),
}
#[derive(Clone, Copy)]
enum Phase {
    Start,
    Destination,
    Equals,
    Filename,
    At,
    Scaled,
    Size,
    Magnification,
    Source,
    Amount,
    NoLig,
    FontNumber,
    IntegerEquals,
    IntegerValue,
    Character,
    CodeEquals,
    CodeValue,
    PdfUse,
    PdfText,
    Done,
}
pub struct FontExecution {
    kind: FontKind,
    phase: Phase,
    global: bool,
    source: Option<Arc<SourceSpan>>,
    directory: PathBuf,
    cs: i64,
    name: String,
    filename: OsString,
    size: Option<i64>,
    scaled: i64,
    font: usize,
    amount: i64,
    character: u32,
    integer: Option<FontInteger>,
}
fn scan(value: Option<FontValue>) -> Result<ScanValue> {
    if let Some(FontValue::Scan(value)) = value {
        Ok(value)
    } else {
        Err(Error::runtime("Invalid font scanner result"))
    }
}
fn unit(value: Option<FontValue>) -> Result<()> {
    if matches!(value, Some(FontValue::Unit)) {
        Ok(())
    } else {
        Err(Error::runtime("Invalid font service result"))
    }
}
impl FontExecution {
    pub fn new(kind: FontKind, global: bool) -> Self {
        Self {
            kind,
            phase: Phase::Start,
            global,
            source: None,
            directory: PathBuf::new(),
            cs: 0,
            name: String::new(),
            filename: OsString::new(),
            size: None,
            scaled: 1000,
            font: 0,
            amount: 0,
            character: 0,
            integer: None,
        }
    }
    fn request(&mut self, phase: Phase, request: ScanRequest) -> FontStep {
        self.phase = phase;
        FontStep::Yield(FontAction::Scan(request))
    }
    fn bind(&self, services: &mut impl LanguageServices, number: usize) {
        services.env_mut().define(
            self.cs,
            MeaningCommand::Operation(Command::SetFont),
            CommandOperand::Number(number as i64),
            self.global,
        );
        services.env_mut().font_token(number, Some(&self.name));
    }
    fn defaults(&self, services: &mut impl LanguageServices, number: usize) {
        let hyphen = services.parameter("defaulthyphenchar", 45);
        let skew = services.parameter("defaultskewchar", -1);
        let font = &mut services.fonts_mut().items[number];
        font.hyphenchar = hyphen;
        font.skewchar = skew;
    }
    fn loaded(&mut self, services: &mut impl LanguageServices) -> Result<FontStep> {
        let old_count = services.fonts().items.len();
        let number = services.fonts_mut().load(
            self.filename
                .to_str()
                .ok_or_else(|| Error::value("Non-UTF-8 font filename"))?,
            &self.directory,
            self.size,
            self.scaled,
        )?;
        self.bind(services, number);
        if number == old_count {
            self.defaults(services, number);
        }
        let font = &services.fonts().items[number];
        let fields = serde_json::json!({"identifier":self.name,"font":number,"name":font.name,"size_sp":font.size,"design_size_sp":font.tfm_design_size,"tfm_file":font.tfm_file,"pfb_file":font.materialization.pfb_file,"source":self.source});
        services.event("font_load", fields);
        self.phase = Phase::Done;
        Ok(FontStep::Complete(number))
    }
    pub fn resume(
        &mut self,
        services: &mut impl LanguageServices,
        returned: Option<Result<FontValue>>,
    ) -> Result<FontStep> {
        let value = returned.transpose()?;
        Ok(match self.phase {
            Phase::Start => {
                if value.is_some() {
                    return Err(Error::runtime("Unrequested font service result"));
                }
                match self.kind {
                    FontKind::Define | FontKind::Copy | FontKind::Letterspace => {
                        if matches!(self.kind, FontKind::Define) {
                            self.source = services.input().source();
                            self.directory = services
                                .input()
                                .cursor
                                .name
                                .as_ref()
                                .map(|name| {
                                    Path::new(name).parent().unwrap_or_else(|| Path::new(""))
                                })
                                .map(Path::to_path_buf)
                                .unwrap_or_default();
                        }
                        self.phase = Phase::Destination;
                        FontStep::Yield(FontAction::Destination)
                    }
                    FontKind::Integer(code) => {
                        self.integer = Some(FontCommands::integer_attribute(code)?);
                        self.request(Phase::FontNumber, ScanRequest::FontIdent)
                    }
                    _ => self.request(Phase::FontNumber, ScanRequest::FontIdent),
                }
            }
            Phase::Destination => {
                unit(value)?;
                (self.cs, self.name) = FontCommands::destination_selected(
                    services,
                    self.global,
                    !matches!(self.kind, FontKind::Letterspace),
                );
                self.request(Phase::Equals, ScanRequest::OptionalEquals)
            }
            Phase::Equals => {
                scan(value)?;
                if matches!(self.kind, FontKind::Define) {
                    self.request(Phase::Filename, ScanRequest::Filename)
                } else {
                    self.request(Phase::Source, ScanRequest::FontIdent)
                }
            }
            Phase::Filename => {
                let ScanValue::Filename(filename) = scan(value)? else {
                    return Err(Error::runtime("Invalid font filename result"));
                };
                self.filename = filename;
                self.request(Phase::At, ScanRequest::Keyword("at".into()))
            }
            Phase::At => {
                if scan(value)?.flag()? {
                    self.request(
                        Phase::Size,
                        ScanRequest::Dimen {
                            mu: false,
                            infinite: false,
                        },
                    )
                } else {
                    self.request(Phase::Scaled, ScanRequest::Keyword("scaled".into()))
                }
            }
            Phase::Scaled => {
                if scan(value)?.flag()? {
                    self.request(Phase::Magnification, ScanRequest::Int)
                } else {
                    return self.loaded(services);
                }
            }
            Phase::Size => {
                let amount = scan(value)?.integer()?;
                if !(0 < amount && amount < 2048 * C::UNITY) {
                    return Err(Error::value(
                        "Font at size must be positive and less than 2048pt",
                    ));
                }
                self.size = Some(amount);
                return self.loaded(services);
            }
            Phase::Magnification => {
                self.scaled = scan(value)?.integer()?;
                if !(1..=32768).contains(&self.scaled) {
                    return Err(Error::value(
                        "Font scaled magnification must be between 1 and 32768",
                    ));
                }
                return self.loaded(services);
            }
            Phase::Source => {
                let ScanValue::Font(number) = scan(value)? else {
                    return Err(Error::runtime("Invalid font source result"));
                };
                self.font = number;
                if matches!(self.kind, FontKind::Copy) {
                    let number = services.fonts_mut().copy(number)?;
                    self.bind(services, number);
                    self.phase = Phase::Done;
                    FontStep::Complete(number)
                } else {
                    self.request(Phase::Amount, ScanRequest::Int)
                }
            }
            Phase::Amount => {
                self.amount = scan(value)?.integer()?.clamp(-1000, 1000);
                self.request(Phase::NoLig, ScanRequest::Keyword("nolig".into()))
            }
            Phase::NoLig => {
                let nolig = scan(value)?.flag()?;
                let number = services
                    .fonts_mut()
                    .letterspace(self.font, self.amount, nolig)?;
                self.defaults(services, number);
                self.bind(services, number);
                self.phase = Phase::Done;
                FontStep::Complete(number)
            }
            Phase::FontNumber => {
                let ScanValue::Font(number) = scan(value)? else {
                    return Err(Error::runtime("Invalid font identifier result"));
                };
                self.font = number;
                match self.kind {
                    FontKind::Integer(_) => {
                        if services.env().reuse_stateful.get()
                            && (!services.env().reuse_font_construction_allowed.get()
                                || !services.fonts().can_mutate_constructed(number))
                        {
                            services
                                .env()
                                .reuse_rejected_by
                                .set(Some("existing-font-integer-mutation"));
                            services.env().reject_reuse_observation();
                        }
                        if matches!(self.integer, Some(FontInteger::NoLigatures)) {
                            FontCatalog::disable_ligatures(&mut services.fonts_mut().items[number]);
                            self.phase = Phase::Done;
                            FontStep::Complete(number)
                        } else {
                            self.request(Phase::IntegerEquals, ScanRequest::OptionalEquals)
                        }
                    }
                    FontKind::Code(_) => self.request(Phase::Character, ScanRequest::CharNum),
                    FontKind::Pdf(code) => {
                        if number == 0 {
                            return Err(Error::value("Invalid PDF font identifier"));
                        }
                        match code {
                            0 => {
                                self.phase = Phase::PdfUse;
                                FontStep::Yield(FontAction::UseFont(number))
                            }
                            1 => {
                                self.phase = Phase::PdfText;
                                FontStep::Yield(FontAction::Text)
                            }
                            _ => {
                                services.fonts_mut().items[number]
                                    .materialization
                                    .no_builtin_tounicode = true;
                                self.phase = Phase::Done;
                                FontStep::Complete(number)
                            }
                        }
                    }
                    _ => return Err(Error::runtime("Invalid font execution phase")),
                }
            }
            Phase::IntegerEquals => {
                scan(value)?;
                self.request(Phase::IntegerValue, ScanRequest::Int)
            }
            Phase::IntegerValue => {
                let amount = scan(value)?.integer()?;
                let font = &mut services.fonts_mut().items[self.font];
                match self.integer {
                    Some(FontInteger::Hyphenchar) => font.hyphenchar = amount,
                    Some(FontInteger::Skewchar) => font.skewchar = amount,
                    _ => return Err(Error::runtime("Invalid font integer execution phase")),
                }
                self.phase = Phase::Done;
                FontStep::Complete(self.font)
            }
            Phase::Character => {
                self.character = scan(value)?.integer()? as u32;
                self.request(Phase::CodeEquals, ScanRequest::OptionalEquals)
            }
            Phase::CodeEquals => {
                scan(value)?;
                self.request(Phase::CodeValue, ScanRequest::Int)
            }
            Phase::CodeValue => {
                let amount = scan(value)?.integer()?;
                let FontKind::Code(CommandOperand::FontCode(code)) = self.kind else {
                    return Err(Error::value("Invalid font code"));
                };
                crate::fonts::microtypography::set_code(
                    &mut services.fonts_mut().items[self.font],
                    code,
                    self.character,
                    amount,
                );
                self.phase = Phase::Done;
                FontStep::Complete(self.font)
            }
            Phase::PdfUse => {
                unit(value)?;
                self.phase = Phase::PdfText;
                FontStep::Yield(FontAction::Text)
            }
            Phase::PdfText => {
                let Some(FontValue::Text(text)) = value else {
                    return Err(Error::runtime("Invalid PDF font text result"));
                };
                if matches!(self.kind, FontKind::Pdf(0)) {
                    let bytes = text
                        .chars()
                        .map(|character| {
                            u8::try_from(character as u32)
                                .map_err(|_| Error::value("Non-octet PDF font inclusion text"))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    services.fonts_mut().items[self.font]
                        .materialization
                        .included_codes
                        .extend(bytes.into_iter().map(u32::from));
                } else {
                    services.fonts_mut().items[self.font]
                        .materialization
                        .font_attributes = text;
                }
                self.phase = Phase::Done;
                FontStep::Complete(self.font)
            }
            Phase::Done => return Err(Error::runtime("Font result already consumed")),
        })
    }
}
