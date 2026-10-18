//! Owned SFNT resources and pdfTeX TrueType remapping (`formats/sfnt.py`).
use crate::core::errors::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
include!("sfnt_tables.rs");

pub const UINT16_MASK: u32 = 0xffff;
pub const UINT32_MASK: u64 = 0xffff_ffff;
pub const INT16_MAX: u32 = 32767;
pub const FIXED_POINT_SCALE: u32 = 65536;
pub const SFNT_CHECKSUM_TOTAL: u32 = 0xb1b0_afba;
pub type TableTag = [u8; 4];
pub type Tables = BTreeMap<TableTag, Vec<u8>>;

#[derive(Clone, Copy, Debug)]
pub struct CompositeGlyphFlag(pub u16);
impl CompositeGlyphFlag {
    pub const ARGUMENT_WORDS: u16 = 1;
    pub const SCALE: u16 = 8;
    pub const MORE_COMPONENTS: u16 = 32;
    pub const XY_SCALE: u16 = 64;
    pub const MATRIX: u16 = 128;
    pub fn argument_bytes(self) -> usize {
        if self.0 & Self::ARGUMENT_WORDS != 0 {
            4
        } else {
            2
        }
    }
    pub fn transform_bytes(self) -> usize {
        if self.0 & Self::SCALE != 0 {
            2
        } else if self.0 & Self::XY_SCALE != 0 {
            4
        } else if self.0 & Self::MATRIX != 0 {
            8
        } else {
            0
        }
    }
}
pub fn bytes(data: &[u8], offset: usize, count: usize) -> Result<&[u8]> {
    data.get(
        offset..offset.checked_add(count).ok_or_else(|| {
            Error::value("Truncated SFNT table").with_issue(crate::core::recovery::Issue::Font)
        })?,
    )
    .ok_or_else(|| {
        Error::value("Truncated SFNT table").with_issue(crate::core::recovery::Issue::Font)
    })
}
pub fn u16_at(data: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(
        bytes(data, offset, 2)?.try_into().unwrap(),
    ))
}
pub fn i16_at(data: &[u8], offset: usize) -> Result<i16> {
    Ok(i16::from_be_bytes(
        bytes(data, offset, 2)?.try_into().unwrap(),
    ))
}
pub fn u32_at(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(
        bytes(data, offset, 4)?.try_into().unwrap(),
    ))
}
fn put16(data: &mut [u8], offset: usize, value: u16) -> Result<()> {
    data.get_mut(offset..offset + 2)
        .ok_or_else(|| {
            Error::value("Truncated SFNT table").with_issue(crate::core::recovery::Issue::Font)
        })?
        .copy_from_slice(&value.to_be_bytes());
    Ok(())
}
fn put32(data: &mut [u8], offset: usize, value: u32) -> Result<()> {
    data.get_mut(offset..offset + 4)
        .ok_or_else(|| {
            Error::value("Truncated SFNT table").with_issue(crate::core::recovery::Issue::Font)
        })?
        .copy_from_slice(&value.to_be_bytes());
    Ok(())
}
fn words16(data: &mut Vec<u8>, words: impl IntoIterator<Item = u16>) {
    for word in words {
        data.extend_from_slice(&word.to_be_bytes());
    }
}
fn words32(data: &mut Vec<u8>, words: impl IntoIterator<Item = u32>) {
    for word in words {
        data.extend_from_slice(&word.to_be_bytes());
    }
}
fn table<'a>(tables: &'a Tables, tag: &TableTag) -> Result<&'a [u8]> {
    tables.get(tag).map(Vec::as_slice).ok_or_else(|| {
        Error::value(format!(
            "Missing SFNT table {}",
            String::from_utf8_lossy(tag)
        ))
    })
}
fn latin1(data: &[u8]) -> String {
    data.iter().map(|&byte| char::from(byte)).collect()
}
pub fn table_directory(data: &[u8]) -> Result<Tables> {
    let base = if data.starts_with(b"ttcf") {
        if u32_at(data, 8)? == 0 {
            return Err(Error::value("Empty TrueType collection")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        u32_at(data, 12)? as usize
    } else {
        0
    };
    if ![b"\0\x01\0\0".as_slice(), b"true", b"OTTO"].contains(&bytes(data, base, 4)?) {
        return Err(
            Error::value("Invalid SFNT signature").with_issue(crate::core::recovery::Issue::Font)
        );
    }
    let count = u16_at(data, base + 4)? as usize;
    let mut result = Tables::new();
    for index in 0..count {
        let position = base + 12 + index * 16;
        let tag: TableTag = bytes(data, position, 4)?.try_into().unwrap();
        let offset = u32_at(data, position + 8)? as usize;
        let length = u32_at(data, position + 12)? as usize;
        if result.contains_key(&tag)
            || offset
                .checked_add(length)
                .is_none_or(|end| end > data.len())
        {
            return Err(Error::value("Invalid SFNT table directory")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        result.insert(tag, bytes(data, offset, length)?.to_vec());
    }
    Ok(result)
}
pub fn checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0_u32, |sum, chunk| {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}
pub fn assemble(source: &Tables) -> Result<Vec<u8>> {
    let mut tables = source.clone();
    let head = tables.get_mut(b"head").ok_or_else(|| {
        Error::value("Missing SFNT table head").with_issue(crate::core::recovery::Issue::Font)
    })?;
    put32(head, 8, 0)?;
    let count = tables.len();
    if count == 0 || count > u16::MAX as usize {
        return Err(
            Error::value("Invalid SFNT table count").with_issue(crate::core::recovery::Issue::Font)
        );
    }
    let power = 1_usize << (usize::BITS - count.leading_zeros() - 1);
    let mut result = Vec::new();
    words32(&mut result, [0x10000]);
    words16(
        &mut result,
        [
            count as u16,
            (power * 16) as u16,
            power.ilog2() as u16,
            (count * 16 - power * 16) as u16,
        ],
    );
    let mut body = Vec::new();
    let mut offset = 12 + 16 * count;
    let mut head_offset = 0;
    for (tag, data) in tables {
        if &tag == b"head" {
            head_offset = offset;
        }
        result.extend_from_slice(&tag);
        words32(
            &mut result,
            [checksum(&data), offset as u32, data.len() as u32],
        );
        body.extend_from_slice(&data);
        let padding = (4 - data.len() % 4) % 4;
        body.resize(body.len() + padding, 0);
        offset += data.len() + padding;
    }
    result.extend(body);
    let adjustment = SFNT_CHECKSUM_TOTAL.wrapping_sub(checksum(&result));
    put32(&mut result, head_offset + 8, adjustment)?;
    Ok(result)
}
pub fn post_names(post: &[u8], count: usize, warn: &mut dyn FnMut(&str)) -> Result<Vec<String>> {
    let version = u32_at(post, 0)?;
    if version == 0x10000 {
        return Ok((0..count)
            .map(|index| MAC_NAMES.get(index).unwrap_or(&".notdef").to_string())
            .collect());
    }
    if version != 0x20000 {
        if version != 0x30000 {
            warn("Unsupported post format, assuming 3.0");
        }
        return Ok(vec![".notdef".into(); count]);
    }
    let number = u16_at(post, 32)? as usize;
    let indices = (0..number)
        .map(|index| u16_at(post, 34 + index * 2))
        .collect::<Result<Vec<_>>>()?;
    let mut names: Vec<String> = MAC_NAMES.iter().map(|name| name.to_string()).collect();
    let mut cursor = 34 + 2 * number;
    while cursor < post.len() {
        let length = post[cursor] as usize;
        cursor += 1;
        if cursor + length > post.len() {
            return Err(Error::value("Truncated TrueType post name")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        names.push(latin1(&post[cursor..cursor + length]));
        cursor += length;
    }
    if number > count || indices.iter().any(|&index| index as usize >= names.len()) {
        return Err(Error::value("Invalid TrueType post name index")
            .with_issue(crate::core::recovery::Issue::Font));
    }
    let mut result: Vec<_> = indices
        .into_iter()
        .map(|index| names[index as usize].clone())
        .collect();
    result.resize(count, ".notdef".into());
    Ok(result)
}
pub fn cmap_format4(
    tables: &Tables,
    platform: u16,
    encoding: u16,
) -> Result<Option<BTreeMap<u32, usize>>> {
    let cmap = table(tables, b"cmap")?;
    let count = u16_at(cmap, 2)? as usize;
    for index in 0..count {
        let position = 4 + index * 8;
        let offset = u32_at(cmap, position + 4)? as usize;
        if (u16_at(cmap, position)?, u16_at(cmap, position + 2)?) != (platform, encoding)
            || u16_at(cmap, offset)? != 4
        {
            continue;
        }
        let length = u16_at(cmap, offset + 2)? as usize;
        let data = bytes(cmap, offset, length)?;
        let segments = u16_at(data, 6)? as usize / 2;
        let mut mapping = BTreeMap::new();
        for segment in 0..segments {
            let end = u16_at(data, 14 + segment * 2)?;
            let start = u16_at(data, 16 + segments * 2 + segment * 2)?;
            let delta = i16_at(data, 16 + segments * 4 + segment * 2)?;
            let relative = u16_at(data, 16 + segments * 6 + segment * 2)?;
            if start > end {
                return Err(Error::value("Invalid TrueType cmap segment")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            for code in u32::from(start)..=u32::from(end) {
                let mut glyph = if relative == 0 {
                    ((code as i64 + delta as i64) & UINT16_MASK as i64) as u16
                } else {
                    u16_at(
                        data,
                        16 + segments * 6
                            + segment * 2
                            + relative as usize
                            + (code as usize - start as usize) * 2,
                    )?
                };
                if relative != 0 && glyph != 0 {
                    glyph = glyph.wrapping_add(delta as u16);
                }
                if glyph != 0 {
                    mapping.insert(code, glyph as usize);
                }
            }
        }
        let glyph_count = u16_at(table(tables, b"maxp")?, 4)? as usize;
        if mapping.values().any(|&glyph| glyph >= glyph_count) {
            return Err(Error::value("TrueType cmap glyph index out of range")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        return Ok(Some(mapping));
    }
    Ok(None)
}
pub fn byte_cmap(indices: &[u16; 256], count: usize) -> Vec<u8> {
    let mut mac = Vec::new();
    if count < 256 {
        words16(&mut mac, [0, 262, 0]);
        mac.extend(indices.iter().map(|index| *index as u8));
    } else {
        words16(&mut mac, [6, 522, 0, 0, 256]);
        words16(&mut mac, indices.iter().copied());
    }
    let mut symbol = Vec::new();
    words16(
        &mut symbol,
        [
            4, 544, 0, 4, 4, 1, 0, 0xf0ff, 0xffff, 0, 0xf000, 0xffff, 0, 1, 4, 0,
        ],
    );
    words16(&mut symbol, indices.iter().copied());
    let mut result = Vec::new();
    words16(&mut result, [0, 2, 1, 0]);
    words32(&mut result, [20]);
    words16(&mut result, [3, 0]);
    words32(&mut result, [20 + mac.len() as u32]);
    result.extend(mac);
    result.extend(symbol);
    result
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SfntKind {
    Truetype,
    Opentype,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SfntResource {
    pub data: Vec<u8>,
    pub name: String,
    pub bbox: [i64; 4],
    pub italic_angle: f64,
    pub fixed_pitch: bool,
    pub descriptor_metrics: Vec<(String, i64)>,
    pub tables: Tables,
    pub glyph_names: Vec<String>,
    pub kind: SfntKind,
    pub encoding: Vec<String>,
    pub encoding_source: String,
    pub matrix: [f64; 6],
    pub length1: usize,
    pub length2: usize,
    pub length3: usize,
}
impl SfntResource {
    pub fn read(path: &Path, warn: &mut dyn FnMut(&str)) -> Result<Self> {
        Self::parse(&crate::resources::host::read(path)?, warn)
    }
    pub fn parse(data: &[u8], warn: &mut dyn FnMut(&str)) -> Result<Self> {
        let tables = table_directory(data)?;
        let head = table(&tables, b"head")?;
        let hhea = table(&tables, b"hhea")?;
        let post = table(&tables, b"post")?;
        let units = u16_at(head, 18)? as i64;
        if units == 0 {
            return Err(Error::value("Invalid SFNT unitsPerEm")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        let scale = |value: i64| value * 1000 / units;
        let bbox = [
            scale(i16_at(head, 36)? as i64),
            scale(i16_at(head, 38)? as i64),
            scale(i16_at(head, 40)? as i64),
            scale(i16_at(head, 42)? as i64),
        ];
        let mut metrics = vec![
            ("Ascent".into(), scale(i16_at(hhea, 4)? as i64)),
            ("Descent".into(), scale(i16_at(hhea, 6)? as i64)),
        ];
        if let Some(pclt) = tables.get(b"PCLT") {
            metrics.extend([
                ("XHeight".into(), scale(u16_at(pclt, 10)? as i64)),
                ("CapHeight".into(), scale(u16_at(pclt, 16)? as i64)),
            ]);
        }
        let mut name = "Unknown".to_string();
        if let Some(names) = tables.get(b"name") {
            let count = u16_at(names, 2)? as usize;
            let base = u16_at(names, 4)? as usize;
            let mut entries = Vec::new();
            for index in 0..count {
                let mut entry = [0_u16; 6];
                for (field, value) in entry.iter_mut().enumerate() {
                    *value = u16_at(names, 6 + 12 * index + field * 2)?;
                }
                entries.push(entry);
            }
            if let Some(entry) = entries
                .iter()
                .find(|entry| entry[0] == 1 && entry[1] == 0 && entry[3] == 6)
                .or_else(|| {
                    entries
                        .iter()
                        .find(|entry| entry[0] == 3 && matches!(entry[1], 0 | 1) && entry[3] == 6)
                })
            {
                let begin = base + entry[5] as usize;
                let raw =
                    &names[begin.min(names.len())..(begin + entry[4] as usize).min(names.len())];
                name = if entry[0] == 3 {
                    raw.iter()
                        .skip(1)
                        .step_by(2)
                        .map(|&byte| char::from(byte))
                        .collect()
                } else {
                    latin1(raw)
                };
                name.retain(|ch| !ch.is_whitespace() && !"()<>[]{}/%".contains(ch));
            }
        }
        let fixed = u32_at(post, 4)?;
        let whole = fixed / FIXED_POINT_SCALE;
        let fraction = (fixed % FIXED_POINT_SCALE) as f64 / FIXED_POINT_SCALE as f64;
        let angle = if whole > INT16_MAX {
            -((FIXED_POINT_SCALE - whole) as f64 + fraction)
        } else {
            whole as f64 + fraction
        };
        let fixed_pitch = u32_at(post, 12)? != 0;
        let (owned_data, glyph_names, kind, length1) = if let Some(cff) = tables.get(b"CFF ") {
            if cff.len() < 4 || cff[0] != 1 {
                return Err(Error::value("Invalid CFF header")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            let mut cursor = cff[2] as usize;
            let number = u16_at(cff, cursor)?;
            cursor += 2;
            if number != 1 {
                return Err(Error::value("OpenType CFF must contain one font")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            let size = *cff.get(cursor).ok_or_else(|| {
                Error::value("Truncated CFF Name INDEX")
                    .with_issue(crate::core::recovery::Issue::Font)
            })? as usize;
            cursor += 1;
            if !(1..=4).contains(&size) {
                return Err(Error::value("Invalid CFF INDEX offset size")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            if cursor + size * 2 > cff.len() {
                return Err(Error::value("Truncated CFF INDEX offsets")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            let decode = |slice: &[u8]| {
                slice
                    .iter()
                    .fold(0_usize, |value, &byte| value * 256 + byte as usize)
            };
            let first = decode(&cff[cursor..cursor + size]);
            let last = decode(&cff[cursor + size..cursor + size * 2]);
            cursor += size * 2;
            if first != 1 || last <= first || cursor + last - 1 > cff.len() {
                return Err(Error::value("Invalid CFF Name INDEX offsets")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            name = latin1(&cff[cursor + first - 1..cursor + last - 1]);
            (cff.clone(), Vec::new(), SfntKind::Opentype, 0)
        } else {
            for tag in [
                b"maxp", b"loca", b"glyf", b"hmtx", b"cmap", b"name", b"OS/2",
            ] {
                if !tables.contains_key(tag) {
                    return Err(Error::value(format!(
                        "Missing TrueType table {}",
                        String::from_utf8_lossy(tag)
                    )));
                }
            }
            let count = u16_at(table(&tables, b"maxp")?, 4)? as usize;
            if count < 2 {
                return Err(Error::value("TrueType requires .notdef and .null glyphs")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            (
                assemble(&tables)?,
                post_names(post, count, warn)?,
                SfntKind::Truetype,
                data.len(),
            )
        };
        Ok(Self {
            data: owned_data,
            name,
            bbox,
            italic_angle: angle,
            fixed_pitch,
            descriptor_metrics: metrics,
            tables,
            glyph_names,
            kind,
            encoding: vec![".notdef".into(); 256],
            encoding_source: "SFNT".into(),
            matrix: [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
            length1,
            length2: 0,
            length3: 0,
        })
    }
    pub fn subset(
        &self,
        encoding: Option<&[String]>,
        codes: &BTreeSet<u8>,
        warn: &mut dyn FnMut(&str),
        subfont: Option<&[i32]>,
        pid_eid: (u16, u16),
    ) -> Result<(Self, Vec<usize>)> {
        if self.kind != SfntKind::Truetype {
            return Err(Error::value("OTF fonts must be included entirely")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        let encoding = encoding.filter(|encoding| !encoding.is_empty());
        if encoding.is_none() && subfont.is_none() {
            return Err(
                Error::value("Subset TrueType must be a reencoded or a subfont")
                    .with_issue(crate::core::recovery::Issue::Font),
            );
        }
        let mut indices = [0_u16; 256];
        let mut order = vec![0, 1];
        let mut mapped = BTreeMap::from([(0_usize, 0_usize), (1, 1)]);
        let mut unicode: Option<BTreeMap<u32, usize>> = None;
        let mut requested = codes.clone();
        let mut by_name = BTreeMap::new();
        for (index, name) in self.glyph_names.iter().enumerate() {
            if name != ".notdef" {
                by_name.entry(name.as_str()).or_insert(index);
            }
        }
        if encoding.is_some_and(|encoding| encoding.get(97).is_some_and(|name| name == "a")) {
            requested.insert(97);
        }
        if subfont.is_some() {
            unicode = cmap_format4(&self.tables, pid_eid.0, pid_eid.1)?;
            if unicode.is_none() {
                warn(&format!(
                    "Cannot find cmap subtable for PidEid=({}, {})",
                    pid_eid.0, pid_eid.1
                ));
                unicode = Some(BTreeMap::new());
            }
        }
        for code in requested {
            let name = match encoding {
                Some(encoding) => encoding.get(code as usize).cloned().ok_or_else(|| {
                    Error::value("Invalid TrueType encoding")
                        .with_issue(crate::core::recovery::Issue::Font)
                })?,
                None => format!("subfont {code}"),
            };
            if name == ".notdef" {
                continue;
            }
            let mut glyph = if let Some(subfont) = subfont {
                subfont
                    .get(code as usize)
                    .and_then(|code| unicode.as_ref().unwrap().get(&(*code as u32)))
                    .copied()
            } else {
                by_name.get(name.as_str()).copied()
            };
            if glyph.is_none() {
                if let Some(hex) = name.strip_prefix("uni").filter(|hex| {
                    !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
                }) {
                    if unicode.is_none() {
                        unicode = Some(
                            cmap_format4(&self.tables, 3, 1)?
                                .filter(|map| !map.is_empty())
                                .or(cmap_format4(&self.tables, 0, 3)?)
                                .unwrap_or_default(),
                        );
                    }
                    glyph = u32::from_str_radix(hex, 16)
                        .ok()
                        .and_then(|code| unicode.as_ref().unwrap().get(&code).copied());
                }
            }
            if glyph.is_none() {
                if let Some(raw) = name.strip_prefix("index") {
                    let parsed = if let Some(hex) =
                        raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X"))
                    {
                        (!hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
                            .then(|| usize::from_str_radix(hex, 16))
                            .transpose()
                    } else if !raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_digit()) {
                        Some(usize::from_str_radix(
                            raw,
                            if raw.len() > 1 && raw.starts_with('0') {
                                8
                            } else {
                                10
                            },
                        ))
                        .transpose()
                    } else {
                        Ok(None)
                    };
                    glyph = parsed.map_err(|_| {
                        Error::value("Invalid TrueType glyph index")
                            .with_issue(crate::core::recovery::Issue::Font)
                    })?;
                }
            }
            let Some(glyph) = glyph.filter(|&glyph| glyph < self.glyph_names.len()) else {
                warn(&format!("TrueType glyph `{name}' undefined"));
                continue;
            };
            let new = if let Some(&new) = mapped.get(&glyph) {
                new
            } else {
                let new = order.len();
                mapped.insert(glyph, new);
                order.push(glyph);
                new
            };
            indices[code as usize] = u16::try_from(new).map_err(|_| {
                Error::value("Too many TrueType subset glyphs")
                    .with_issue(crate::core::recovery::Issue::Font)
            })?;
        }
        let mut tables = self.tables.clone();
        let count = self.glyph_names.len();
        let long = i16_at(table(&tables, b"head")?, 50)? != 0;
        let loca = table(&tables, b"loca")?;
        let offsets = (0..=count)
            .map(|index| {
                if long {
                    u32_at(loca, index * 4).map(|n| n as usize)
                } else {
                    u16_at(loca, index * 2).map(|n| n as usize * 2)
                }
            })
            .collect::<Result<Vec<_>>>()?;
        let source_glyf = table(&tables, b"glyf")?;
        if offsets[count] > source_glyf.len()
            || offsets.windows(2).any(|window| window[0] > window[1])
        {
            return Err(Error::value("Invalid TrueType loca")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        let mut glyf = Vec::new();
        let mut new_offsets = Vec::new();
        let mut position = 0;
        while position < order.len() {
            let old = order[position];
            position += 1;
            new_offsets.push(glyf.len() as u32);
            let mut program = source_glyf[offsets[old]..offsets[old + 1]].to_vec();
            if !program.is_empty() && i16_at(&program, 0)? < 0 {
                let mut cursor = 10;
                loop {
                    let flags = CompositeGlyphFlag(u16_at(&program, cursor)?);
                    let component = u16_at(&program, cursor + 2)? as usize;
                    if component >= count {
                        return Err(Error::value("Invalid TrueType composite glyph index")
                            .with_issue(crate::core::recovery::Issue::Font));
                    }
                    let new = if let Some(&new) = mapped.get(&component) {
                        new
                    } else {
                        let new = order.len();
                        mapped.insert(component, new);
                        order.push(component);
                        new
                    };
                    put16(
                        &mut program,
                        cursor + 2,
                        u16::try_from(new).map_err(|_| {
                            Error::value("Too many TrueType subset glyphs")
                                .with_issue(crate::core::recovery::Issue::Font)
                        })?,
                    )?;
                    cursor += 4 + flags.argument_bytes() + flags.transform_bytes();
                    if cursor > program.len() {
                        return Err(Error::value("Truncated TrueType composite glyph")
                            .with_issue(crate::core::recovery::Issue::Font));
                    }
                    if flags.0 & CompositeGlyphFlag::MORE_COMPONENTS == 0 {
                        break;
                    }
                }
            }
            glyf.extend(program);
        }
        new_offsets.push(glyf.len() as u32);
        tables.insert(*b"glyf", glyf);
        let mut new_loca = Vec::new();
        words32(&mut new_loca, new_offsets);
        tables.insert(*b"loca", new_loca);
        put16(tables.get_mut(b"head").unwrap(), 50, 1)?;
        let metric_count = u16_at(table(&tables, b"hhea")?, 34)? as usize;
        if !(1..=count).contains(&metric_count) {
            return Err(Error::value("Invalid TrueType hmetrics count")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        let hmtx = table(&tables, b"hmtx")?;
        let mut metrics = Vec::new();
        for index in 0..metric_count {
            metrics.push((u16_at(hmtx, index * 4)?, i16_at(hmtx, index * 4 + 2)?));
        }
        for index in 0..count - metric_count {
            metrics.push((
                metrics[metric_count - 1].0,
                i16_at(hmtx, metric_count * 4 + 2 * index)?,
            ));
        }
        let mut new_hmtx = Vec::new();
        for &index in &order {
            words16(&mut new_hmtx, [metrics[index].0, metrics[index].1 as u16]);
        }
        tables.insert(*b"hmtx", new_hmtx);
        let new_count = u16::try_from(order.len()).map_err(|_| {
            Error::value("Too many TrueType subset glyphs")
                .with_issue(crate::core::recovery::Issue::Font)
        })?;
        put16(tables.get_mut(b"hhea").unwrap(), 34, new_count)?;
        put16(tables.get_mut(b"maxp").unwrap(), 4, new_count)?;
        tables.insert(*b"cmap", byte_cmap(&indices, order.len()));
        let mut os2 = table(&tables, b"OS/2")?
            .get(..78)
            .ok_or_else(|| {
                Error::value("Truncated TrueType OS/2 table")
                    .with_issue(crate::core::recovery::Issue::Font)
            })?
            .to_vec();
        put16(&mut os2, 0, 1)?;
        for (index, value) in [3, 0x10000000, 0, 0].into_iter().enumerate() {
            put32(&mut os2, 42 + index * 4, value)?;
        }
        put16(&mut os2, 64, 0)?;
        put16(&mut os2, 66, 0xf0ff)?;
        words32(&mut os2, [0x80000000, 0]);
        tables.insert(*b"OS/2", os2);
        let mut hash = Sha256::new();
        hash.update(&self.data);
        hash.update(codes.iter().copied().collect::<Vec<_>>());
        let tag: String = hash.finalize()[..6]
            .iter()
            .map(|byte| char::from(65 + byte % 26))
            .collect();
        let name = format!("{tag}+{}", self.name);
        let names = table(&tables, b"name")?;
        let format = u16_at(names, 0)?;
        let number = u16_at(names, 2)?;
        let base = u16_at(names, 4)? as usize;
        let mut records = Vec::new();
        let mut strings = Vec::new();
        for index in 0..number as usize {
            let position = 6 + 12 * index;
            let pid = u16_at(names, position)?;
            let eid = u16_at(names, position + 2)?;
            let lang = u16_at(names, position + 4)?;
            let key = u16_at(names, position + 6)?;
            let length = u16_at(names, position + 8)? as usize;
            let offset = u16_at(names, position + 10)? as usize;
            let start = base + offset;
            let original = &names[start.min(names.len())..(start + length).min(names.len())];
            let mut value = Vec::new();
            if [1, 3, 4, 6].contains(&key) {
                let prefix = format!("{tag}+");
                if pid == 3 {
                    for word in prefix.encode_utf16() {
                        value.extend_from_slice(&word.to_be_bytes());
                    }
                } else {
                    value.extend_from_slice(prefix.as_bytes());
                }
            }
            value.extend_from_slice(original);
            words16(
                &mut records,
                [
                    pid,
                    eid,
                    lang,
                    key,
                    u16::try_from(value.len()).map_err(|_| {
                        Error::value("TrueType name too long")
                            .with_issue(crate::core::recovery::Issue::Font)
                    })?,
                    u16::try_from(strings.len()).map_err(|_| {
                        Error::value("TrueType names too long")
                            .with_issue(crate::core::recovery::Issue::Font)
                    })?,
                ],
            );
            strings.extend(value);
        }
        let mut names = Vec::new();
        words16(&mut names, [format, number, 6 + 12 * number]);
        names.extend(records);
        names.extend(strings);
        tables.insert(*b"name", names);
        let post = table(&self.tables, b"post")?;
        if u32_at(post, 0)? == 0x30000 {
            tables.insert(*b"post", post[..32.min(post.len())].to_vec());
        } else {
            let mac_indices: BTreeMap<&str, usize> = MAC_NAMES
                .iter()
                .enumerate()
                .map(|(index, &name)| (name, index))
                .collect();
            let mut custom_indices = BTreeMap::new();
            let mut custom: Vec<&str> = Vec::new();
            let mut glyph_indices = Vec::new();
            for &old in &order {
                let glyph_name = self.glyph_names[old].as_str();
                let index = if let Some(&index) = mac_indices.get(glyph_name) {
                    index
                } else {
                    let custom_index = if let Some(&index) = custom_indices.get(glyph_name) {
                        index
                    } else {
                        let index = custom.len();
                        custom.push(glyph_name);
                        custom_indices.insert(glyph_name, index);
                        index
                    };
                    MAC_NAMES.len() + custom_index
                };
                glyph_indices.push(u16::try_from(index).map_err(|_| {
                    Error::value("Too many TrueType post names")
                        .with_issue(crate::core::recovery::Issue::Font)
                })?);
            }
            let mut new_post = Vec::new();
            words32(&mut new_post, [0x20000]);
            new_post.extend_from_slice(bytes(post, 4, 28)?);
            words16(&mut new_post, [new_count]);
            words16(&mut new_post, glyph_indices);
            for name in custom {
                let raw: Vec<u8> = name
                    .chars()
                    .map(|ch| u8::try_from(ch as u32).expect("post names are Latin1"))
                    .collect();
                new_post.push(u8::try_from(raw.len()).map_err(|_| {
                    Error::value("TrueType post name too long")
                        .with_issue(crate::core::recovery::Issue::Font)
                })?);
                new_post.extend(raw);
            }
            tables.insert(*b"post", new_post);
        }
        let allowed = [
            *b"PCLT", *b"fpgm", *b"cvt ", *b"prep", *b"glyf", *b"loca", *b"OS/2", *b"head",
            *b"hhea", *b"hmtx", *b"maxp", *b"name", *b"post", *b"cmap",
        ];
        tables.retain(|tag, _| allowed.contains(tag));
        let data = assemble(&tables)?;
        let mut resource = self.clone();
        resource.length1 = data.len();
        resource.data = data;
        resource.name = name;
        Ok((resource, order))
    }
}
