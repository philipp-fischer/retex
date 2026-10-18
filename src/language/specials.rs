//! Retained specials and PDF literal mode dispatch (`specials.py`).
use super::character_tokens::Token;
use super::scanning::{ScanRequest, ScanValue};
use super::services::ListServices;
use super::streams::{display_tokens, expand_retained};
use crate::core::enums::NodeKind;
use crate::core::errors::{Error, Result};
use crate::typesetting::model::NodeFields;
use crate::typesetting::model::{LiteralMode, Node, NodePayload, SpecialEffect};
use std::sync::Arc;
pub struct Specials;
impl Specials {
    pub fn scan(services: &mut impl ListServices, literal: bool) -> Result<()> {
        let mut execution = SpecialExecution::new(literal);
        let mut returned = None;
        loop {
            match execution.resume(services, returned.take())? {
                SpecialStep::Complete => return Ok(()),
                SpecialStep::Scan(request) => {
                    returned = Some(services.scan(request).map(SpecialValue::Scan))
                }
                SpecialStep::Text { expanded } => {
                    returned = Some(services.general_text(expanded).map(SpecialValue::Tokens))
                }
            }
        }
    }
    fn append(
        services: &mut impl ListServices,
        late: bool,
        mode: LiteralMode,
        tokens: Arc<[Token]>,
    ) -> Result<()> {
        let origins = vec![services.input().source()];
        let pointer = services.builder_mut().allocate(Node::from(NodeFields {
            node_type: NodeKind::Whatsit,
            whatsit: "special".into(),
            payload: NodePayload::Special(SpecialEffect { late, mode, tokens }),
            origins,
            ..NodeFields::default()
        }))?;
        services.builder_mut().tail_append(pointer)
    }
    pub fn ship(
        services: &mut impl ListServices,
        payload: &SpecialEffect,
    ) -> Result<Option<(LiteralMode, String)>> {
        let tokens = if payload.late {
            expand_retained(services, &payload.tokens)?
        } else {
            payload.tokens.clone()
        };
        let text = display_tokens(services, &tokens)?;
        services.event(
            "special",
            serde_json::json!({"late":payload.late,"mode":payload.mode.as_str(),"text":text}),
        );
        Ok(Self::ship_text(services, &text, payload.mode))
    }
    pub fn ship_text(
        services: &mut impl ListServices,
        text: &str,
        mut mode: LiteralMode,
    ) -> Option<(LiteralMode, String)> {
        let mut text = text;
        if mode == LiteralMode::Special {
            if text.starts_with("PDF:") || text.starts_with("pdf:") {
                text = &text[4..];
                mode = LiteralMode::Origin;
                if let Some(rest) = text.strip_prefix("direct:") {
                    mode = LiteralMode::Direct;
                    text = rest
                } else if let Some(rest) = text.strip_prefix("page:") {
                    mode = LiteralMode::Page;
                    text = rest
                }
            } else {
                if !text.is_empty() && !text.starts_with("SRC:") && !text.starts_with("src:") {
                    services.warning(&format!(
                        "Non-PDF special ignored!\n<special> {}",
                        text.chars().take(64).collect::<String>()
                    ));
                }
                return None;
            }
        }
        Some((mode, text.into()))
    }
}

pub enum SpecialStep {
    Scan(ScanRequest),
    Text { expanded: bool },
    Complete,
}
pub enum SpecialValue {
    Scan(ScanValue),
    Tokens(Arc<[Token]>),
}
enum Phase {
    Start,
    Late,
    Direct,
    Page,
    Tokens,
    Done,
}
pub struct SpecialExecution {
    literal: bool,
    late: bool,
    mode: LiteralMode,
    phase: Phase,
}
impl SpecialExecution {
    pub fn new(literal: bool) -> Self {
        Self {
            literal,
            late: false,
            mode: LiteralMode::Special,
            phase: Phase::Start,
        }
    }
    fn text(&mut self) -> SpecialStep {
        self.phase = Phase::Tokens;
        SpecialStep::Text {
            expanded: !self.late,
        }
    }
    pub fn resume(
        &mut self,
        services: &mut impl ListServices,
        returned: Option<Result<SpecialValue>>,
    ) -> Result<SpecialStep> {
        let value = returned.transpose()?;
        let flag = |value| {
            if let Some(SpecialValue::Scan(value)) = value {
                value.flag()
            } else {
                Err(Error::runtime("Invalid special scanner result"))
            }
        };
        Ok(match self.phase {
            Phase::Start => {
                if value.is_some() {
                    return Err(Error::runtime("Unrequested special result"));
                }
                self.phase = Phase::Late;
                SpecialStep::Scan(ScanRequest::Keyword("shipout".into()))
            }
            Phase::Late => {
                self.late = flag(value)?;
                if self.literal {
                    self.phase = Phase::Direct;
                    SpecialStep::Scan(ScanRequest::Keyword("direct".into()))
                } else {
                    self.text()
                }
            }
            Phase::Direct => {
                if flag(value)? {
                    self.mode = LiteralMode::Direct;
                    self.text()
                } else {
                    self.phase = Phase::Page;
                    SpecialStep::Scan(ScanRequest::Keyword("page".into()))
                }
            }
            Phase::Page => {
                self.mode = if flag(value)? {
                    LiteralMode::Page
                } else {
                    LiteralMode::Origin
                };
                self.text()
            }
            Phase::Tokens => {
                let Some(SpecialValue::Tokens(tokens)) = value else {
                    return Err(Error::runtime("Invalid special tokens"));
                };
                Specials::append(services, self.late, self.mode, tokens)?;
                self.phase = Phase::Done;
                SpecialStep::Complete
            }
            Phase::Done => return Err(Error::runtime("Special result already consumed")),
        })
    }
}
