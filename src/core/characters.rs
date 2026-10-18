//! Byte and Unicode source-character policies (`core/characters.py`).
use super::enums::CharacterCategory;
use super::errors::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LanguageProfile {
    #[default]
    Pdftex,
    Unicode,
}
impl LanguageProfile {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "pdftex" => Ok(Self::Pdftex),
            "unicode" => Ok(Self::Unicode),
            _ => Err(Error::value(format!(
                "{name:?} is not a valid LanguageProfile"
            ))),
        }
    }
    pub fn encoding(self) -> &'static str {
        match self {
            Self::Pdftex => "latin1",
            Self::Unicode => "utf-8",
        }
    }
    pub fn accepts(self, code: i64) -> bool {
        match self {
            Self::Pdftex => (0..=255).contains(&code),
            Self::Unicode => is_scalar(code),
        }
    }
    pub fn validate(self, code: i64) -> Result<u32> {
        if !self.accepts(code) {
            let expected = if self == Self::Pdftex {
                "0–255"
            } else {
                "a Unicode scalar"
            };
            return Err(Error::value(format!(
                "Bad character code (expected {expected}): {code}"
            )));
        }
        Ok(code as u32)
    }
}
pub fn is_scalar(code: i64) -> bool {
    (0..=0x10ffff).contains(&code) && !(0xd800..=0xdfff).contains(&code)
}

// Unicode classification uses a generated snapshot of the Python reference's
// Unicode database; Rust's bundled database may be a different version.
include!("unicode_tables.rs");

pub fn unicode_category(code: u32) -> CharacterCategory {
    if UNICODE_LETTERS_MARKS
        .binary_search_by(|&(start, end)| {
            if code < start {
                std::cmp::Ordering::Greater
            } else if code > end {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
    {
        CharacterCategory::Letter
    } else {
        CharacterCategory::OtherChar
    }
}
pub fn unicode_case(code: u32, upper: bool) -> u32 {
    let table = if upper { UNICODE_UPPER } else { UNICODE_LOWER };
    table
        .binary_search_by_key(&code, |&(key, _)| key)
        .ok()
        .map_or_else(
            || {
                if unicode_category(code) == CharacterCategory::Letter {
                    code
                } else {
                    0
                }
            },
            |index| table[index].1,
        )
}
