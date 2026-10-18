//! Classic SFD byte-to-Unicode subfonts (`fonts/subfonts.py`).
use crate::core::errors::{Error, Result};
use regex::Regex;
use std::path::Path;

pub fn integer(text: &str) -> Result<i64> {
    let sign = if text.starts_with('-') { -1 } else { 1 };
    let text = text.trim_start_matches(['+', '-']);
    let (digits, radix) = if text.to_ascii_lowercase().starts_with("0x") {
        (&text[2..], 16)
    } else if text.len() > 1 && text.starts_with('0') {
        (text, 8)
    } else {
        (text, 10)
    };
    i64::from_str_radix(digits, radix)
        .map(|v| v * sign)
        .map_err(|_| Error::value("Invalid SFD integer"))
}
pub fn read_definitions(path: impl AsRef<Path>) -> Result<Vec<(String, Vec<i32>)>> {
    let bytes = crate::resources::host::read(path)?;
    parse_definitions(&bytes.iter().map(|&b| char::from(b)).collect::<String>())
}
pub fn parse_definitions(text: &str) -> Result<Vec<(String, Vec<i32>)>> {
    let mut records = Vec::new();
    let mut pending = String::new();
    for line in text
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
    {
        if let Some((before, after)) = line.split_once('\\') {
            if !after.trim().is_empty() {
                return Err(Error::value("Invalid SFD continuation"));
            }
            pending.push_str(before);
            pending.push(' ');
            continue;
        }
        records.push(format!("{pending}{line}"));
        pending.clear();
    }
    if !pending.is_empty() {
        return Err(Error::value("Unexpected EOF in SFD continuation"));
    }
    let token = Regex::new(
        r"^\s*([+-]?(?:0[xX][\da-fA-F]+|\d+))(?:\s*(:)|\s*_\s*([+-]?(?:0[xX][\da-fA-F]+|\d+)))?",
    )
    .expect("SFD grammar");
    let mut result = Vec::new();
    for line in records {
        let trimmed = line.trim_start();
        let at = trimmed
            .find(char::is_whitespace)
            .ok_or_else(|| Error::value("Invalid SFD subfont definition"))?;
        let infix = trimmed[..at].to_string();
        let body = trimmed[at..].trim_start();
        if body.is_empty() {
            return Err(Error::value("Invalid SFD subfont definition"));
        }
        let (mut cursor, mut offset) = (0, 0_i64);
        let mut mapping = vec![-1; 256];
        while cursor < body.len() && !body[cursor..].trim().is_empty() {
            let matched = token
                .captures(&body[cursor..])
                .ok_or_else(|| Error::value("Invalid SFD token"))?;
            cursor += matched.get(0).expect("complete SFD match").end();
            let first = integer(&matched[1])?;
            if matched.get(2).is_some() {
                if !(0..=255).contains(&first) {
                    return Err(Error::value("Invalid SFD offset"));
                }
                offset = first;
                continue;
            }
            let last = matched.get(3).map_or(Ok(first), |m| integer(m.as_str()))?;
            if first < 0 || last < first || last >= 65536 || offset + last - first > 255 {
                return Err(Error::value("Invalid SFD range"));
            }
            for code in first..=last {
                mapping[offset as usize] = code as i32;
                offset += 1;
            }
        }
        result.push((infix, mapping));
    }
    Ok(result)
}
