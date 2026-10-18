//! Stored token input on the shared cursor (`token_input.py`).
use super::character_tokens::{Token, token_category, token_character};
use super::commands::traits;
use super::input::InputProcessor;
use super::records::{CommandOperand, MeaningCommand};
use super::state::Environment;
use crate::core::enums::{CharacterCategory as Cat, Command, InputStateKind, TokenProtection};
use crate::core::errors::{Error, Result};
use crate::core::source::SourceSpan;
use std::sync::Arc;

impl InputProcessor {
    pub fn input_from_token_list(&mut self, env: &mut Environment) -> Result<bool> {
        if self
            .cursor
            .token_list
            .as_ref()
            .is_none_or(|tokens| self.cursor.token_index >= tokens.len())
        {
            self.end_token_list();
            return Ok(false);
        }
        let token = self.cursor.token_list.as_ref().unwrap()[self.cursor.token_index];
        if let Some(sources) = &self.cursor.token_sources {
            let source = sources
                .get(self.cursor.token_index)
                .ok_or_else(|| Error::value("Token provenance length mismatch"))?;
            if let Some(source) = source {
                self.cursor.source = Some(source.clone());
            } else {
                self.cursor.source = self.cursor.source.as_ref().map(SourceSpan::without_range);
            }
        }
        self.cursor.token_index += 1;
        if token == Token::Protection(TokenProtection::Noexpand) {
            let following = self
                .cursor
                .token_list
                .as_ref()
                .unwrap()
                .get(self.cursor.token_index)
                .copied()
                .ok_or_else(|| Error::value("Malformed internal noexpand marker"))?;
            self.cursor.token_index += 1;
            self.current.control_sequence = following
                .control_sequence_id()
                .ok_or_else(|| Error::value("Noexpand marker requires a control sequence"))?;
            self.load_meaning(env);
            if self
                .current
                .operation()
                .is_some_and(|command| traits(command).expandable)
            {
                self.current.command = MeaningCommand::Operation(Command::Relax);
                self.current.value = CommandOperand::Protection(TokenProtection::Noexpand);
            }
            return Ok(true);
        }
        if let Some(symbol) = token.control_sequence_id() {
            self.current.control_sequence = symbol;
            self.load_meaning(env);
        } else {
            self.current.control_sequence = 0;
            self.current.command = MeaningCommand::Character(
                token_category(token).ok_or_else(|| Error::value("Invalid character token"))?,
            );
            self.current.value = CommandOperand::Number(
                token_character(token).ok_or_else(|| Error::value("Missing token character"))?
                    as i64,
            );
        }
        self.current.token = token;
        Ok(true)
    }
    pub fn end_token_list(&mut self) {
        if !self.input_stack.is_empty() {
            self.pop_input_state();
        }
    }
    pub fn begin_generated_tokens(
        &mut self,
        tokens: Arc<[Token]>,
        source: Option<Arc<SourceSpan>>,
    ) -> Result<()> {
        let source = source
            .or_else(|| self.source())
            .as_ref()
            .map(SourceSpan::without_range);
        self.begin_tokens(tokens, source, None)
    }
    pub fn begin_tokens(
        &mut self,
        tokens: Arc<[Token]>,
        source: Option<Arc<SourceSpan>>,
        sources: Option<Arc<[Option<Arc<SourceSpan>>]>>,
    ) -> Result<()> {
        let source = source.or_else(|| self.source());
        while self.cursor.state == InputStateKind::TokenList
            && self
                .cursor
                .token_list
                .as_ref()
                .is_none_or(|tokens| self.cursor.token_index >= tokens.len())
        {
            if self.input_stack.is_empty() {
                break;
            }
            self.end_token_list();
        }
        self.push_input_state()?;
        self.cursor.state = InputStateKind::TokenList;
        self.cursor.token_list = Some(tokens);
        self.cursor.token_index = 0;
        self.cursor.token_sources = sources;
        if source.is_some() {
            self.cursor.source = source;
        }
        Ok(())
    }
    pub fn back_input(&mut self) -> Result<()> {
        self.back_token(self.current.token, self.source())
    }
    pub fn back_token(&mut self, token: Token, source: Option<Arc<SourceSpan>>) -> Result<()> {
        if token_category(token) == Some(Cat::LeftBrace) {
            self.align_state -= 1;
        } else if token_category(token) == Some(Cat::RightBrace) {
            self.align_state += 1;
        }
        self.begin_tokens(Arc::from([token]), source, None)
    }
}
