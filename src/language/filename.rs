//! Owned form of input.py's expanded bare, quoted and braced filename scanner.
use super::input::InputProcessor;
use crate::core::{
    characters::LanguageProfile,
    enums::{CharacterCategory as Cat, Command},
    errors::{Error, ErrorKind, Result},
};
use std::ffi::OsString;

pub enum FilenameAction {
    ExpandedToken,
    BackInput,
}
pub enum FilenameStep {
    Yield(FilenameAction),
    Complete(OsString),
}
#[derive(Default)]
enum Phase {
    #[default]
    Start,
    First,
    BracedFirst,
    Next,
    Back,
    Done,
}
#[derive(Default)]
pub struct FilenameExecution {
    phase: Phase,
    previous: Option<bool>,
    braced: bool,
    quoted: bool,
    name: String,
}
impl FilenameExecution {
    pub fn resume(
        &mut self,
        input: &mut InputProcessor,
        returned: Option<Result<()>>,
    ) -> Result<FilenameStep> {
        let result = self.step(input, returned);
        if result.is_err() || matches!(result, Ok(FilenameStep::Complete(_))) {
            if let Some(previous) = self.previous.take() {
                input.scanning_filename = previous;
            }
            self.phase = Phase::Done;
        }
        result
    }
    fn step(
        &mut self,
        input: &mut InputProcessor,
        returned: Option<Result<()>>,
    ) -> Result<FilenameStep> {
        match self.phase {
            Phase::Start => {
                if returned.is_some() {
                    return Err(Error::runtime("Unrequested filename service result"));
                }
                self.previous = Some(input.scanning_filename);
                input.scanning_filename = true;
                self.phase = Phase::First;
                Ok(FilenameStep::Yield(FilenameAction::ExpandedToken))
            }
            Phase::First => {
                returned.ok_or_else(|| Error::runtime("Missing filename token result"))??;
                let current = &input.current;
                if current.is_category(Cat::Spacer)
                    || current.is_command(Command::Relax) && current.character() == Some(0)
                {
                    return Ok(FilenameStep::Yield(FilenameAction::ExpandedToken));
                }
                self.braced = current.is_category(Cat::LeftBrace);
                if self.braced {
                    self.phase = Phase::BracedFirst;
                    Ok(FilenameStep::Yield(FilenameAction::ExpandedToken))
                } else {
                    self.current(input)
                }
            }
            Phase::BracedFirst => {
                // Preserve the reference's raw EOF at the first braced token.
                returned.ok_or_else(|| Error::runtime("Missing filename token result"))??;
                self.current(input)
            }
            Phase::Next => {
                match returned.ok_or_else(|| Error::runtime("Missing filename token result"))? {
                    Ok(()) => self.current(input),
                    Err(error) if error.kind == ErrorKind::Eof => {
                        if self.braced || self.quoted {
                            Err(Error::value("Unfinished input filename"))
                        } else {
                            self.finish(input)
                        }
                    }
                    Err(error) => Err(error),
                }
            }
            Phase::Back => {
                returned.ok_or_else(|| Error::runtime("Missing filename back-input result"))??;
                self.finish(input)
            }
            Phase::Done => Err(Error::runtime("Filename result already consumed")),
        }
    }
    fn current(&mut self, input: &InputProcessor) -> Result<FilenameStep> {
        let current = &input.current;
        if self.braced && current.literal_category(Cat::RightBrace) {
            return self.finish(input);
        }
        if !self.braced && current.is_category(Cat::OtherChar) && current.character() == Some(34) {
            self.quoted = !self.quoted;
        } else if current.is_category(Cat::Spacer) {
            if !self.braced && !self.quoted {
                return self.finish(input);
            }
            self.name.push(' ');
        } else if current
            .category()
            .is_none_or(|category| category < Cat::LeftBrace || category > Cat::OtherChar)
            || current
                .character()
                .is_none_or(|code| !input.profile.accepts(code))
        {
            if self.braced || self.quoted {
                return Err(Error::value("Invalid token in input filename"));
            }
            self.phase = Phase::Back;
            return Ok(FilenameStep::Yield(FilenameAction::BackInput));
        } else {
            self.name
                .push(char::from_u32(current.character().unwrap() as u32).unwrap());
        }
        self.phase = Phase::Next;
        Ok(FilenameStep::Yield(FilenameAction::ExpandedToken))
    }
    fn finish(&mut self, input: &InputProcessor) -> Result<FilenameStep> {
        if self.name.is_empty() {
            return Err(Error::value("Missing filename for input"));
        }
        let name = if input.profile == LanguageProfile::Unicode {
            OsString::from(std::mem::take(&mut self.name))
        } else {
            crate::resources::search::filesystem_name(&self.name)?
        };
        Ok(FilenameStep::Complete(name))
    }
}
