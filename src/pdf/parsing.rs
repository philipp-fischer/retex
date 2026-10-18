//! PDF grammar for source dictionaries. Values retain octet strings and references.
use crate::core::errors::{Error, Result};
use crate::pdf::{
    syntax::{character_string, pdf_name},
    writer::latin1,
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PdfValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    Name(String),
    String(Vec<u8>),
    Array(Vec<Self>),
    Dictionary(Vec<(String, Self)>),
    Reference(u32, u16),
    Stream {
        dictionary: Vec<(String, Self)>,
        data: Vec<u8>,
    },
}
impl PdfValue {
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Dictionary(d) | Self::Stream { dictionary: d, .. } => d
                .iter()
                .find(|(k, _)| k == key.trim_start_matches('/'))
                .map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn entries(&self) -> Result<&[(String, Self)]> {
        match self {
            Self::Dictionary(d) | Self::Stream { dictionary: d, .. } => Ok(d),
            _ => Err(Error::value("Expected PDF dictionary")),
        }
    }
    pub fn number(&self) -> Result<f64> {
        match self {
            Self::Integer(n) => Ok(*n as f64),
            Self::Real(n) => Ok(*n),
            _ => Err(Error::value("Expected PDF number")),
        }
    }
    pub fn serialize(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.write(&mut out)?;
        Ok(out)
    }
    fn write(&self, out: &mut Vec<u8>) -> Result<()> {
        match self {
            Self::Null => out.extend(b"null"),
            Self::Boolean(n) => out.extend(if *n { b"true".as_slice() } else { b"false" }),
            Self::Integer(n) => out.extend(n.to_string().as_bytes()),
            Self::Real(n) => out.extend(n.to_string().as_bytes()),
            Self::Name(n) => out.extend(latin1(&format!("/{}", pdf_name(n)?))?),
            Self::String(s) => out.extend(latin1(&octet_string(s))?),
            Self::Reference(n, g) => out.extend(format!("{n} {g} R").as_bytes()),
            Self::Array(items) => {
                out.push(b'[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(b' ')
                    }
                    item.write(out)?
                }
                out.push(b']')
            }
            Self::Dictionary(d) => write_dictionary(d, out)?,
            Self::Stream { dictionary, data } => {
                let mut d = dictionary.clone();
                d.retain(|(k, _)| k != "Length");
                d.push(("Length".into(), Self::Integer(data.len() as i64)));
                write_dictionary(&d, out)?;
                out.extend(b"\nstream\n");
                out.extend(data);
                out.extend(b"\nendstream");
            }
        }
        Ok(())
    }
}
fn write_dictionary(d: &[(String, PdfValue)], out: &mut Vec<u8>) -> Result<()> {
    out.extend(b"<<");
    for (k, v) in d {
        out.extend(latin1(&format!(" /{} ", pdf_name(k)?))?);
        v.write(out)?
    }
    out.extend(b" >>");
    Ok(())
}
pub fn octet_string(bytes: &[u8]) -> String {
    format!(
        "({})",
        bytes
            .iter()
            .map(|&c| character_string(c as u32))
            .collect::<String>()
    )
}
pub fn string(text: &str) -> Result<String> {
    Ok(octet_string(&latin1(text)?))
}
pub fn source_string(text: &str) -> String {
    if text.starts_with('(') && text.ends_with(')')
        || text.starts_with('<')
            && text.ends_with('>')
            && text.len() % 2 == 0
            && text[1..text.len() - 1]
                .bytes()
                .all(|c| c.is_ascii_hexdigit())
    {
        text.into()
    } else {
        format!("({text})")
    }
}
pub fn dictionary(text: &str) -> Result<PdfValue> {
    parse(&latin1(&format!("<< {text} >>"))?)
}
pub fn parse(bytes: &[u8]) -> Result<PdfValue> {
    let mut p = Parser { bytes, position: 0 };
    let result = p.value()?;
    p.space();
    if p.position != bytes.len() {
        return Err(Error::value("Trailing PDF syntax"));
    }
    Ok(result)
}
/// Read an emitted indirect-object body, including a length-delimited stream.
/// Dictionary syntax is parsed before examining the stream keyword, so strings
/// containing that word cannot alter the binary-data boundary.
pub fn parse_indirect_body(bytes: &[u8]) -> Result<PdfValue> {
    let mut parser = Parser { bytes, position: 0 };
    let value = parser.value()?;
    parser.space();
    if parser.position == bytes.len() {
        return Ok(value);
    }
    let PdfValue::Dictionary(dictionary) = value else {
        return Err(Error::value("Trailing PDF object syntax"));
    };
    if !bytes[parser.position..].starts_with(b"stream") {
        return Err(Error::value("Trailing PDF object syntax"));
    }
    parser.position += 6;
    while matches!(bytes.get(parser.position), Some(b' ' | b'\t')) {
        parser.position += 1;
    }
    match bytes.get(parser.position) {
        Some(b'\r') => {
            parser.position += 1;
            if bytes.get(parser.position) == Some(&b'\n') {
                parser.position += 1;
            }
        }
        Some(b'\n') => parser.position += 1,
        _ => return Err(Error::value("Missing PDF stream line ending")),
    }
    let length = match dictionary
        .iter()
        .find(|(key, _)| key == "Length")
        .map(|(_, value)| value)
    {
        Some(PdfValue::Integer(length)) if *length >= 0 => {
            usize::try_from(*length).map_err(|_| Error::value("PDF stream length overflow"))?
        }
        _ => return Err(Error::value("Invalid emitted PDF stream length")),
    };
    let end = parser
        .position
        .checked_add(length)
        .ok_or_else(|| Error::value("PDF stream length overflow"))?;
    let data = bytes
        .get(parser.position..end)
        .ok_or_else(|| Error::value("Truncated emitted PDF stream"))?
        .to_vec();
    parser.position = end;
    parser.space();
    if !bytes[parser.position..].starts_with(b"endstream") {
        return Err(Error::value("Missing emitted PDF endstream"));
    }
    parser.position += 9;
    parser.space();
    if parser.position != bytes.len() {
        return Err(Error::value("Trailing emitted PDF stream syntax"));
    }
    Ok(PdfValue::Stream { dictionary, data })
}

struct Parser<'a> {
    bytes: &'a [u8],
    position: usize,
}
fn whitespace(c: u8) -> bool {
    matches!(c, 0 | 9 | 10 | 12 | 13 | 32)
}
fn delimiter(c: u8) -> bool {
    whitespace(c) || b"()<>[]{}/%".contains(&c)
}
impl Parser<'_> {
    fn space(&mut self) {
        loop {
            while self
                .bytes
                .get(self.position)
                .is_some_and(|c| whitespace(*c))
            {
                self.position += 1
            }
            if self.bytes.get(self.position) == Some(&b'%') {
                while self
                    .bytes
                    .get(self.position)
                    .is_some_and(|c| !matches!(c, b'\r' | b'\n'))
                {
                    self.position += 1
                }
            } else {
                break;
            }
        }
    }
    fn word(&mut self) -> &[u8] {
        let start = self.position;
        while self
            .bytes
            .get(self.position)
            .is_some_and(|c| !delimiter(*c))
        {
            self.position += 1
        }
        &self.bytes[start..self.position]
    }
    fn value(&mut self) -> Result<PdfValue> {
        self.space();
        let c = *self
            .bytes
            .get(self.position)
            .ok_or_else(|| Error::value("Unexpected end of PDF value"))?;
        self.position += 1;
        Ok(match c {
            b'/' => {
                let mut out = Vec::new();
                while let Some(&c) = self.bytes.get(self.position) {
                    if delimiter(c) {
                        break;
                    }
                    self.position += 1;
                    if c == b'#' {
                        let bytes = self
                            .bytes
                            .get(self.position..self.position + 2)
                            .ok_or_else(|| Error::value("Invalid PDF name escape"))?;
                        out.push(
                            u8::from_str_radix(
                                std::str::from_utf8(bytes)
                                    .map_err(|_| Error::value("Invalid PDF name"))?,
                                16,
                            )
                            .map_err(|_| Error::value("Invalid PDF name escape"))?,
                        );
                        self.position += 2
                    } else {
                        out.push(c)
                    }
                }
                PdfValue::Name(out.iter().map(|&b| char::from(b)).collect())
            }
            b'(' => {
                let mut out = Vec::new();
                let mut depth = 1;
                while depth > 0 {
                    let c = *self
                        .bytes
                        .get(self.position)
                        .ok_or_else(|| Error::value("Unterminated PDF string"))?;
                    self.position += 1;
                    match c {
                        b'(' => {
                            depth += 1;
                            out.push(c)
                        }
                        b')' => {
                            depth -= 1;
                            if depth > 0 {
                                out.push(c)
                            }
                        }
                        b'\\' => {
                            let c = *self
                                .bytes
                                .get(self.position)
                                .ok_or_else(|| Error::value("Invalid PDF string escape"))?;
                            self.position += 1;
                            match c {
                                b'n' => out.push(b'\n'),
                                b'r' => out.push(b'\r'),
                                b't' => out.push(b'\t'),
                                b'b' => out.push(8),
                                b'f' => out.push(12),
                                b'\r' => {
                                    if self.bytes.get(self.position) == Some(&b'\n') {
                                        self.position += 1
                                    }
                                }
                                b'\n' => {}
                                b'0'..=b'7' => {
                                    let mut n = (c - b'0') as u16;
                                    for _ in 0..2 {
                                        if let Some(&d @ b'0'..=b'7') =
                                            self.bytes.get(self.position)
                                        {
                                            n = n * 8 + (d - b'0') as u16;
                                            self.position += 1
                                        } else {
                                            break;
                                        }
                                    }
                                    out.push(n as u8)
                                }
                                _ => out.push(c),
                            }
                        }
                        b'\r' => {
                            if self.bytes.get(self.position) == Some(&b'\n') {
                                self.position += 1
                            }
                            out.push(b'\n')
                        }
                        _ => out.push(c),
                    }
                }
                PdfValue::String(out)
            }
            b'[' => {
                let mut values = Vec::new();
                loop {
                    self.space();
                    if self.bytes.get(self.position) == Some(&b']') {
                        self.position += 1;
                        break;
                    }
                    values.push(self.value()?)
                }
                PdfValue::Array(values)
            }
            b'<' => {
                if self.bytes.get(self.position) == Some(&b'<') {
                    self.position += 1;
                    let mut d = Vec::new();
                    loop {
                        self.space();
                        if self.bytes.get(self.position..self.position + 2) == Some(b">>") {
                            self.position += 2;
                            break;
                        }
                        let PdfValue::Name(k) = self.value()? else {
                            return Err(Error::value("Expected PDF dictionary key"));
                        };
                        let v = self.value()?;
                        if let Some((_, old)) = d.iter_mut().find(|(name, _)| name == &k) {
                            *old = v
                        } else {
                            d.push((k, v))
                        }
                    }
                    PdfValue::Dictionary(d)
                } else {
                    let mut digits = Vec::new();
                    loop {
                        let c = *self
                            .bytes
                            .get(self.position)
                            .ok_or_else(|| Error::value("Unterminated PDF hex string"))?;
                        self.position += 1;
                        if c == b'>' {
                            break;
                        }
                        if !whitespace(c) {
                            digits.push(c)
                        }
                    }
                    if digits.len() % 2 != 0 {
                        digits.push(b'0')
                    }
                    let mut out = Vec::new();
                    for pair in digits.chunks(2) {
                        out.push(
                            u8::from_str_radix(
                                std::str::from_utf8(pair)
                                    .map_err(|_| Error::value("Invalid PDF hex string"))?,
                                16,
                            )
                            .map_err(|_| Error::value("Invalid PDF hex string"))?,
                        )
                    }
                    PdfValue::String(out)
                }
            }
            _ => {
                self.position -= 1;
                let w = self.word();
                match w {
                    b"null" => PdfValue::Null,
                    b"true" => PdfValue::Boolean(true),
                    b"false" => PdfValue::Boolean(false),
                    _ => {
                        let s = std::str::from_utf8(w)
                            .map_err(|_| Error::value("Invalid PDF token"))?;
                        if let Ok(n) = s.parse::<i64>() {
                            let saved = self.position;
                            self.space();
                            let g = std::str::from_utf8(self.word())
                                .ok()
                                .and_then(|s| s.parse::<u16>().ok());
                            self.space();
                            if let Some(g) = g {
                                if self.word() == b"R" && n >= 0 && n <= u32::MAX as i64 {
                                    return Ok(PdfValue::Reference(n as u32, g));
                                }
                            }
                            self.position = saved;
                            PdfValue::Integer(n)
                        } else {
                            PdfValue::Real(
                                s.parse().map_err(|_| Error::value("Invalid PDF number"))?,
                            )
                        }
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod emitted_stream_tests {
    use super::*;
    #[test]
    fn stream_lengths_protect_binary_and_literal_keyword_boundaries() {
        let value = PdfValue::Stream {
            dictionary: vec![("Note".into(), PdfValue::String(b"\nstream\n".to_vec()))],
            data: b"\x00\nendstream\xff".to_vec(),
        };
        let encoded = value.serialize().unwrap();
        let parsed = parse_indirect_body(&encoded).unwrap();
        let PdfValue::Stream { data, .. } = parsed else {
            panic!("Missing stream")
        };
        let PdfValue::Stream { data: expected, .. } = value else {
            unreachable!()
        };
        assert_eq!(data, expected);
        for malformed in [
            b"<< /Length 100 >>\nstream\nX\nendstream".as_slice(),
            b"<< /Length -1 >>\nstream\nX\nendstream",
            b"<< /Length 1 >> stream X endstream",
            b"<< /Length 1 >>\nstream\nX\nendstream junk",
        ] {
            assert!(parse_indirect_body(malformed).is_err());
        }
    }
}
