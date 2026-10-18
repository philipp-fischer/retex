//! Owned read/write streams and delayed I/O effects (`streams.py`).
use super::character_tokens::{Token, character_token};
use super::conversions::printed_bytes;
use super::expansion::Expander;
use super::input::{InputProcessor, ScannerStatus};
use super::input_frames::{PhysicalStream, SourceStream};
use super::services::ListServices;
use super::stream_execution::{
    StreamAction, StreamExecution, StreamRequest, StreamStep, StreamValue,
};
use super::tokens::MacroDefinition;
use crate::core::capacities::{reached, read_size};
use crate::core::characters::LanguageProfile;
use crate::core::constants::Constants as C;
use crate::core::enums::{CharacterCategory as Cat, InputStateKind, WriteDestination};
use crate::core::errors::{Error, ErrorKind, Result};
use crate::typesetting::model::{IoEffect, StreamDestination};
use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::sync::Arc;

pub fn write_destination(number: i64) -> StreamDestination {
    if number < 0 {
        StreamDestination::Named(WriteDestination::Log)
    } else if number <= C::MAX_FILE_STREAM {
        StreamDestination::File(number as u8)
    } else if number == C::SHELL_WRITE_STREAM {
        StreamDestination::Named(WriteDestination::Shell)
    } else {
        StreamDestination::Named(WriteDestination::Terminal)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadState {
    Closed,
    JustOpen,
    Normal,
}
impl ReadState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::JustOpen => "just_open",
            Self::Normal => "normal",
        }
    }
}
pub struct ReadStream {
    pub handle: Box<dyn PhysicalStream>,
    pub state: ReadState,
}
#[derive(Clone, Debug)]
pub enum StreamEvent {
    ReadStream {
        stream: i64,
        state: ReadState,
    },
    Read {
        stream: i64,
        tokens: usize,
        raw: bool,
    },
    Write {
        destination: StreamDestination,
        text: String,
    },
}
pub trait StreamServices: ListServices {
    fn interaction(&self) -> u8;
    fn eight_bit_output(&self) -> bool;
    fn terminal_write(&mut self, text: &str) -> Result<()>;
    fn observe_terminal_write(&mut self, _text: &str) -> Result<()> {
        Ok(())
    }
}
#[derive(Default)]
pub struct Streams {
    pub readers: BTreeMap<i64, ReadStream>,
    pub writers: BTreeMap<u8, crate::resources::host::Writer>,
    pub events: Vec<StreamEvent>,
}
pub fn expand_retained(services: &mut impl ListServices, tokens: &[Token]) -> Result<Arc<[Token]>> {
    let mut execution = super::retained_text::RetainedText::new(tokens.to_vec().into());
    let mut returned = None;
    loop {
        match execution.resume(services, returned.take())? {
            super::retained_text::RetainedStep::Complete(tokens) => return Ok(tokens),
            super::retained_text::RetainedStep::Expand => {
                returned = Some(services.general_text(true))
            }
        }
    }
}

pub fn display_tokens(services: &mut impl ListServices, tokens: &[Token]) -> Result<String> {
    let mut text = String::new();
    for &token in tokens {
        text.push_str(&services.display_token(token)?)
    }
    Ok(text)
}
impl Streams {
    pub(crate) fn checkpoint_clone(&self) -> Result<Option<Self>> {
        let mut readers = BTreeMap::new();
        for (&number, stream) in &self.readers {
            let Some(handle) = stream.handle.fork() else {
                return Ok(None);
            };
            readers.insert(
                number,
                ReadStream {
                    handle,
                    state: stream.state,
                },
            );
        }
        let mut writers = BTreeMap::new();
        for (&number, stream) in &self.writers {
            let Some(handle) = stream.fork()? else {
                return Ok(None);
            };
            writers.insert(number, handle);
        }
        Ok(Some(Self {
            readers,
            writers,
            events: self.events.clone(),
        }))
    }

    pub fn close(&mut self) -> Result<()> {
        for (_, mut record) in std::mem::take(&mut self.readers) {
            record.handle.close()
        }
        let mut error = None;
        for (_, mut writer) in std::mem::take(&mut self.writers) {
            if let Err(failure) = writer.flush() {
                error = Some(Error::from(failure))
            }
        }
        if let Some(error) = error {
            Err(error)
        } else {
            Ok(())
        }
    }
    pub fn close_input(&mut self, number: i64) {
        if let Some(mut record) = self.readers.remove(&number) {
            record.handle.close()
        }
    }
    pub fn input_state(&self, number: i64) -> ReadState {
        self.readers
            .get(&number)
            .map_or(ReadState::Closed, |record| record.state)
    }
    pub fn at_eof(&self, number: i64) -> bool {
        !self.readers.contains_key(&number)
    }
    pub fn open_input(&mut self, services: &mut impl StreamServices, opening: bool) -> Result<()> {
        self.drive(services, StreamRequest::Input { opening })
    }
    pub fn read(
        &mut self,
        services: &mut impl StreamServices,
        raw: bool,
        global: bool,
    ) -> Result<()> {
        self.drive(services, StreamRequest::Read { raw, global })
    }
    pub(crate) fn read_content(
        &mut self,
        services: &mut impl StreamServices,
        number: i64,
        destination: i64,
        raw: bool,
        global: bool,
    ) -> Result<()> {
        if !(0..=C::MAX_FILE_STREAM).contains(&number) || !self.readers.contains_key(&number) {
            return Err(Error::value(
                "Terminal input is not supported; supply the requested file",
            ));
        }
        let input = services.input();
        if reached(input.capacities.max_in_open, input.in_open) {
            return Err(Error::value("Too many open files")
                .with_issue(crate::core::recovery::Issue::Capacity));
        }
        let mut scanner = InputProcessor::new(
            input.profile,
            input.ini,
            input.capacities,
            input.resources.clone(),
        );
        scanner.initialize_cat_codes();
        scanner.cat_code = input.cat_code.clone();
        scanner.in_open = 1;
        scanner.cursor.name = input.cursor.name.clone();
        scanner.par_loc = input.par_loc;
        scanner.par_token = input.par_token;
        scanner.read_line_only = true;
        scanner.cursor.buffer_base =
            input.cursor.buffer_base + (input.cursor.limit + 1).max(0) as usize;
        read_size(scanner.capacities.buf_size, scanner.cursor.buffer_base)?;
        let mut tokens = Vec::new();
        let mut balance = 0;
        loop {
            let limit = read_size(scanner.capacities.buf_size, scanner.cursor.buffer_base)?;
            let line = self
                .readers
                .get_mut(&number)
                .map(|record| {
                    record.handle.observe_explicit_read()?;
                    record.handle.readline(limit, scanner.profile)
                })
                .transpose()?
                .flatten();
            let line = if let Some(line) = line {
                self.readers.get_mut(&number).unwrap().state = ReadState::Normal;
                line
            } else {
                self.close_input(number);
                if balance != 0 {
                    return Err(Error::value("File ended within read: unbalanced braces"));
                }
                "\n".into()
            };
            // input_ln receives host text, already decoded by the owned stream.
            let bytes = if scanner.profile == LanguageProfile::Unicode {
                line.into_bytes()
            } else {
                line.chars()
                    .map(|character| {
                        u8::try_from(character as u32)
                            .map_err(|_| Error::value("Non-octet read line"))
                    })
                    .collect::<Result<Vec<_>>>()?
            };
            scanner.input_file = vec![Some(Box::new(SourceStream::new(Cursor::new(bytes))))];
            scanner.cursor.index = 0;
            scanner.input_ln(services.env())?;
            scanner.cursor.loc = 0;
            scanner.cursor.state = InputStateKind::NewLine;
            if raw {
                let end = (scanner.cursor.limit + 1).max(0) as usize;
                for &code in &scanner.cursor.buffer.as_ref().unwrap()[..end] {
                    tokens.push(character_token(
                        if code == 32 {
                            Cat::Spacer
                        } else {
                            Cat::OtherChar
                        },
                        code as i64,
                    )?)
                }
                break;
            }
            scanner.scanner_status = ScannerStatus::Defining;
            scanner.warning_cs = destination;
            loop {
                match scanner.get_token(services.env_mut()) {
                    Ok(()) => {}
                    Err(error) if error.kind == ErrorKind::Eof => break,
                    Err(error) => return Err(error),
                }
                if scanner.current.literal_category(Cat::LeftBrace) {
                    balance += 1
                } else if scanner.current.literal_category(Cat::RightBrace) {
                    balance -= 1;
                    if balance < 0 {
                        balance = 0;
                        break;
                    }
                }
                tokens.push(scanner.current.token)
            }
            if balance == 0 {
                break;
            }
        }
        let count = tokens.len();
        let source = services.input().source();
        Expander::define_macro(
            services.env_mut(),
            destination,
            MacroDefinition {
                body: tokens.into(),
                parameters: None,
                protected: false,
                source,
                long: false,
                outer: false,
                body_sources: None,
            },
            global,
        );
        self.events.push(StreamEvent::Read {
            stream: number,
            tokens: count,
            raw,
        });
        Ok(())
    }
    pub fn output(
        &mut self,
        services: &mut impl StreamServices,
        operation: u8,
        immediate: bool,
    ) -> Result<()> {
        self.drive(
            services,
            StreamRequest::Output {
                operation,
                immediate,
            },
        )
    }
    fn drive(&mut self, services: &mut impl StreamServices, request: StreamRequest) -> Result<()> {
        let mut execution = StreamExecution::new(request);
        let mut returned = None;
        loop {
            match execution.resume(self, services, returned.take())? {
                StreamStep::Complete => return Ok(()),
                StreamStep::Yield(action) => {
                    returned = Some(match action {
                        StreamAction::RetainedText(tokens) => {
                            expand_retained(services, &tokens).map(StreamValue::Tokens)
                        }
                        StreamAction::Scan(request) => {
                            services.scan(request).map(StreamValue::Scan)
                        }
                        StreamAction::RawToken => services.raw_token().map(|_| StreamValue::Unit),
                        StreamAction::GeneralText { expanded } => {
                            services.general_text(expanded).map(StreamValue::Tokens)
                        }
                    })
                }
            }
        }
    }
    pub fn perform(
        &mut self,
        services: &mut impl StreamServices,
        payload: &IoEffect,
    ) -> Result<()> {
        self.drive(services, StreamRequest::Perform(payload.clone()))
    }
    pub(crate) fn perform_static(
        &mut self,
        services: &mut impl StreamServices,
        payload: &IoEffect,
    ) -> Result<()> {
        match payload {
            IoEffect::Open { stream, filename } => {
                if let Some(mut writer) = self.writers.remove(stream) {
                    writer.flush()?
                }
                let file = services.input().resources.search.policy.open_output(
                    filename
                        .to_str()
                        .ok_or_else(|| Error::value("Non-UTF-8 output filename"))?,
                    false,
                    true,
                )?;
                self.writers.insert(*stream, file);
            }
            IoEffect::Close(StreamDestination::File(stream)) => {
                if let Some(mut writer) = self.writers.remove(stream) {
                    writer.flush()?
                }
            }
            IoEffect::Close(_) => {}
            IoEffect::Write { .. } => {
                return Err(Error::runtime("Write expansion requires owned operands"));
            }
        }
        Ok(())
    }
    pub(crate) fn write_text(
        &mut self,
        services: &mut impl StreamServices,
        destination: StreamDestination,
        text: String,
    ) -> Result<()> {
        let terminal = destination != StreamDestination::Named(WriteDestination::Shell)
            && match destination {
                StreamDestination::File(n) => !self.writers.contains_key(&n),
                _ => true,
            };
        let text = if terminal {
            crate::diagnostics::messages::noninteractive_message(text)
        } else {
            text
        };
        let unicode = services.input().profile == LanguageProfile::Unicode;
        let text = printed_bytes(
            &text,
            services.parameter("newlinechar", -1),
            services.eight_bit_output(),
            unicode,
        );
        if terminal {
            services.observe_terminal_write(&text)?;
        }
        if let StreamDestination::File(stream) = destination {
            if let Some(writer) = self.writers.get_mut(&stream) {
                let output = format!("{text}\n");
                let bytes = if unicode {
                    output.into_bytes()
                } else {
                    output
                        .chars()
                        .map(|character| {
                            u8::try_from(character as u32)
                                .map_err(|_| Error::value("Non-octet write output"))
                        })
                        .collect::<Result<Vec<_>>>()?
                };
                writer.write_all(&bytes)?;
                writer.flush()?;
            } else if services.interaction() != 0 {
                services.terminal_write(&text)?
            }
        } else if destination != StreamDestination::Named(WriteDestination::Shell)
            && services.interaction() != 0
        {
            services.terminal_write(&text)?
        }
        self.events.push(StreamEvent::Write { destination, text });
        Ok(())
    }
}
