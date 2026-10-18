//! Sparse font character policies (`fonts/microtypography.py`).
use super::formats::tfm::{CharacterTag, TfmFont};
use super::formats::vf::{VirtualFont, VirtualOperation};
use crate::core::errors::Result;
use crate::core::values::{floor_div, round_ratio, trunc_div};
use serde::{Deserialize, Serialize};
use std::cell::{Ref, RefMut};
use std::collections::BTreeMap;
use std::sync::Arc;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FontCode {
    Left,
    Right,
    Expansion,
    Tag,
    SpaceWidth,
    SpaceStretch,
    SpaceShrink,
    Before,
    After,
}
impl FontCode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Left => "lpcode",
            Self::Right => "rpcode",
            Self::Expansion => "efcode",
            Self::Tag => "tagcode",
            Self::SpaceWidth => "knbscode",
            Self::SpaceStretch => "stbscode",
            Self::SpaceShrink => "shbscode",
            Self::Before => "knbccode",
            Self::After => "knaccode",
        }
    }
}
pub fn tag_mask(tag: CharacterTag) -> i64 {
    match tag {
        CharacterTag::None => 0,
        CharacterTag::Ligature => 1,
        CharacterTag::NextLarger => 2,
        CharacterTag::Extensible => 4,
    }
}
pub trait TypographyFont {
    fn name(&self) -> &str;
    fn size(&self) -> i64;
    fn has_character(&self, code: u32) -> bool;
    fn char_width(&self, code: u32) -> i64;
    fn parameter(&self, index: usize) -> i64;
    fn tfm(&self) -> Option<&TfmFont>;
    fn tfm_mut(&mut self) -> Option<&mut TfmFont>;
    fn codes(&self) -> Ref<'_, super::model::MicroCodes>;
    fn codes_mut(&mut self) -> RefMut<'_, super::model::MicroCodes>;
    fn remove_ligature_start(&mut self, code: u32);
    fn install_virtual(
        &mut self,
        virtual_font: VirtualFont,
        width_overrides: BTreeMap<u32, i64>,
        letterspacing: (usize, i64),
    );
}
pub fn code_value(font: &impl TypographyFont, setting: FontCode, character: u32) -> i64 {
    if setting == FontCode::Tag {
        if !font.has_character(character) {
            return -1;
        }
        let tag = font
            .tfm()
            .and_then(|tfm| tfm.char_tags.get(&character))
            .map_or(CharacterTag::None, |&(tag, _)| tag);
        return tag_mask(tag);
    }
    font.codes()
        .get(&setting)
        .and_then(|codes| codes.get(&character))
        .copied()
        .unwrap_or(if setting == FontCode::Expansion {
            1000
        } else {
            0
        })
}
pub fn set_code(font: &mut impl TypographyFont, setting: FontCode, character: u32, value: i64) {
    if setting == FontCode::Tag {
        let value = value.clamp(-7, 0);
        let previous = font
            .codes()
            .get(&setting)
            .and_then(|codes| codes.get(&character))
            .copied()
            .unwrap_or(0);
        font.codes_mut()
            .entry(setting)
            .or_default()
            .insert(character, -(previous.abs() | value.abs()));
        let mut removed_ligature = false;
        if let Some(tfm) = font.tfm_mut() {
            let (tag, remainder) = tfm
                .char_tags
                .get(&character)
                .copied()
                .unwrap_or((CharacterTag::None, 0));
            if tag_mask(tag) & value.abs() != 0 {
                tfm.char_tags
                    .insert(character, (CharacterTag::None, remainder));
                if tag == CharacterTag::Ligature {
                    tfm.lig_kern_start.remove(&character);
                    removed_ligature = true;
                }
            }
        }
        if removed_ligature {
            font.remove_ligature_start(character);
        }
    } else {
        font.codes_mut().entry(setting).or_default().insert(
            character,
            value.clamp(
                if setting == FontCode::Expansion {
                    0
                } else {
                    -1000
                },
                1000,
            ),
        );
    }
}
pub fn scale_em(em: i64, value: i64, denominator: i64) -> Result<i64> {
    let product = em as i128 * value as i128;
    Ok(if value < 0 {
        trunc_div(product, denominator as i128)?
    } else {
        round_ratio(product, denominator as i128)?
    } as i64)
}
pub fn em_amount(font: &impl TypographyFont, setting: FontCode, character: u32) -> Result<i64> {
    scale_em(
        font.parameter(6),
        code_value(font, setting, character),
        1000,
    )
}
pub fn auto_kern(
    font: &impl TypographyFont,
    left: u32,
    right: u32,
    prepend: bool,
    append: bool,
) -> Result<i64> {
    Ok(if append && left < 256 {
        em_amount(font, FontCode::After, left)?
    } else {
        0
    } + if prepend && right < 256 {
        em_amount(font, FontCode::Before, right)?
    } else {
        0
    })
}
pub fn install_letterspacing(
    font: &mut impl TypographyFont,
    base_number: usize,
    amount: i64,
) -> Result<()> {
    let width = scale_em(font.parameter(6), amount, 1000)?;
    let half = scale_em(font.parameter(6), amount, 2000)?;
    let fraction = floor_div(half as i128 * (1 << 20), font.size() as i128)? as i64;
    let codes: Vec<_> = font.tfm().map_or_else(
        || (0..256).collect(),
        |tfm| tfm.width_fix.keys().copied().collect(),
    );
    let widths = codes
        .iter()
        .map(|&code| (code, font.char_width(code) + width))
        .collect();
    let packets = codes
        .into_iter()
        .map(|code| {
            (
                code,
                Arc::from(vec![
                    VirtualOperation::Right(fraction),
                    VirtualOperation::Char {
                        code,
                        advance: true,
                    },
                    VirtualOperation::Right(fraction),
                ]),
            )
        })
        .collect();
    let virtual_font = VirtualFont {
        path: font.name().into(),
        fonts: BTreeMap::from([(0, base_number)]),
        font_order: vec![0],
        packets,
    };
    font.install_virtual(virtual_font, widths, (base_number, amount));
    Ok(())
}
