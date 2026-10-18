//! Raw token spelling, diagnostic printing and Roman conversion (`conversions.py`).
use super::character_tokens::{Token, token_category, token_character};
use super::input::InputProcessor;
use super::records::{CommandOperand, MeaningCommand};
use super::state::Environment;
use super::tokens::{MacroDefinition, parameter_number};
use crate::core::constants::Constants as C;
use crate::core::enums::{CharacterCategory as Cat, Command};
use crate::core::errors::{Error, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrimitiveName {
    pub command: Command,
    pub value: CommandOperand,
    pub name: String,
}
pub fn printed_bytes(text: &str, newline: i64, eight_bit: bool, unicode: bool) -> String {
    let mut result = String::new();
    for character in text.chars() {
        let code = character as u32;
        if code as i64 == newline {
            result.push('\n');
        } else if unicode {
            result.push(character);
        } else if code < 32 {
            result.push_str("^^");
            result.push(char::from_u32(code + 64).unwrap());
        } else if code == 127 {
            result.push_str("^^?");
        } else if code >= 128 && !eight_bit {
            result.push_str(&format!("^^{code:02x}"));
        } else {
            result.push(character);
        }
    }
    result
}
pub fn print_tex(stream: &mut dyn Write, text: &str, end: &str, unicode: bool) -> Result<()> {
    if unicode {
        stream.write_all(text.as_bytes())?;
        stream.write_all(end.as_bytes())?;
    } else {
        let bytes: Result<Vec<u8>> = text
            .chars()
            .chain(end.chars())
            .map(|c| u8::try_from(c as u32).map_err(|_| Error::value("Non-octet terminal output")))
            .collect();
        stream.write_all(&bytes?)?;
    }
    stream.flush()?;
    Ok(())
}
pub fn roman_numeral(mut number: i64, limit: Option<usize>) -> Result<String> {
    let mut result = String::new();
    if number <= 0 {
        return Ok(result);
    }
    for (value, text) in [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ] {
        let count = (number / value) as usize;
        number %= value;
        let length = count
            .checked_mul(text.len())
            .and_then(|n| result.len().checked_add(n))
            .ok_or_else(|| Error::value("Conversion output exceeds token limit"))?;
        if limit.is_some_and(|limit| length > limit) {
            return Err(Error::value("Conversion output exceeds token limit"));
        }
        result.push_str(&text.repeat(count));
    }
    Ok(result)
}
pub struct TokenPrinter<'a> {
    pub input: &'a InputProcessor,
    pub env: &'a Environment,
    pub primitives: &'a [PrimitiveName],
    pub font_name: &'a dyn Fn(usize) -> Result<String>,
}
impl TokenPrinter<'_> {
    pub fn escape(&self, name: &str) -> String {
        let code = InputProcessor::parameter(self.env, "escapechar", 92);
        format!(
            "{}{name}",
            if self.input.profile.accepts(code) {
                char::from_u32(code as u32).unwrap().to_string()
            } else {
                String::new()
            }
        )
    }
    pub fn control_sequence(&self, cs: i64, trailing_space: bool) -> Result<String> {
        if let Some(code) = self.env.active_character(cs) {
            return Ok(char::from_u32(code).unwrap().to_string());
        }
        if cs == C::NULL_CS {
            return Ok(format!(
                "{}{}{}",
                self.escape("csname"),
                self.escape("endcsname"),
                if trailing_space { " " } else { "" }
            ));
        }
        let name = self.env.cs_name(cs);
        if cs < C::NULL_CS
            || self
                .env
                .symbol_keys
                .get(&cs)
                .is_some_and(|key| key.starts_with("control-symbol:"))
        {
            let code = if cs < C::NULL_CS {
                (cs - C::SINGLE_BASE) as u32
            } else {
                name.chars()
                    .next()
                    .ok_or_else(|| Error::value("Empty control symbol"))? as u32
            };
            return Ok(format!(
                "{}{}",
                self.escape(
                    &char::from_u32(code)
                        .ok_or_else(|| Error::value("Invalid control symbol"))?
                        .to_string()
                ),
                if trailing_space && self.input.category_code(self.env, code)? == Cat::Letter {
                    " "
                } else {
                    ""
                }
            ));
        }
        Ok(format!(
            "{}{}",
            self.escape(&name),
            if trailing_space { " " } else { "" }
        ))
    }
    pub fn string_token(&self, token: Token) -> Result<String> {
        if let Some(cs) = token.control_sequence_id() {
            self.control_sequence(cs, false)
        } else {
            Ok(char::from_u32(
                token_character(token).ok_or_else(|| Error::value("Unprintable token"))?,
            )
            .ok_or_else(|| Error::value("Invalid character token"))?
            .to_string())
        }
    }
    pub fn display_token(&self, token: Token, match_character: char) -> Result<String> {
        if let Some(cs) = token.control_sequence_id() {
            return self.control_sequence(cs, true);
        }
        if let Some(number) = parameter_number(token) {
            return Ok(format!("{match_character}{number}"));
        }
        let text = self.string_token(token)?;
        Ok(if token_category(token) == Some(Cat::MacParam) {
            text.repeat(2)
        } else {
            text
        })
    }
    pub fn macro_text(&self, definition: &MacroDefinition) -> Result<String> {
        let mut result = String::new();
        let mut marker = '#';
        if let Some(parameters) = &definition.parameters {
            for &token in &parameters.prefix {
                result.push_str(&self.display_token(token, marker)?);
            }
            for (index, delimiter) in parameters.delimiters.iter().enumerate() {
                marker = char::from_u32(parameters.match_characters[index])
                    .ok_or_else(|| Error::value("Invalid parameter marker"))?;
                result.push(marker);
                result.push_str(&(index + 1).to_string());
                for &token in delimiter {
                    result.push_str(&self.display_token(token, marker)?);
                }
            }
        }
        result.push_str("->");
        for &token in definition.body.iter() {
            result.push_str(&self.display_token(token, marker)?);
        }
        Ok(result)
    }
    pub fn meaning(&self, command: MeaningCommand, value: &CommandOperand) -> Result<String> {
        if let MeaningCommand::Character(category) = command {
            let prefix = match category {
                Cat::LeftBrace => "begin-group character ",
                Cat::RightBrace => "end-group character ",
                Cat::MathShift => "math shift character ",
                Cat::TabMark => "alignment tab character ",
                Cat::MacParam => "macro parameter character ",
                Cat::SupMark => "superscript character ",
                Cat::SubMark => "subscript character ",
                Cat::Spacer => "blank space ",
                Cat::Letter => "the letter ",
                Cat::OtherChar => "the character ",
                _ => "[unknown command code!]",
            };
            let CommandOperand::Number(code) = value else {
                return Err(Error::value("Invalid character meaning"));
            };
            return Ok(format!(
                "{prefix}{}",
                char::from_u32(*code as u32)
                    .ok_or_else(|| Error::value("Invalid character meaning"))?
            ));
        }
        let MeaningCommand::Operation(command) = command else {
            unreachable!()
        };
        Ok(match command {
            Command::Macro => {
                let CommandOperand::Macro(definition) = value else {
                    return Err(Error::value("Invalid macro meaning"));
                };
                let mut prefix = String::new();
                if definition.protected {
                    prefix.push_str(&self.escape("protected"));
                }
                if definition.long {
                    prefix.push_str(&self.escape("long"));
                }
                if definition.outer {
                    prefix.push_str(&self.escape("outer"));
                }
                format!(
                    "{}{}macro:{}",
                    prefix,
                    if prefix.is_empty() { "" } else { " " },
                    self.macro_text(definition)?
                )
            }
            Command::UndefinedCs => "undefined".into(),
            Command::CharGiven | Command::MathCharNum => {
                let CommandOperand::Number(code) = value else {
                    return Err(Error::value("Invalid character constant"));
                };
                format!(
                    "{}\"{code:X}",
                    self.escape(if command == Command::CharGiven {
                        "char"
                    } else {
                        "mathchar"
                    })
                )
            }
            Command::RegAlias => {
                let CommandOperand::Quantity(reference) = value else {
                    return Err(Error::value("Invalid register alias"));
                };
                let super::records::ValueIndex::Number(number) = reference.key.index else {
                    return Err(Error::value("Invalid register alias"));
                };
                format!("{}{number}", self.escape(reference.kind.name()))
            }
            Command::SetFont => {
                let CommandOperand::Number(number) = value else {
                    return Err(Error::value("Invalid font meaning"));
                };
                format!("select font {}", (self.font_name)(*number as usize)?)
            }
            Command::ParEnd => self.escape("par"),
            Command::Relax => self.escape("relax"),
            _ => self
                .primitives
                .iter()
                .find(|primitive| primitive.command == command && &primitive.value == value)
                .map_or_else(
                    || "[unknown command code!]".into(),
                    |primitive| self.escape(&primitive.name),
                ),
        })
    }
}
