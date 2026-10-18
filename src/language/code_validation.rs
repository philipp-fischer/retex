//! Validation at character-code language boundaries (`code_validation.py`).
use crate::core::constants::Constants as C;
use crate::core::enums::CharacterCategory;
use crate::core::errors::{Error, Result};
pub const MATH_CLASS_BITS: u32 = 3;
pub const MATH_FAMILY_BITS: u32 = 4;
pub const MATH_CHARACTER_BITS: u32 = 8;
pub const ACTIVE_MATH_CODE: i64 = 1 << (MATH_CLASS_BITS + MATH_FAMILY_BITS + MATH_CHARACTER_BITS);
pub const DELIMITER_BITS: u32 = 2 * (MATH_FAMILY_BITS + MATH_CHARACTER_BITS);
pub const MAX_DELIMITER_CODE: i64 = (1 << DELIMITER_BITS) - 1;
pub const MAX_DELIMITER_OPERAND: i64 = (1 << (DELIMITER_BITS + MATH_CLASS_BITS)) - 1;
pub const MATH_FAMILY_COUNT: usize = 1 << MATH_FAMILY_BITS;
pub const CODE_NAMES: [&str; 6] = [
    "catcode", "lccode", "uccode", "sfcode", "mathcode", "delcode",
];
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum CodeTable {
    Catcode,
    Lccode,
    Uccode,
    Sfcode,
    Mathcode,
    Delcode,
}
impl CodeTable {
    pub fn name(self) -> &'static str {
        match self {
            Self::Catcode => "catcode",
            Self::Lccode => "lccode",
            Self::Uccode => "uccode",
            Self::Sfcode => "sfcode",
            Self::Mathcode => "mathcode",
            Self::Delcode => "delcode",
        }
    }
    pub fn validate(self, value: i64) -> Result<()> {
        match self {
            Self::Catcode => validate_catcode(value),
            Self::Lccode => validate_lccode(value),
            Self::Uccode => validate_uccode(value),
            Self::Sfcode => validate_sfcode(value),
            Self::Mathcode => validate_mathcode(value),
            Self::Delcode => validate_delcode(value),
        }
    }
}
pub fn validate_catcode(value: i64) -> Result<()> {
    if !(0..=15).contains(&value) {
        Err(Error::value(format!(
            "catcode must be between {} and {}: {value}",
            CharacterCategory::Escape,
            CharacterCategory::InvalidChar
        )))
    } else {
        Ok(())
    }
}
pub fn validate_lccode(value: i64) -> Result<()> {
    validate_byte(value, "lccode")
}
pub fn validate_uccode(value: i64) -> Result<()> {
    validate_byte(value, "uccode")
}
fn validate_byte(value: i64, name: &str) -> Result<()> {
    if !(0..=C::MAX_BYTE).contains(&value) {
        Err(Error::value(format!(
            "{name} must be a byte character code: {value}"
        )))
    } else {
        Ok(())
    }
}
pub fn validate_sfcode(value: i64) -> Result<()> {
    if value < 0 {
        Err(Error::value(format!("sfcode must be nonnegative: {value}")))
    } else {
        Ok(())
    }
}
pub fn validate_mathcode(value: i64) -> Result<()> {
    if !(0..=ACTIVE_MATH_CODE).contains(&value) {
        Err(Error::value(format!(
            "mathcode must be a packed math code or active marker: {value}"
        )))
    } else {
        Ok(())
    }
}
pub fn validate_delcode(value: i64) -> Result<()> {
    if value > MAX_DELIMITER_CODE {
        Err(Error::value(format!(
            "delcode must fit its two family/character pairs: {value}"
        )))
    } else {
        Ok(())
    }
}
pub fn validate_spacefactor(value: i64) -> Result<()> {
    if value <= 0 {
        Err(Error::value(format!(
            "spacefactor must be positive: {value}"
        )))
    } else {
        Ok(())
    }
}
