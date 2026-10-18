//! Physical characters and double-superscript decoding (`physical_input.py`).
use super::input::InputProcessor;
use super::records::{CommandOperand, MeaningCommand};
use super::state::Environment;
use crate::core::characters::LanguageProfile;
use crate::core::constants::Constants as C;
use crate::core::enums::{CharacterCategory as Cat, InputStateKind as State};
use crate::core::errors::{Error, Result};
use crate::core::source::SourceSpan;
use std::sync::Arc;

impl InputProcessor {
    pub fn input_from_file(&mut self, env: &mut Environment) -> Result<bool> {
        loop {
            if self.cursor.loc as isize > self.cursor.limit {
                self.cursor.state = State::NewLine;
                if !self.move_to_next_line(env)? {
                    return Ok(false);
                }
                continue;
            }
            let loc = self.cursor.loc;
            self.reduce_expanded_character(env, loc)?;
            let name = self.cursor.name.as_deref().unwrap_or("");
            let file = self
                .cursor
                .source
                .as_ref()
                .filter(|source| source.file.as_ref() == name)
                .map(|source| source.file.clone())
                .unwrap_or_else(|| Arc::from(name));
            let mut source = SourceSpan::with_file(
                file,
                self.cursor.line,
                self.cursor.columns.as_ref().unwrap()[loc],
            );
            source.byte_column =
                std::num::NonZeroUsize::new(self.cursor.byte_columns.as_ref().unwrap()[loc]);
            source.end_byte_column = self
                .cursor
                .byte_columns
                .as_ref()
                .unwrap()
                .get(loc + 1)
                .copied()
                .and_then(std::num::NonZeroUsize::new);
            let source = Arc::new(source);
            self.cursor.source = Some(
                if self.recovery.masks.iter().any(|m| {
                    m.file == source.file
                        && m.line == source.line
                        && m.byte_column <= source.end_byte_column
                        && m.end_byte_column >= source.byte_column
                }) {
                    SourceSpan::without_range(&source)
                } else {
                    source
                },
            );
            let character = self.cursor.buffer.as_ref().unwrap()[loc];
            self.current.value = CommandOperand::Number(character as i64);
            self.cursor.loc += 1;
            let category = self.category_code(env, character)?;
            if self.is_ignored_char(category) {
                continue;
            }
            if category == Cat::Escape {
                self.scan_control_sequence(env)?;
                return Ok(true);
            }
            if category == Cat::ActiveChar {
                self.current.control_sequence = env.character_symbol(character, true, true);
                self.load_meaning(env);
                self.cursor.state = State::MidLine;
                return Ok(true);
            }
            if category == Cat::Spacer {
                self.cursor.state = State::SkipBlanks;
                self.current.value = CommandOperand::Number(32);
            }
            if category == Cat::CarRet {
                self.cursor.loc = (self.cursor.limit + 1) as usize;
                match self.cursor.state {
                    State::MidLine => {
                        self.current.command = MeaningCommand::Character(Cat::Spacer);
                        self.current.value = CommandOperand::Number(32);
                        return Ok(true);
                    }
                    State::SkipBlanks => continue,
                    _ => {
                        self.current.control_sequence = self.par_loc;
                        self.load_meaning(env);
                        return Ok(true);
                    }
                }
            }
            if category == Cat::Comment {
                self.cursor.loc = (self.cursor.limit + 1) as usize;
                continue;
            }
            if category == Cat::InvalidChar {
                return Err(Error::value("Text line contains an invalid character"));
            }
            if matches!(category, Cat::LeftBrace | Cat::RightBrace)
                || (matches!(self.cursor.state, State::SkipBlanks | State::NewLine)
                    && !matches!(category, Cat::Spacer | Cat::Comment))
            {
                self.cursor.state = State::MidLine;
            }
            self.current.command = MeaningCommand::Character(category);
            return Ok(true);
        }
    }
    pub fn is_ignored_char(&self, category: Cat) -> bool {
        category == Cat::Ignore
            || (matches!(self.cursor.state, State::SkipBlanks | State::NewLine)
                && category == Cat::Spacer)
    }
    pub fn scan_control_sequence(&mut self, env: &mut Environment) -> Result<()> {
        if self.cursor.loc as isize > self.cursor.limit {
            self.current.control_sequence = C::NULL_CS;
            self.load_meaning(env);
            return Ok(());
        }
        let mut k = self.cursor.loc;
        self.reduce_expanded_character(env, k)?;
        let character = self.cursor.buffer.as_ref().unwrap()[k];
        self.current.value = CommandOperand::Number(character as i64);
        let category = self.category_code(env, character)?;
        self.cursor.state = if matches!(category, Cat::Letter | Cat::Spacer) {
            State::SkipBlanks
        } else {
            State::MidLine
        };
        if category == Cat::Letter && (k as isize) < self.cursor.limit {
            loop {
                k += 1;
                if k as isize > self.cursor.limit {
                    break;
                }
                self.reduce_expanded_character(env, k)?;
                let character = self.cursor.buffer.as_ref().unwrap()[k];
                self.current.value = CommandOperand::Number(character as i64);
                if self.category_code(env, character)? != Cat::Letter {
                    break;
                }
            }
            self.current.control_sequence = if k > self.cursor.loc + 1 {
                let name: String = self.cursor.buffer.as_ref().unwrap()[self.cursor.loc..k]
                    .iter()
                    .map(|&code| char::from_u32(code).unwrap())
                    .collect();
                env.intern(&name, true)
            } else {
                env.character_symbol(
                    self.cursor.buffer.as_ref().unwrap()[self.cursor.loc],
                    false,
                    true,
                )
            };
            self.cursor.loc = k;
        } else {
            self.current.control_sequence = env.character_symbol(
                self.cursor.buffer.as_ref().unwrap()[self.cursor.loc],
                false,
                true,
            );
            self.cursor.loc += 1;
        }
        self.load_meaning(env);
        // The scanner may have consumed a control word, including ^^ decoding.
        // Its end is the start of the next physical character, not the next
        // logical column. Synthetic endline characters have no physical range.
        if let Some(source) = &mut self.cursor.source {
            Arc::make_mut(source).end_byte_column = self
                .cursor
                .byte_columns
                .as_ref()
                .and_then(|columns| columns.get(self.cursor.loc))
                .copied()
                .and_then(std::num::NonZeroUsize::new);
        }
        Ok(())
    }
    pub fn reduce_expanded_character(&mut self, env: &Environment, position: usize) -> Result<()> {
        let hexadecimal = |code: u32| match code {
            48..=57 => Some(code - 48),
            97..=102 => Some(code - 87),
            _ => None,
        };
        while (position + 2) as isize <= self.cursor.limit {
            let buffer = self.cursor.buffer.as_ref().unwrap();
            let marker = buffer[position];
            if self.category_code(env, marker)? != Cat::SupMark || buffer[position + 1] != marker {
                return Ok(());
            }
            let mut extended = None;
            if self.profile == LanguageProfile::Unicode {
                for count in [6, 4] {
                    let end = position + count * 2;
                    if (end - 1) as isize <= self.cursor.limit
                        && buffer[position..position + count]
                            .iter()
                            .all(|&code| code == marker)
                    {
                        if let Some(digits) = buffer[position + count..end]
                            .iter()
                            .map(|&code| hexadecimal(code))
                            .collect::<Option<Vec<_>>>()
                        {
                            let value = digits
                                .into_iter()
                                .fold(0_u32, |number, digit| number * 16 + digit);
                            extended = Some((self.profile.validate(value as i64)?, count * 2));
                            break;
                        }
                    }
                }
            }
            let (value, consumed) = if let Some(extended) = extended {
                extended
            } else {
                let value = buffer[position + 2];
                if value >= 128 {
                    return Ok(());
                }
                if (position + 3) as isize <= self.cursor.limit {
                    if let (Some(high), Some(low)) =
                        (hexadecimal(value), hexadecimal(buffer[position + 3]))
                    {
                        (high * 16 + low, 4)
                    } else {
                        (if value < 64 { value + 64 } else { value - 64 }, 3)
                    }
                } else {
                    (if value < 64 { value + 64 } else { value - 64 }, 3)
                }
            };
            let buffer = Arc::make_mut(self.cursor.buffer.as_mut().unwrap());
            buffer[position] = value;
            buffer.drain(position + 1..position + consumed);
            Arc::make_mut(self.cursor.columns.as_mut().unwrap())
                .drain(position + 1..position + consumed);
            Arc::make_mut(self.cursor.byte_columns.as_mut().unwrap())
                .drain(position + 1..position + consumed);
            self.cursor.limit -= (consumed - 1) as isize;
            self.cursor.last = self.cursor.last.saturating_sub(consumed - 1);
        }
        Ok(())
    }
}
