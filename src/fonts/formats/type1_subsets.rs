//! Type1 dependency closure and sparse binary subsetting (`type1_subsets.py`).
use super::type1::{PS_NAME, STANDARD_ENCODING, Type1Resource};
use super::type1_cipher::{CHARSTRING_CIPHER_SEED, EEXEC_CIPHER_SEED, cipher};
use crate::core::errors::{Error, Result};
use crate::pdf::writer::latin1;
use regex::{Regex, bytes::Regex as ByteRegex};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

pub const ESCAPED_OPERATOR_OFFSET: u16 = 32;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum CharStringOperator {
    Hstem = 1,
    Vstem = 3,
    Vmoveto = 4,
    Rlineto = 5,
    Hlineto = 6,
    Vlineto = 7,
    Rrcurveto = 8,
    Closepath = 9,
    Callsubr = 10,
    Return = 11,
    Escape = 12,
    Hsbw = 13,
    Endchar = 14,
    Rmoveto = 21,
    Hmoveto = 22,
    Vhcurveto = 30,
    Hvcurveto = 31,
    Dotsection = 32,
    Vstem3 = 33,
    Hstem3 = 34,
    Seac = 38,
    Sbw = 39,
    Div = 44,
    Callothersubr = 48,
    Pop = 49,
    Setcurrentpoint = 65,
}
pub const RETURN_INSTRUCTION: &[u8] = &[11];
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryKey {
    Glyph(String),
    Subr(i64),
}
#[derive(Clone, Debug)]
pub struct Entry {
    pub key: EntryKey,
    pub start: usize,
    pub end: usize,
    pub payload_start: usize,
    pub payload_end: usize,
    pub size_start: usize,
    pub size_end: usize,
}
#[derive(Clone, Debug)]
struct DictionaryHeader {
    start: usize,
    end: usize,
    size: Range<usize>,
}
pub struct Type1Program {
    pub resource: Type1Resource,
    pub private: Vec<u8>,
    pub iv: usize,
    pub glyphs: BTreeMap<String, Entry>,
    pub subrs: BTreeMap<i64, Entry>,
    char_header: DictionaryHeader,
    subr_header: Option<DictionaryHeader>,
}
pub struct Closure {
    pub glyphs: BTreeSet<String>,
    pub subrs: BTreeSet<i64>,
    pub repairs: BTreeMap<i64, Vec<u8>>,
}
struct Frame {
    key: EntryKey,
    data: Vec<u8>,
    cursor: usize,
    last: Option<u16>,
}
impl Type1Program {
    pub fn new(resource: Type1Resource) -> Result<Self> {
        let encrypted = resource
            .data
            .get(resource.length1..resource.length1 + resource.length2)
            .ok_or_else(|| {
                Error::value("Invalid Type1 sections")
                    .with_issue(crate::core::recovery::Issue::Font)
            })?;
        let private = cipher(encrypted, EEXEC_CIPHER_SEED, false);
        let chars = ByteRegex::new(r"(?-u)/CharStrings\s+(\d+)\s+dict(?:\s+dup)?\s+begin")
            .expect("CharStrings grammar");
        let found = chars.captures(&private).ok_or_else(|| {
            Error::value("Invalid Type1 CharStrings dictionary")
                .with_issue(crate::core::recovery::Issue::Font)
        })?;
        let char_header = DictionaryHeader {
            start: found.get(0).expect("dictionary match").start(),
            end: found.get(0).expect("dictionary match").end(),
            size: found.get(1).expect("dictionary size").range(),
        };
        let subs = ByteRegex::new(r"(?-u)/Subrs\s+(\d+)\s+array").expect("Subrs grammar");
        let subr_header =
            subs.captures(&private[..char_header.start])
                .map(|found| DictionaryHeader {
                    start: found.get(0).expect("subr match").start(),
                    end: found.get(0).expect("subr match").end(),
                    size: found.get(1).expect("subr size").range(),
                });
        let first = subr_header
            .as_ref()
            .map_or(char_header.start, |header| header.start);
        let iv = ByteRegex::new(r"(?-u)/lenIV\s+(-?\d+)\b")
            .expect("lenIV grammar")
            .captures(&private[..first]);
        let iv = iv.map_or(Ok(4_i64), |found| {
            parse_integer(found.get(1).expect("lenIV value").as_bytes())
        })?;
        if iv < 0 {
            return Err(
                Error::value("Invalid Type1 lenIV: target requires a nonnegative value")
                    .with_issue(crate::core::recovery::Issue::Font),
            );
        }
        let mut program = Self {
            resource,
            private,
            iv: iv as usize,
            glyphs: BTreeMap::new(),
            subrs: BTreeMap::new(),
            char_header,
            subr_header,
        };
        for (key, entry) in
            program.entries(program.char_header.end, program.private.len(), false)?
        {
            if let EntryKey::Glyph(name) = key {
                program.glyphs.insert(name, entry);
            }
        }
        if let Some(header) = &program.subr_header {
            for (key, entry) in program.entries(header.end, program.char_header.start, true)? {
                if let EntryKey::Subr(number) = key {
                    program.subrs.insert(number, entry);
                }
            }
        }
        Ok(program)
    }
    pub fn entries(
        &self,
        mut start: usize,
        end: usize,
        subrs: bool,
    ) -> Result<BTreeMap<EntryKey, Entry>> {
        let key = if subrs {
            r"dup\s+(\d+)"
        } else {
            r"/([^\s/()<>\[\]{}%]+)"
        };
        let pattern = ByteRegex::new(&format!(
            r"(?-u){key}\s+(\d+)\s+[^\s/()<>\[\]{{}}%]+[ \t\r\n]"
        ))
        .expect("binary Type1 entry");
        let suffix = ByteRegex::new(r"(?-u)^\s*(?:noaccess\s+(?:put|def)|[^\s/()<>\[\]{}%]+)")
            .expect("binary Type1 suffix");
        let mut result = BTreeMap::new();
        while start < end {
            let Some(matched) = pattern.captures(&self.private[start..end]) else {
                break;
            };
            let whole = matched.get(0).expect("binary entry match");
            let payload_start = start + whole.end();
            let size = parse_integer(matched.get(2).expect("binary entry size").as_bytes())?;
            let payload_end = payload_start
                .checked_add(size as usize)
                .filter(|&n| n <= end)
                .ok_or_else(|| {
                    Error::value("Invalid Type1 binary entry")
                        .with_issue(crate::core::recovery::Issue::Font)
                })?;
            let tail = suffix
                .find(&self.private[payload_end..end])
                .ok_or_else(|| {
                    Error::value("Invalid Type1 binary entry")
                        .with_issue(crate::core::recovery::Issue::Font)
                })?;
            let name = matched.get(1).expect("binary entry key").as_bytes();
            let key = if subrs {
                EntryKey::Subr(parse_integer(name)?)
            } else {
                EntryKey::Glyph(name.iter().map(|&b| char::from(b)).collect())
            };
            let entry = Entry {
                key: key.clone(),
                start: start + whole.start(),
                end: payload_end + tail.end(),
                payload_start,
                payload_end,
                size_start: start + matched.get(2).expect("size match").start(),
                size_end: start + matched.get(2).expect("size match").end(),
            };
            start = entry.end;
            result.insert(key, entry);
        }
        Ok(result)
    }
    pub fn bytecode(&self, entry: &Entry) -> Result<Vec<u8>> {
        let data = cipher(
            &self.private[entry.payload_start..entry.payload_end],
            CHARSTRING_CIPHER_SEED,
            false,
        );
        if data.len() < self.iv {
            return Err(Error::value("Invalid Type1 encrypted charstring")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        Ok(data[self.iv..].to_vec())
    }
    fn begin(
        &self,
        key: EntryKey,
        frames: &mut Vec<Frame>,
        closure: &mut Closure,
        warn: &mut dyn FnMut(&str),
    ) -> Result<()> {
        let entry = match &key {
            EntryKey::Glyph(name) => self.glyphs.get(name),
            EntryKey::Subr(number) => self.subrs.get(number),
        };
        let Some(entry) = entry else {
            match key {
                EntryKey::Glyph(name) => {
                    warn(&format!("glyph `{name}' undefined"));
                    return Ok(());
                }
                EntryKey::Subr(number) => {
                    return Err(Error::value(format!("Invalid Type1 Subr {number}")));
                }
            }
        };
        match &key {
            EntryKey::Glyph(name) => {
                if !closure.glyphs.insert(name.clone()) {
                    return Ok(());
                }
            }
            EntryKey::Subr(number) => {
                if frames.iter().any(|frame| frame.key == key) {
                    return Err(Error::value("Invalid Type1 recursive Subr")
                        .with_issue(crate::core::recovery::Issue::Font));
                }
                closure.subrs.insert(*number);
            }
        }
        frames.push(Frame {
            key,
            data: self.bytecode(entry)?,
            cursor: 0,
            last: None,
        });
        Ok(())
    }
    pub fn closure(
        &self,
        names: impl IntoIterator<Item = String>,
        warn: &mut dyn FnMut(&str),
    ) -> Result<Closure> {
        let mut closure = Closure {
            glyphs: BTreeSet::new(),
            subrs: (0..4).filter(|key| self.subrs.contains_key(key)).collect(),
            repairs: BTreeMap::new(),
        };
        let mut operands = Vec::<i64>::new();
        let mut last_other = 3;
        let mut frames = Vec::<Frame>::new();
        let names: BTreeSet<_> = names.into_iter().collect();
        for name in std::iter::once(".notdef".to_string()).chain(names) {
            self.begin(EntryKey::Glyph(name), &mut frames, &mut closure, warn)?;
            while let Some(frame) = frames.last_mut() {
                if frame.cursor == frame.data.len() {
                    let frame = frames.pop().expect("active frame");
                    if let EntryKey::Subr(number) = frame.key {
                        if frame.last != Some(11) {
                            warn(&format!(
                                "Last command in subr {number} is not a RETURN; adding it"
                            ));
                            let mut data = frame.data;
                            data.extend(RETURN_INSTRUCTION);
                            closure.repairs.insert(number, data);
                        }
                    }
                    continue;
                }
                let byte = frame.data[frame.cursor];
                frame.cursor += 1;
                if byte >= 32 {
                    let value = if byte <= 246 {
                        byte as i64 - 139
                    } else if byte <= 254 {
                        if frame.cursor == frame.data.len() {
                            return Err(Error::value("Truncated Type1 operand")
                                .with_issue(crate::core::recovery::Issue::Font));
                        }
                        let next = frame.data[frame.cursor] as i64;
                        frame.cursor += 1;
                        if byte <= 250 {
                            (byte as i64 - 247) * 256 + 108 + next
                        } else {
                            -((byte as i64 - 251) * 256 + 108 + next)
                        }
                    } else {
                        let data =
                            frame
                                .data
                                .get(frame.cursor..frame.cursor + 4)
                                .ok_or_else(|| {
                                    Error::value("Truncated Type1 operand")
                                        .with_issue(crate::core::recovery::Issue::Font)
                                })?;
                        frame.cursor += 4;
                        i32::from_be_bytes(data.try_into().expect("four-byte Type1 operand")) as i64
                    };
                    if operands.len() == 24 {
                        return Err(Error::value("Invalid Type1 operand stack overflow")
                            .with_issue(crate::core::recovery::Issue::Font));
                    }
                    operands.push(value);
                    continue;
                }
                let opcode = if byte == 12 {
                    if frame.cursor == frame.data.len() {
                        return Err(Error::value("Truncated Type1 operator")
                            .with_issue(crate::core::recovery::Issue::Font));
                    }
                    let opcode = 32 + frame.data[frame.cursor] as u16;
                    frame.cursor += 1;
                    opcode
                } else {
                    byte as u16
                };
                frame.last = Some(opcode);
                match opcode {
                    10 => {
                        let number = pop(&mut operands, 1)?[0];
                        self.begin(EntryKey::Subr(number), &mut frames, &mut closure, warn)?;
                    }
                    11 => {}
                    44 => {
                        pop(&mut operands, 2)?;
                        operands.push(0);
                    }
                    48 => {
                        if operands.len() < 2 {
                            return Err(Error::value("Invalid Type1 OtherSubr")
                                .with_issue(crate::core::recovery::Issue::Font));
                        }
                        if operands[operands.len() - 1] == 3 {
                            if operands.len() < 3 {
                                return Err(Error::value("Invalid Type1 OtherSubr")
                                    .with_issue(crate::core::recovery::Issue::Font));
                            }
                            last_other = operands[operands.len() - 3];
                        }
                        let count = operands[operands.len() - 2] + 2;
                        pop(&mut operands, count)?;
                    }
                    49 => operands.push(last_other),
                    38 => {
                        if operands.len() != 5 {
                            return Err(Error::value("Invalid Type1 seac operands")
                                .with_issue(crate::core::recovery::Issue::Font));
                        }
                        let (base, accent) = (operands[3], operands[4]);
                        operands.clear();
                        if !(0..256).contains(&base) || !(0..256).contains(&accent) {
                            return Err(Error::value("Invalid Type1 seac character")
                                .with_issue(crate::core::recovery::Issue::Font));
                        }
                        self.begin(
                            EntryKey::Glyph(STANDARD_ENCODING[accent as usize].into()),
                            &mut frames,
                            &mut closure,
                            warn,
                        )?;
                        self.begin(
                            EntryKey::Glyph(STANDARD_ENCODING[base as usize].into()),
                            &mut frames,
                            &mut closure,
                            warn,
                        )?;
                    }
                    1 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 13 | 14 | 21 | 22 | 30 | 31 | 32 | 33 | 34
                    | 39 | 65 => {
                        let count = match opcode {
                            1 | 3 | 5 | 13 | 21 | 65 => Some(2),
                            4 | 6 | 7 | 22 => Some(1),
                            8 | 33 | 34 => Some(6),
                            30 | 31 | 39 => Some(4),
                            _ => None,
                        };
                        if count.is_some_and(|n| operands.len() != n) {
                            return Err(Error::value("Invalid Type1 operator operands")
                                .with_issue(crate::core::recovery::Issue::Font));
                        }
                        operands.clear();
                    }
                    _ => return Err(Error::value(format!("Invalid Type1 operator {opcode}"))),
                }
            }
        }
        Ok(closure)
    }
    pub fn subset(
        &self,
        names: impl IntoIterator<Item = String>,
        warn: &mut dyn FnMut(&str),
    ) -> Result<(Type1Resource, Vec<String>)> {
        let closure = self.closure(names, warn)?;
        let count = closure.subrs.last().copied().unwrap_or(-1) + 1;
        let mut patches = vec![(
            self.char_header.size.start,
            self.char_header.size.end,
            closure.glyphs.len().to_string().into_bytes(),
        )];
        if let Some(header) = &self.subr_header {
            patches.push((
                header.size.start,
                header.size.end,
                count.to_string().into_bytes(),
            ));
        }
        for (key, entry) in &self.glyphs {
            if !closure.glyphs.contains(key) {
                patches.push((entry.start, entry.end, Vec::new()));
            }
        }
        for (&key, entry) in &self.subrs {
            if key >= count {
                patches.push((entry.start, entry.end, Vec::new()));
            } else if !closure.subrs.contains(&key) || closure.repairs.contains_key(&key) {
                let mut data = vec![0; self.iv];
                data.extend(
                    closure
                        .repairs
                        .get(&key)
                        .map_or(RETURN_INSTRUCTION, Vec::as_slice),
                );
                let data = cipher(&data, CHARSTRING_CIPHER_SEED, true);
                patches.push((
                    entry.size_start,
                    entry.size_end,
                    data.len().to_string().into_bytes(),
                ));
                patches.push((entry.payload_start, entry.payload_end, data));
            }
        }
        patches.sort();
        let mut private = Vec::new();
        let mut cursor = 0;
        for (start, end, replacement) in patches {
            if start < cursor || end < start || end > self.private.len() {
                return Err(Error::value("Invalid overlapping Type1 subset patches")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            private.extend(&self.private[cursor..start]);
            private.extend(replacement);
            cursor = end;
        }
        private.extend(&self.private[cursor..]);
        let private = cipher(&private, EEXEC_CIPHER_SEED, true);
        let mut digest = Sha256::new();
        digest.update(&self.resource.data);
        digest.update(latin1(
            &closure.glyphs.iter().cloned().collect::<Vec<_>>().join(" "),
        )?);
        let signature = digest.finalize();
        let tag: String = signature[..6]
            .iter()
            .map(|b| char::from(65 + b % 26))
            .collect();
        let name = format!("{tag}+{}", self.resource.name);
        let header: String = self.resource.data[..self.resource.length1]
            .iter()
            .map(|&b| char::from(b))
            .collect();
        let replacement = format!("/FontName /{name}");
        let header = Regex::new(&format!(r"/FontName\s+/({PS_NAME})"))
            .expect("subset FontName")
            .replacen(&header, 1, regex::NoExpand(&replacement))
            .into_owned();
        let mut first_codes = BTreeMap::new();
        for (code, glyph) in self.resource.encoding.iter().enumerate() {
            first_codes.entry(glyph.clone()).or_insert(code);
        }
        let encoding: Vec<_> = self
            .resource
            .encoding
            .iter()
            .enumerate()
            .map(|(code, glyph)| {
                if closure.glyphs.contains(glyph) && first_codes[glyph] == code {
                    glyph.clone()
                } else {
                    ".notdef".into()
                }
            })
            .collect();
        let mut program =
            "/Encoding 256 array\n0 1 255 {1 index exch /.notdef put} for\n".to_string();
        for (code, glyph) in encoding.iter().enumerate() {
            if glyph != ".notdef" {
                program.push_str(&format!("dup {code} /{glyph} put\n"));
            }
        }
        program.push_str("readonly def");
        let header = Regex::new(r"(?s)/Encoding\s*.*?\bdef\b")
            .expect("subset encoding")
            .replacen(&header, 1, regex::NoExpand(&program))
            .into_owned();
        let header = latin1(&header)?;
        let mut resource = self.resource.clone();
        resource.length1 = header.len();
        resource.length2 = private.len();
        resource.name = name;
        resource.encoding = encoding;
        let mut data = header;
        data.extend(private);
        data.extend(&self.resource.data[self.resource.length1 + self.resource.length2..]);
        resource.data = data;
        Ok((resource, closure.glyphs.into_iter().collect()))
    }
}
fn parse_integer(bytes: &[u8]) -> Result<i64> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| {
            Error::value("Invalid Type1 integer").with_issue(crate::core::recovery::Issue::Font)
        })
}
fn pop(operands: &mut Vec<i64>, count: i64) -> Result<Vec<i64>> {
    if count < 0 || count as usize > operands.len() {
        return Err(Error::value("Invalid Type1 operand stack")
            .with_issue(crate::core::recovery::Issue::Font));
    }
    Ok(operands.split_off(operands.len() - count as usize))
}
