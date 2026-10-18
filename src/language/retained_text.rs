//! Owned expansion of retained tokens in TeX's disabled list mode.
use super::{character_tokens::Token, services::ListServices};
use crate::core::{
    enums::ListMode,
    errors::{Error, Result},
};
use std::sync::Arc;

pub enum RetainedStep {
    Expand,
    Complete(Arc<[Token]>),
}
pub struct RetainedText {
    tokens: Arc<[Token]>,
    phase: u8,
    previous_mode: Option<ListMode>,
}
impl RetainedText {
    pub fn new(tokens: Arc<[Token]>) -> Self {
        Self {
            tokens,
            phase: 0,
            previous_mode: None,
        }
    }
    pub fn resume(
        &mut self,
        services: &mut impl ListServices,
        returned: Option<Result<Arc<[Token]>>>,
    ) -> Result<RetainedStep> {
        match self.phase {
            0 => {
                if returned.is_some() {
                    return Err(Error::runtime("Unrequested retained text result"));
                }
                let mut wrapped = Vec::with_capacity(self.tokens.len() + 2);
                wrapped.push(Token::left_brace());
                wrapped.extend_from_slice(&self.tokens);
                wrapped.push(Token::right_brace());
                services
                    .input_mut()
                    .begin_tokens(wrapped.into(), None, None)?;
                self.previous_mode = Some(services.builder().context.mode);
                services.builder_mut().context.mode = ListMode::Disabled;
                self.phase = 1;
                Ok(RetainedStep::Expand)
            }
            1 => {
                if let Some(mode) = self.previous_mode.take() {
                    services.builder_mut().context.mode = mode;
                }
                self.phase = 2;
                returned
                    .ok_or_else(|| Error::runtime("Missing retained text result"))?
                    .map(RetainedStep::Complete)
            }
            _ => Err(Error::runtime("Retained text result already consumed")),
        }
    }
}
