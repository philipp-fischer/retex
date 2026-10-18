//! Shared owned stream syntax and retained write expansion (`streams.py`).
use super::{
    character_tokens::Token,
    input_frames::open_physical,
    scanning::{ScanRequest, ScanValue},
    streams::{
        ReadState, ReadStream, StreamEvent, StreamServices, Streams, display_tokens,
        write_destination,
    },
};
use crate::typesetting::model::NodeFields;
use crate::{
    core::{
        constants::Constants as C,
        enums::{CharacterCategory as Cat, NodeKind},
        errors::{Error, Result},
    },
    typesetting::model::{IoEffect, Node, NodePayload},
};
use std::{path::Path, sync::Arc};

pub enum StreamRequest {
    Input { opening: bool },
    Read { raw: bool, global: bool },
    Output { operation: u8, immediate: bool },
    Perform(IoEffect),
}
pub enum StreamAction {
    Scan(ScanRequest),
    RawToken,
    GeneralText { expanded: bool },
    RetainedText(Arc<[Token]>),
}
pub enum StreamValue {
    Unit,
    Scan(ScanValue),
    Tokens(Arc<[Token]>),
}
pub enum StreamStep {
    Yield(StreamAction),
    Complete,
}
enum Phase {
    Start,
    InputNumber,
    InputEquals,
    InputFilename,
    ReadNumber,
    ReadTo,
    Destination,
    OutputNumber,
    OutputEquals,
    OutputFilename,
    OutputTokens,
    WriteExpanded,
    Done,
}
pub struct StreamExecution {
    request: StreamRequest,
    phase: Phase,
    number: i64,
    destination: Option<crate::typesetting::model::StreamDestination>,
}
fn scan(value: Option<StreamValue>) -> Result<ScanValue> {
    if let Some(StreamValue::Scan(value)) = value {
        Ok(value)
    } else {
        Err(Error::runtime("Invalid stream scanner result"))
    }
}
impl StreamExecution {
    pub fn new(request: StreamRequest) -> Self {
        Self {
            request,
            phase: Phase::Start,
            number: 0,
            destination: None,
        }
    }
    fn scan(&mut self, phase: Phase, request: ScanRequest) -> StreamStep {
        self.phase = phase;
        StreamStep::Yield(StreamAction::Scan(request))
    }
    fn done(&mut self) -> StreamStep {
        self.phase = Phase::Done;
        StreamStep::Complete
    }
    fn perform(
        &mut self,
        streams: &mut Streams,
        services: &mut impl StreamServices,
        payload: IoEffect,
    ) -> Result<StreamStep> {
        if let IoEffect::Write {
            destination,
            tokens,
        } = payload
        {
            self.destination = Some(destination);
            self.phase = Phase::WriteExpanded;
            Ok(StreamStep::Yield(StreamAction::RetainedText(tokens)))
        } else {
            streams.perform_static(services, &payload)?;
            Ok(self.done())
        }
    }
    fn output(
        &mut self,
        streams: &mut Streams,
        services: &mut impl StreamServices,
        payload: IoEffect,
    ) -> Result<StreamStep> {
        if matches!(
            self.request,
            StreamRequest::Output {
                immediate: true,
                ..
            }
        ) {
            self.perform(streams, services, payload)
        } else {
            let origins = vec![services.input().source()];
            let pointer = services.builder_mut().allocate(Node::from(NodeFields {
                node_type: NodeKind::Whatsit,
                whatsit: "io".into(),
                payload: NodePayload::Io(payload),
                origins,
                ..NodeFields::default()
            }))?;
            services.builder_mut().tail_append(pointer)?;
            Ok(self.done())
        }
    }
    pub fn resume(
        &mut self,
        streams: &mut Streams,
        services: &mut impl StreamServices,
        returned: Option<Result<StreamValue>>,
    ) -> Result<StreamStep> {
        let value = returned.transpose()?;
        Ok(match self.phase {
            Phase::Start => {
                if value.is_some() {
                    return Err(Error::runtime("Unrequested stream result"));
                }
                match &self.request {
                    StreamRequest::Input { .. } => self.scan(
                        Phase::InputNumber,
                        ScanRequest::Register(Some(C::MAX_FILE_STREAM)),
                    ),
                    StreamRequest::Read { .. } => self.scan(Phase::ReadNumber, ScanRequest::Int),
                    StreamRequest::Output { operation, .. } => self.scan(
                        Phase::OutputNumber,
                        if *operation == 0 {
                            ScanRequest::Register(Some(C::MAX_FILE_STREAM))
                        } else {
                            ScanRequest::Int
                        },
                    ),
                    StreamRequest::Perform(payload) => {
                        let payload = payload.clone();
                        return self.perform(streams, services, payload);
                    }
                }
            }
            Phase::InputNumber => {
                self.number = scan(value)?.integer()?;
                streams.close_input(self.number);
                if matches!(self.request, StreamRequest::Input { opening: true }) {
                    self.scan(Phase::InputEquals, ScanRequest::OptionalEquals)
                } else {
                    streams.events.push(StreamEvent::ReadStream {
                        stream: self.number,
                        state: streams.input_state(self.number),
                    });
                    self.done()
                }
            }
            Phase::InputEquals => {
                scan(value)?;
                self.scan(Phase::InputFilename, ScanRequest::Filename)
            }
            Phase::InputFilename => {
                let ScanValue::Filename(name) = scan(value)? else {
                    return Err(Error::runtime("Invalid stream filename"));
                };
                let directory = services
                    .input()
                    .cursor
                    .name
                    .as_ref()
                    .and_then(|name| Path::new(name).parent())
                    .map(Path::to_path_buf);
                let path = services.input_mut().resources.resolve(
                    name.to_str()
                        .ok_or_else(|| Error::value("Non-UTF-8 input filename"))?,
                    directory.as_deref(),
                    ".tex",
                    false,
                    false,
                )?;
                if let Some(path) = path {
                    streams.readers.insert(
                        self.number,
                        ReadStream {
                            handle: open_physical(&path)?,
                            state: ReadState::JustOpen,
                        },
                    );
                }
                streams.events.push(StreamEvent::ReadStream {
                    stream: self.number,
                    state: streams.input_state(self.number),
                });
                self.done()
            }
            Phase::ReadNumber => {
                self.number = scan(value)?.integer()?;
                self.scan(Phase::ReadTo, ScanRequest::Keyword("to".into()))
            }
            Phase::ReadTo => {
                if !scan(value)?.flag()? {
                    return Err(Error::value("Missing to in read command"));
                }
                self.phase = Phase::Destination;
                StreamStep::Yield(StreamAction::RawToken)
            }
            Phase::Destination => {
                if !matches!(value, Some(StreamValue::Unit)) {
                    return Err(Error::runtime("Invalid read destination result"));
                }
                if services.input().current.is_category(Cat::Spacer) {
                    return Ok(StreamStep::Yield(StreamAction::RawToken));
                }
                let destination = services.input().current.control_sequence;
                if destination == 0 {
                    return Err(Error::value("Expected control sequence after read to"));
                }
                let StreamRequest::Read { raw, global } = self.request else {
                    unreachable!()
                };
                streams.read_content(services, self.number, destination, raw, global)?;
                self.done()
            }
            Phase::OutputNumber => {
                self.number = scan(value)?.integer()?;
                let StreamRequest::Output { operation, .. } = self.request else {
                    unreachable!()
                };
                match operation {
                    0 => self.scan(Phase::OutputEquals, ScanRequest::OptionalEquals),
                    1 => {
                        self.phase = Phase::OutputTokens;
                        StreamStep::Yield(StreamAction::GeneralText { expanded: false })
                    }
                    2 => {
                        return self.output(
                            streams,
                            services,
                            IoEffect::Close(write_destination(self.number)),
                        );
                    }
                    _ => return Err(Error::value("Unknown output stream operation")),
                }
            }
            Phase::OutputEquals => {
                scan(value)?;
                self.scan(Phase::OutputFilename, ScanRequest::Filename)
            }
            Phase::OutputFilename => {
                let ScanValue::Filename(mut filename) = scan(value)? else {
                    return Err(Error::runtime("Invalid output filename"));
                };
                let last = Path::new(&filename).file_name().unwrap_or_default();
                if !super::input_frames::filesystem_bytes(last).contains(&b'.') {
                    filename.push(".tex");
                }
                return self.output(
                    streams,
                    services,
                    IoEffect::Open {
                        stream: self.number as u8,
                        filename,
                    },
                );
            }
            Phase::OutputTokens => {
                let Some(StreamValue::Tokens(tokens)) = value else {
                    return Err(Error::runtime("Invalid output tokens"));
                };
                return self.output(
                    streams,
                    services,
                    IoEffect::Write {
                        destination: write_destination(self.number),
                        tokens,
                    },
                );
            }
            Phase::WriteExpanded => {
                let Some(StreamValue::Tokens(tokens)) = value else {
                    return Err(Error::runtime("Invalid expanded write result"));
                };
                let text = display_tokens(services, &tokens)?;
                streams.write_text(
                    services,
                    self.destination.take().expect("write destination"),
                    text,
                )?;
                self.done()
            }
            Phase::Done => return Err(Error::runtime("Stream result already consumed")),
        })
    }
}
