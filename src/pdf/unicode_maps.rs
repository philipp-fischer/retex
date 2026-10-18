//! Glyph Unicode definitions and byte-font ToUnicode CMaps (`unicode_maps.py`).
use crate::core::errors::{Error, Result};
use crate::core::persistence::PersistentComponent;
use regex::Regex;
use std::collections::BTreeMap;

pub fn utf16(code: u32) -> Result<String> {
    if code > 0x10ffff {
        return Err(Error::value("Unicode code point out of range"));
    }
    if code <= 0xffff {
        Ok(format!("{code:04X}"))
    } else {
        let code = code - 0x10000;
        Ok(format!(
            "{:04X}{:04X}",
            0xd800 + (code >> 10),
            0xdc00 + (code & 1023)
        ))
    }
}
#[derive(Clone, Debug, Default)]
pub struct UnicodeMaps {
    pub entries: BTreeMap<String, Option<String>>,
}
impl UnicodeMaps {
    pub fn define(&mut self, glyph: &str, value: &str, warn: &mut dyn FnMut(&str)) {
        let glyph = glyph.split('\0').next().unwrap_or("");
        let value = value.split('\0').next().unwrap_or("").trim_matches(' ');
        if glyph.is_empty()
            || glyph == ".notdef"
            || !Regex::new(r"^[0-9A-F ]+$")
                .expect("ToUnicode hex")
                .is_match(value)
        {
            warn(&format!(
                "ToUnicode: invalid parameter(s): {} => {}",
                python_repr(glyph),
                python_repr(value)
            ));
            return;
        }
        let result = if value.contains(' ') {
            Some(value.replace(' ', ""))
        } else {
            let code = u32::from_str_radix(value, 16)
                .ok()
                .filter(|&code| code <= 0x10ffff);
            if let Some(code) = code {
                utf16(code).ok()
            } else {
                warn(&format!(
                    "ToUnicode: value out of range [0,10FFFF]: {value}"
                ));
                None
            }
        };
        self.entries.insert(glyph.into(), result);
    }
    pub fn lookup(&self, glyph: &str, tfm: &str) -> Option<String> {
        if glyph.is_empty() || glyph == ".notdef" {
            return None;
        }
        let glyph = glyph.split('.').next().unwrap_or("");
        if glyph.is_empty() {
            return None;
        }
        if glyph.contains('_') {
            return Some(
                glyph
                    .split('_')
                    .map(|part| self.lookup(part, tfm).unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join(""),
            );
        }
        for key in [format!("tfm:{tfm}/{glyph}"), glyph.into()] {
            if let Some(value) = self.entries.get(&key) {
                return value.clone();
            }
        }
        if Regex::new(r"^uni(?:[0-9A-F]{4})+$")
            .expect("uni glyph name")
            .is_match(glyph)
        {
            let digits = &glyph[3..];
            if digits.as_bytes().chunks_exact(4).all(|word| {
                let value = u32::from_str_radix(std::str::from_utf8(word).expect("ASCII hex"), 16)
                    .expect("hex quartet");
                !(0xd800..=0xdfff).contains(&value)
            }) {
                return Some(digits.into());
            }
        } else if Regex::new(r"^u[0-9A-F]{4,6}$")
            .expect("u glyph name")
            .is_match(glyph)
        {
            let code = u32::from_str_radix(&glyph[1..], 16).expect("Unicode glyph hex");
            if code <= 0x10ffff && !(0xd800..=0xdfff).contains(&code) {
                return utf16(code).ok();
            }
        }
        None
    }
    pub fn cmap(
        &self,
        names: &[String],
        tfm: &str,
        enabled: bool,
        no_builtin: bool,
        warn: &mut dyn FnMut(&str),
    ) -> Option<Vec<u8>> {
        if no_builtin || !enabled {
            return None;
        }
        if self.entries.is_empty() {
            warn("No GlyphToUnicode entry has been inserted yet");
            return None;
        }
        let pairs: Vec<_> = names
            .iter()
            .enumerate()
            .filter_map(|(code, name)| {
                self.lookup(name, tfm)
                    .filter(|value| !value.is_empty())
                    .map(|value| (code, value))
            })
            .collect();
        if pairs.is_empty() {
            return None;
        }
        let mut lines: Vec<String> = [
            "/CIDInit /ProcSet findresource begin",
            "12 dict begin",
            "begincmap",
            "/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def",
            "/CMapName /TeX-UCS def",
            "/CMapType 2 def",
            "1 begincodespacerange",
            "<00> <FF>",
            "endcodespacerange",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        for chunk in pairs.chunks(100) {
            lines.push(format!("{} beginbfchar", chunk.len()));
            lines.extend(
                chunk
                    .iter()
                    .map(|(code, value)| format!("<{code:02X}> <{value}>")),
            );
            lines.push("endbfchar".into());
        }
        lines.extend(
            [
                "endcmap",
                "CMapName currentdict /CMap defineresource pop",
                "end",
                "end",
            ]
            .into_iter()
            .map(str::to_string),
        );
        Some((lines.join("\n") + "\n").into_bytes())
    }
}
impl PersistentComponent for UnicodeMaps {
    type State = BTreeMap<String, Option<String>>;
    fn dump_state(&self) -> Self::State {
        self.entries.clone()
    }
    fn restore_state(&mut self, state: Self::State) -> Result<()> {
        self.entries = state;
        Ok(())
    }
}
fn python_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut result = quote.to_string();
    for character in text.chars() {
        match character {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            character if character == quote => {
                result.push('\\');
                result.push(character);
            }
            character if character.is_control() => {
                result.push_str(&format!("\\x{:02x}", character as u32))
            }
            character => result.push(character),
        }
    }
    result.push(quote);
    result
}
