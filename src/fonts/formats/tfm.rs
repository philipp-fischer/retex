//! Classic TFM reader, retaining raw metrics and instructions (`formats/tfm.py`).
use crate::fonts::ligatures::{LigatureInstruction, decode_ligature_program};
use crate::fonts::scaling::{FIX_WORD_TO_SCALED_SHIFT, TFM_FRACTION_BITS};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

const TFM_DIMENSION_LIMIT: i64 = 16 * (1 << TFM_FRACTION_BITS);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum CharacterTag {
    #[default]
    None = 0,
    Ligature = 1,
    NextLarger = 2,
    Extensible = 3,
}
impl CharacterTag {
    fn from_bits(bits: u8) -> Self {
        match bits & 3 {
            0 => Self::None,
            1 => Self::Ligature,
            2 => Self::NextLarger,
            _ => Self::Extensible,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TfmFont {
    pub checksum: u32,
    pub memory_words: usize,
    pub design_size: i64,
    pub bc: u16,
    pub ec: u16,
    pub widths: BTreeMap<u32, i64>,
    pub heights: BTreeMap<u32, i64>,
    pub depths: BTreeMap<u32, i64>,
    pub italics: BTreeMap<u32, i64>,
    pub lig_kern: Vec<LigatureInstruction>,
    pub lig_kern_start: BTreeMap<u32, usize>,
    pub kerns: Vec<i64>,
    pub boundary_char: u32,
    pub boundary_label: Option<usize>,
    pub false_boundary_char: u32,
    pub params: Vec<i64>,
    pub width_fix: BTreeMap<u32, i64>,
    pub height_fix: BTreeMap<u32, i64>,
    pub depth_fix: BTreeMap<u32, i64>,
    pub italic_fix: BTreeMap<u32, i64>,
    pub kern_fix: Vec<i64>,
    pub param_fix: Vec<i64>,
    pub char_tags: BTreeMap<u32, (CharacterTag, u8)>,
    pub extensible: BTreeMap<u32, [u8; 4]>,
}
impl Default for TfmFont {
    fn default() -> Self {
        Self {
            checksum: 0,
            memory_words: 0,
            design_size: 0,
            bc: 0,
            ec: 255,
            widths: BTreeMap::new(),
            heights: BTreeMap::new(),
            depths: BTreeMap::new(),
            italics: BTreeMap::new(),
            lig_kern: Vec::new(),
            lig_kern_start: BTreeMap::new(),
            kerns: Vec::new(),
            boundary_char: 256,
            boundary_label: None,
            false_boundary_char: 256,
            params: Vec::new(),
            width_fix: BTreeMap::new(),
            height_fix: BTreeMap::new(),
            depth_fix: BTreeMap::new(),
            italic_fix: BTreeMap::new(),
            kern_fix: Vec::new(),
            param_fix: Vec::new(),
            char_tags: BTreeMap::new(),
            extensible: BTreeMap::new(),
        }
    }
}
#[derive(Default)]
pub struct TfmReader {
    pub debug: bool,
}
impl TfmReader {
    pub fn read_tfm(&self, filename: impl AsRef<Path>) -> Option<TfmFont> {
        match crate::resources::host::read(filename) {
            Ok(data) => self.read_bytes(&data),
            Err(error) => {
                if self.debug {
                    crate::resources::host::console_line(
                        true,
                        format!("Error reading TFM: {error}"),
                    );
                }
                None
            }
        }
    }
    pub fn read_u16(data: &[u8], pos: usize) -> Option<u16> {
        Some(u16::from_be_bytes(data.get(pos..pos + 2)?.try_into().ok()?))
    }
    pub fn read_u32(data: &[u8], pos: usize) -> Option<u32> {
        Some(u32::from_be_bytes(data.get(pos..pos + 4)?.try_into().ok()?))
    }
    pub fn read_fix_word(data: &[u8], pos: usize) -> Option<i64> {
        Some(i32::from_be_bytes(data.get(pos..pos + 4)?.try_into().ok()?) as i64)
    }
    pub fn fix_word_to_scaled(value: i64, design_size: i64) -> i64 {
        ((value as i128 * design_size as i128) >> TFM_FRACTION_BITS) as i64
    }

    pub fn read_bytes(&self, data: &[u8]) -> Option<TfmFont> {
        let mut header = [0_usize; 12];
        for (index, value) in header.iter_mut().enumerate() {
            *value = Self::read_u16(data, index * 2)? as usize;
        }
        let [lf, lh, bc, ec, nw, nh, nd, ni, nl, nk, ne, np] = header;
        let count = ec as isize - bc as isize + 1;
        if count < 0 {
            return None;
        }
        let expected = 6 + lh + count as usize + nw + nh + nd + ni + nl + nk + ne + np;
        if lf != expected
            || data.len() < lf * 4
            || lh < 2
            || bc > ec + 1
            || ec > 255
            || [lf, lh, nw, nh, nd, ni, nl, nk, ne, np]
                .iter()
                .any(|&n| n > 32767)
            || [nw, nh, nd, ni].contains(&0)
        {
            if self.debug {
                crate::resources::host::console_line(
                    true,
                    format!("TFM length mismatch: {lf} != {expected}"),
                );
            }
            return None;
        }
        let mut font = TfmFont {
            bc: bc as u16,
            ec: ec as u16,
            memory_words: lf - 6 - lh + 7_usize.saturating_sub(np),
            checksum: Self::read_u32(data, 24)?,
            design_size: Self::read_fix_word(data, 28)? >> FIX_WORD_TO_SCALED_SHIFT,
            ..TfmFont::default()
        };
        if font.design_size < 65536 {
            return None;
        }
        let mut pos = 24 + lh * 4;
        let mut char_info = Vec::new();
        for _ in 0..count {
            let info = data.get(pos..pos + 4)?;
            let (w, h, d, it, tag, remainder) = (
                info[0] as usize,
                (info[1] >> 4) as usize,
                (info[1] & 15) as usize,
                (info[2] >> 2) as usize,
                CharacterTag::from_bits(info[2]),
                info[3],
            );
            if w >= nw
                || h >= nh
                || d >= nd
                || it >= ni
                || (tag == CharacterTag::Ligature && remainder as usize >= nl)
                || (tag == CharacterTag::Extensible && remainder as usize >= ne)
            {
                return None;
            }
            char_info.push((w, h, d, it, tag, remainder));
            pos += 4;
        }
        let mut tables = Vec::new();
        for length in [nw, nh, nd, ni] {
            let mut table = Vec::new();
            for _ in 0..length {
                table.push(Self::read_fix_word(data, pos)?);
                pos += 4;
            }
            tables.push(table);
        }
        let mut program = Vec::new();
        for _ in 0..nl {
            program.push(<[u8; 4]>::try_from(data.get(pos..pos + 4)?).ok()?);
            pos += 4;
        }
        for _ in 0..nk {
            let raw = Self::read_fix_word(data, pos)?;
            font.kern_fix.push(raw);
            font.kerns
                .push(Self::fix_word_to_scaled(raw, font.design_size));
            pos += 4;
        }
        let mut recipes = Vec::new();
        for _ in 0..ne {
            recipes.push(<[u8; 4]>::try_from(data.get(pos..pos + 4)?).ok()?);
            pos += 4;
        }
        for index in 0..np {
            let raw = Self::read_fix_word(data, pos)?;
            pos += 4;
            font.param_fix.push(raw);
            font.params.push(if index == 0 {
                raw >> FIX_WORD_TO_SCALED_SHIFT
            } else {
                Self::fix_word_to_scaled(raw, font.design_size)
            });
        }
        if tables.iter().any(|t| t[0] != 0)
            || tables
                .iter()
                .flatten()
                .chain(font.kern_fix.iter())
                .chain(font.param_fix.iter().skip(1))
                .any(|&v| !(-TFM_DIMENSION_LIMIT..TFM_DIMENSION_LIMIT).contains(&v))
        {
            return None;
        }
        let exists = |code: usize| bc <= code && code <= ec && char_info[code - bc].0 != 0;
        for (index, info) in char_info.iter().enumerate() {
            if info.4 == CharacterTag::NextLarger {
                let mut visited = HashSet::from([bc + index]);
                let mut next = info.5 as usize;
                loop {
                    if next < bc || next > ec || !visited.insert(next) {
                        return None;
                    }
                    let next_info = &char_info[next - bc];
                    if next_info.4 != CharacterTag::NextLarger {
                        break;
                    }
                    next = next_info.5 as usize;
                }
            }
        }
        let boundary = if nl > 0 && program[0][0] == 255 {
            program[0][1] as usize
        } else {
            256
        };
        for (index, &[skip, right, op, rem]) in program.iter().enumerate() {
            if skip > 128 {
                if 256 * op as usize + rem as usize >= nl {
                    return None;
                }
            } else if (right as usize != boundary && !exists(right as usize))
                || (op < 128 && !exists(rem as usize))
                || (op >= 128 && 256 * (op - 128) as usize + rem as usize >= nk)
                || (skip < 128 && index + skip as usize + 1 >= nl)
            {
                return None;
            }
        }
        if recipes.iter().any(|recipe| {
            recipe
                .iter()
                .enumerate()
                .any(|(i, &c)| (c != 0 || i == 3) && !exists(c as usize))
        }) {
            return None;
        }
        font.boundary_char = boundary as u32;
        font.false_boundary_char = if exists(boundary) {
            256
        } else {
            boundary as u32
        };
        if nl > 0 && program[nl - 1][0] == 255 {
            font.boundary_label =
                Some(256 * program[nl - 1][2] as usize + program[nl - 1][3] as usize);
        }
        font.params.resize(font.params.len().max(7), 0);
        for (index, &(w, h, d, it, tag, remainder)) in char_info.iter().enumerate() {
            let code = (bc + index) as u32;
            font.char_tags.insert(code, (tag, remainder));
            if tag == CharacterTag::Extensible {
                font.extensible.insert(code, recipes[remainder as usize]);
            }
            if w == 0 {
                continue;
            }
            for ((selected, raw), (table, at)) in [
                (&mut font.widths, &mut font.width_fix),
                (&mut font.heights, &mut font.height_fix),
                (&mut font.depths, &mut font.depth_fix),
                (&mut font.italics, &mut font.italic_fix),
            ]
            .into_iter()
            .zip(tables.iter().zip([w, h, d, it]))
            {
                raw.insert(code, table[at]);
                selected.insert(code, Self::fix_word_to_scaled(table[at], font.design_size));
            }
            if tag == CharacterTag::Ligature {
                font.lig_kern_start.insert(code, remainder as usize);
            }
        }
        font.lig_kern = decode_ligature_program(&program);
        Some(font)
    }
}
