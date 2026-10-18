//! Unicode characters alongside compact octet tokens (`character_tokens.py`).
use crate::core::characters::is_scalar;
use crate::core::constants::Constants;
use crate::core::enums::{CharacterCategory, TokenProtection};
use crate::core::errors::{Error, Result};
use serde::{Deserialize, Serialize};
pub const CS_TOKEN_FLAG: i64 = 0x1000000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CharacterToken {
    pub category: CharacterCategory,
    pub code: u32,
}
impl CharacterToken {
    pub fn new(category: CharacterCategory, code: i64) -> Result<Self> {
        if !is_scalar(code) {
            return Err(Error::value(format!("Invalid Unicode scalar: {code}")));
        }
        Ok(Self {
            category,
            code: code as u32,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Token {
    Packed(i64),
    Character(CharacterToken),
    Protection(TokenProtection),
}
pub fn character_token(category: CharacterCategory, code: i64) -> Result<Token> {
    if code <= 255 {
        Ok(Token::Packed(category as i64 * 256 + code))
    } else {
        Ok(Token::Character(CharacterToken::new(category, code)?))
    }
}
pub fn is_control_sequence(token: Token) -> bool {
    matches!(token, Token::Packed(code) if code >= CS_TOKEN_FLAG)
}
pub fn token_category(token: Token) -> Option<CharacterCategory> {
    match token {
        Token::Packed(code) if code < CS_TOKEN_FLAG => {
            CharacterCategory::from_code(code.div_euclid(256))
        }
        Token::Character(character) => Some(character.category),
        _ => None,
    }
}
pub fn token_character(token: Token) -> Option<u32> {
    match token {
        Token::Packed(code) => Some(code.rem_euclid(256) as u32),
        Token::Character(character) => Some(character.code),
        Token::Protection(_) => None,
    }
}
impl Token {
    pub fn control_sequence(symbol: i64) -> Self {
        Self::Packed(symbol + CS_TOKEN_FLAG)
    }
    pub fn control_sequence_id(self) -> Option<i64> {
        match self {
            Self::Packed(code) if code >= CS_TOKEN_FLAG => Some(code - CS_TOKEN_FLAG),
            _ => None,
        }
    }
    pub fn is_left_brace(self) -> bool {
        token_category(self) == Some(CharacterCategory::LeftBrace)
    }
    pub fn is_right_brace(self) -> bool {
        token_category(self) == Some(CharacterCategory::RightBrace)
    }
    pub fn space() -> Self {
        Self::Packed(CharacterCategory::Spacer as i64 * 256 + 32)
    }
    pub fn left_brace() -> Self {
        Self::Packed(Constants::LEFT_BRACE_TOKEN)
    }
    pub fn right_brace() -> Self {
        Self::Packed(Constants::RIGHT_BRACE_TOKEN)
    }
}
