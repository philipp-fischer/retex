//! Immutable macro definitions and argument provenance (`language/tokens.py`).
use super::character_tokens::Token;
use crate::core::constants::Constants as C;
use crate::core::errors::{Error, Result};
use crate::core::source::SourceSpan;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub fn parameter_number(token: Token) -> Option<usize> {
    match token {
        Token::Packed(value)
            if (C::OUT_PARAM_TOKEN + 1..=C::OUT_PARAM_TOKEN + 9).contains(&value) =>
        {
            Some((value - C::OUT_PARAM_TOKEN) as usize)
        }
        _ => None,
    }
}
pub fn parameter_token(number: usize) -> Result<Token> {
    if !(1..=9).contains(&number) {
        Err(Error::value(
            "Macro parameter number must be between 1 and 9",
        ))
    } else {
        Ok(Token::Packed(C::OUT_PARAM_TOKEN + number as i64))
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacroParameters {
    pub prefix: Vec<Token>,
    pub delimiters: Vec<Vec<Token>>,
    pub hash_brace: Option<Token>,
    pub match_characters: Vec<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MacroDefinition {
    pub body: Arc<[Token]>,
    pub parameters: Option<MacroParameters>,
    pub protected: bool,
    pub source: Option<Arc<SourceSpan>>,
    pub long: bool,
    pub outer: bool,
    pub body_sources: Option<Arc<[Option<Arc<SourceSpan>>]>>,
}
#[derive(Clone, Debug, Default)]
pub struct ArgumentTokens {
    pub tokens: Vec<Token>,
    pub sources: Vec<Option<Arc<SourceSpan>>>,
}
impl ArgumentTokens {
    pub fn new(tokens: Vec<Token>, sources: Vec<Option<Arc<SourceSpan>>>) -> Self {
        Self { tokens, sources }
    }
    pub fn append(&mut self, token: Token, source: Option<Arc<SourceSpan>>) {
        self.tokens.push(token);
        self.sources.push(source);
    }
    pub fn extend(&mut self, other: &Self) {
        self.tokens.extend(&other.tokens);
        self.sources.extend(other.sources.iter().cloned());
    }
    pub fn extend_plain(&mut self, tokens: &[Token]) {
        self.tokens.extend(tokens);
        self.sources.resize(self.sources.len() + tokens.len(), None);
    }
    pub fn slice(&self, range: std::ops::Range<usize>) -> Self {
        Self {
            tokens: self.tokens[range.clone()].to_vec(),
            sources: self.sources[range].to_vec(),
        }
    }
}
