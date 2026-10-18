//! Decoded immutable TFM instructions (`fonts/ligatures.py`).
use crate::core::errors::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LigatureAction {
    ReplaceBoth,
    ReplaceLeft,
    ReplaceRight,
    InsertBetween,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LigatureBehavior {
    pub action: LigatureAction,
    pub finish_text_pair: bool,
    pub finish_math_pair: bool,
    pub finish_hyphenation_pair: bool,
    pub pass_left_character: bool,
    pub inserted_math_text: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LigatureOperation {
    ReplaceBoth,
    ReplaceLeft,
    ReplaceRight,
    InsertBetween,
    ReplaceLeftAndAdvance,
    ReplaceRightAndAdvance,
    InsertBetweenAndAdvance,
    InsertBetweenAndAdvanceTwice,
    ReplaceBothAndFinishMath,
    ReplaceBothAndFinishMathAndHyphenation,
}
impl LigatureOperation {
    pub fn behavior(self) -> LigatureBehavior {
        use LigatureAction as A;
        let mut behavior = LigatureBehavior {
            action: A::ReplaceBoth,
            finish_text_pair: false,
            finish_math_pair: false,
            finish_hyphenation_pair: false,
            pass_left_character: false,
            inserted_math_text: false,
        };
        match self {
            Self::ReplaceBoth => {}
            Self::ReplaceLeft => behavior.action = A::ReplaceLeft,
            Self::ReplaceRight => behavior.action = A::ReplaceRight,
            Self::InsertBetween => behavior.action = A::InsertBetween,
            Self::ReplaceLeftAndAdvance | Self::ReplaceRightAndAdvance => {
                behavior.action = if self == Self::ReplaceLeftAndAdvance {
                    A::ReplaceLeft
                } else {
                    A::ReplaceRight
                };
                behavior.finish_text_pair = true;
                behavior.finish_math_pair = true;
                behavior.finish_hyphenation_pair = true;
            }
            Self::InsertBetweenAndAdvance | Self::InsertBetweenAndAdvanceTwice => {
                behavior.action = A::InsertBetween;
                behavior.finish_math_pair = true;
                behavior.pass_left_character = true;
                if self == Self::InsertBetweenAndAdvanceTwice {
                    behavior.finish_text_pair = true;
                    behavior.finish_hyphenation_pair = true;
                    behavior.inserted_math_text = true;
                }
            }
            Self::ReplaceBothAndFinishMath => behavior.finish_math_pair = true,
            Self::ReplaceBothAndFinishMathAndHyphenation => {
                behavior.finish_math_pair = true;
                behavior.finish_hyphenation_pair = true;
            }
        }
        behavior
    }
}
pub const TFM_STOP_FLAG: u8 = 128;
pub const TFM_KERN_FLAG: u8 = 128;
pub const TFM_BYTE_RADIX: usize = 256;

pub fn decode_ligature_operation(opcode: i64) -> Result<LigatureOperation> {
    use LigatureOperation::*;
    if !(0..128).contains(&opcode) {
        return Err(Error::value(format!(
            "Not a TFM ligature operation byte: {opcode}"
        )));
    }
    Ok(match opcode {
        0 => ReplaceBoth,
        1 => ReplaceLeft,
        2 => ReplaceRight,
        3 => InsertBetween,
        4 => ReplaceBothAndFinishMath,
        5 => ReplaceLeftAndAdvance,
        6 => ReplaceRightAndAdvance,
        7 => InsertBetweenAndAdvance,
        11 => InsertBetweenAndAdvanceTwice,
        _ => ReplaceBothAndFinishMathAndHyphenation,
    })
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LigatureInstruction {
    pub next_character: Option<u32>,
    pub next_instruction: Option<usize>,
    pub restart_address: Option<usize>,
    pub replacement: Option<u32>,
    pub kern_index: Option<usize>,
    pub operation: Option<LigatureOperation>,
    pub tfm_opcode: Option<u8>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LigatureMatch {
    pub character: Option<u32>,
    pub kern: Option<i64>,
    pub operation: Option<LigatureOperation>,
    pub tfm_opcode: Option<u8>,
}
pub fn decode_ligature_program(program: &[[u8; 4]]) -> Vec<LigatureInstruction> {
    program
        .iter()
        .enumerate()
        .map(|(index, &[skip, right, opcode, remainder])| {
            let mut record = LigatureInstruction::default();
            if skip > TFM_STOP_FLAG {
                record.restart_address = Some(256 * usize::from(opcode) + usize::from(remainder));
            } else {
                record.next_character = Some(right.into());
                record.next_instruction =
                    (skip < TFM_STOP_FLAG).then_some(index + usize::from(skip) + 1);
                record.tfm_opcode = Some(opcode);
                if opcode >= TFM_KERN_FLAG {
                    record.kern_index =
                        Some(256 * usize::from(opcode - TFM_KERN_FLAG) + usize::from(remainder));
                } else {
                    record.replacement = Some(remainder.into());
                    record.operation = Some(
                        decode_ligature_operation(opcode.into()).expect("validated ligature byte"),
                    );
                }
            }
            record
        })
        .collect()
}
