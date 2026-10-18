//! Decode packed source math and delimiter codes (`math_codes.py`).
use super::math_spacing::AtomKind;
use crate::core::errors::Result;
use crate::language::code_validation::{MATH_CHARACTER_BITS, MATH_CLASS_BITS, MATH_FAMILY_BITS};
use serde::{Deserialize, Serialize};
pub const CHARACTER_RADIX: i64 = 1 << MATH_CHARACTER_BITS;
pub const GLYPH_RADIX: i64 = CHARACTER_RADIX * (1 << MATH_FAMILY_BITS);
pub const CLASS_RADIX: i64 = 1 << MATH_CLASS_BITS;
pub const VARIABLE_FAMILY_CLASS: i64 = 7;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MathCharacterCode {
    pub kind: AtomKind,
    pub family: i64,
    pub character: u32,
    pub variable_family: bool,
}
impl MathCharacterCode {
    pub fn from_source(value: i64) -> Result<Self> {
        let atom_class = value.div_euclid(GLYPH_RADIX);
        let glyph = value.rem_euclid(GLYPH_RADIX);
        let variable_family = atom_class >= VARIABLE_FAMILY_CLASS;
        Ok(Self {
            kind: if variable_family {
                AtomKind::Ordinary
            } else {
                AtomKind::from_code(atom_class.rem_euclid(CLASS_RADIX))?
            },
            family: glyph / CHARACTER_RADIX,
            character: (glyph % CHARACTER_RADIX) as u32,
            variable_family,
        })
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelimiterCode {
    pub small_family: i64,
    pub small_character: u32,
    pub large_family: i64,
    pub large_character: u32,
}
impl DelimiterCode {
    pub fn from_source(value: i64) -> Self {
        let small = value.div_euclid(GLYPH_RADIX);
        let large = value.rem_euclid(GLYPH_RADIX);
        Self {
            small_family: small.div_euclid(CHARACTER_RADIX),
            small_character: small.rem_euclid(CHARACTER_RADIX) as u32,
            large_family: large.div_euclid(CHARACTER_RADIX),
            large_character: large.rem_euclid(CHARACTER_RADIX) as u32,
        }
    }
    pub fn candidates(self) -> [(i64, u32); 2] {
        [
            (self.small_family, self.small_character),
            (self.large_family, self.large_character),
        ]
    }
}
