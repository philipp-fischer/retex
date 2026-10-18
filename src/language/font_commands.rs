//! Font definitions, copies, character policy and diagnostic identity (`font_commands.py`).
use super::font_execution::{FontAction, FontExecution, FontKind, FontStep, FontValue};
use super::records::{CommandOperand, MeaningCommand};
use super::services::LanguageServices;
use crate::core::constants::Constants as C;
use crate::core::enums::Command;
use crate::core::errors::{Error, Result};
use crate::core::units::print_scaled;
use crate::fonts::catalog::FontCatalog;
pub enum FontInteger {
    Hyphenchar,
    Skewchar,
    NoLigatures,
}
pub trait FontCommandServices: LanguageServices {
    fn use_font(&mut self, number: usize) -> Result<usize>;
}
pub struct FontCommands;
impl FontCommands {
    pub(crate) fn destination_selected(
        services: &mut impl LanguageServices,
        global: bool,
        normalize: bool,
    ) -> (i64, String) {
        let cs = services.input().current.control_sequence;
        let mut name = services.env().cs_name(cs);
        if normalize {
            if cs < C::SINGLE_BASE {
                name = format!("FONT{}", cs - C::ACTIVE_BASE)
            } else if cs == C::NULL_CS {
                name = "FONT".into()
            }
        }
        services.env_mut().define(
            cs,
            MeaningCommand::Operation(Command::SetFont),
            CommandOperand::Number(0),
            global,
        );
        (cs, name)
    }
    pub fn define(services: &mut impl LanguageServices, global: bool) -> Result<usize> {
        Self::drive(services, FontExecution::new(FontKind::Define, global))
    }
    fn drive(services: &mut impl LanguageServices, mut execution: FontExecution) -> Result<usize> {
        let mut returned = None;
        loop {
            match execution.resume(services, returned.take())? {
                FontStep::Complete(number) => return Ok(number),
                FontStep::Yield(action) => {
                    returned = Some(match action {
                        FontAction::Destination => services.get_r_token().map(|_| FontValue::Unit),
                        FontAction::Scan(request) => services.scan(request).map(FontValue::Scan),
                        FontAction::Text => services.text().map(FontValue::Text),
                        FontAction::UseFont(_) => {
                            return Err(Error::runtime(
                                "PDF font service requested by ordinary font driver",
                            ));
                        }
                    });
                }
            }
        }
    }
    pub fn name(catalog: &FontCatalog, number: usize) -> Result<String> {
        catalog.observe_construction_read(number);
        let font = catalog
            .items
            .get(number)
            .ok_or_else(|| Error::value("Invalid font identifier"))?;
        let name = font
            .name
            .as_bytes()
            .iter()
            .map(|&byte| char::from(byte))
            .collect::<String>();
        Ok(name
            + &if Some(font.size) != font.tfm_design_size {
                format!(" at {}pt", print_scaled(font.size))
            } else {
                String::new()
            })
    }
    pub fn identifier(services: &mut impl LanguageServices, number: usize) -> String {
        let escape = services.parameter("escapechar", 92);
        let symbol = services.env_mut().font_symbol(number);
        let mut text = format!(
            "{}{}",
            if (0..=255).contains(&escape) {
                char::from(escape as u8).to_string()
            } else {
                String::new()
            },
            services.env().cs_name(symbol)
        );
        if services.parameter("pdftracingfonts", 0) > 0 {
            let font = &services.fonts().items[number];
            text.push_str(&format!(
                " ({}{}{})",
                font.name,
                if Some(font.size) != font.tfm_design_size {
                    "@"
                } else {
                    ""
                },
                if Some(font.size) != font.tfm_design_size {
                    format!("{}pt", print_scaled(font.size))
                } else {
                    String::new()
                }
            ));
        }
        text
    }
    pub fn integer_reference(services: &mut impl LanguageServices) -> Result<(usize, FontInteger)> {
        let attribute =
            Self::integer_attribute(services.input().current.character().unwrap_or(-1))?;
        Ok((services.scan_font_ident()?, attribute))
    }
    pub(crate) fn integer_attribute(code: i64) -> Result<FontInteger> {
        match code {
            0 => Ok(FontInteger::Hyphenchar),
            1 => Ok(FontInteger::Skewchar),
            2 => Ok(FontInteger::NoLigatures),
            _ => Err(Error::value("Invalid font integer setting")),
        }
    }
    pub fn assign_integer(services: &mut impl LanguageServices) -> Result<()> {
        let code = services.input().current.character().unwrap_or(-1);
        Self::drive(services, FontExecution::new(FontKind::Integer(code), false))?;
        Ok(())
    }
    pub fn copy(services: &mut impl LanguageServices, global: bool) -> Result<usize> {
        Self::drive(services, FontExecution::new(FontKind::Copy, global))
    }
    pub fn letterspace(services: &mut impl LanguageServices, global: bool) -> Result<usize> {
        Self::drive(services, FontExecution::new(FontKind::Letterspace, global))
    }
    pub fn pdf_command(services: &mut impl FontCommandServices, code: u8) -> Result<()> {
        let mut execution = FontExecution::new(FontKind::Pdf(code), false);
        let mut returned = None;
        loop {
            match execution.resume(services, returned.take())? {
                FontStep::Complete(_) => return Ok(()),
                FontStep::Yield(action) => {
                    returned = Some(match action {
                        FontAction::Destination => services.get_r_token().map(|_| FontValue::Unit),
                        FontAction::Scan(request) => services.scan(request).map(FontValue::Scan),
                        FontAction::Text => services.text().map(FontValue::Text),
                        FontAction::UseFont(number) => {
                            services.use_font(number).map(|_| FontValue::Unit)
                        }
                    });
                }
            }
        }
    }
}
