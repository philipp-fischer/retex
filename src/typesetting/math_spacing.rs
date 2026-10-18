//! Math atom classes and TeX's inter-atom spacing matrix (`math_spacing.py`).
use crate::core::errors::{Error, Result};
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum AtomKind {
    #[default]
    Ordinary = 0,
    Operator = 1,
    Binary = 2,
    Relation = 3,
    Opening = 4,
    Closing = 5,
    Punctuation = 6,
    Inner = 7,
}
impl AtomKind {
    pub fn from_code(code: i64) -> Result<Self> {
        Ok(match code {
            0 => Self::Ordinary,
            1 => Self::Operator,
            2 => Self::Binary,
            3 => Self::Relation,
            4 => Self::Opening,
            5 => Self::Closing,
            6 => Self::Punctuation,
            7 => Self::Inner,
            _ => return Err(Error::value("Invalid math atom class")),
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MathSpacing {
    None,
    Thin,
    ThinNonscript,
    MediumNonscript,
    ThickNonscript,
    Invalid,
}
impl MathSpacing {
    pub fn parameter(self, script: bool) -> Option<&'static str> {
        if self == Self::Thin {
            return Some("thinmuskip");
        }
        if script {
            return None;
        }
        match self {
            Self::ThinNonscript => Some("thinmuskip"),
            Self::MediumNonscript => Some("medmuskip"),
            Self::ThickNonscript => Some("thickmuskip"),
            _ => None,
        }
    }
}
use MathSpacing::{
    Invalid as I, MediumNonscript as MN, None as N, ThickNonscript as KN, Thin as T,
    ThinNonscript as TN,
};
pub const SPACING: [[MathSpacing; 8]; 8] = [
    [N, T, MN, KN, N, N, N, TN],
    [T, T, I, KN, N, N, N, TN],
    [MN, MN, I, I, MN, I, I, MN],
    [KN, KN, I, N, KN, N, N, KN],
    [N, N, I, N, N, N, N, N],
    [N, T, MN, KN, N, N, N, TN],
    [TN, TN, I, TN, TN, TN, TN, TN],
    [TN, T, MN, KN, TN, N, TN, TN],
];
