//! Immutable declarative Type1 PFB/PFA resources (`fonts/formats/type1.py`).
use super::type1_cipher::{EEXEC_CIPHER_SEED, iter_cipher};
use crate::core::errors::{Error, Result};
use crate::pdf::writer::latin1;
use regex::{Regex, bytes::Regex as ByteRegex};
use serde::{Deserialize, Serialize};
use std::path::Path;

include!("type1_tables.rs");
pub const NUMBER: &str = r"[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?";
pub const PS_NAME: &str = r"[^\s/()<>\[\]{}%]+";

#[derive(Clone, Debug, Default)]
pub struct PfbParser {
    pub segments: Vec<(u8, Vec<u8>)>,
    pub length1: usize,
    pub length2: usize,
    pub length3: usize,
}
impl PfbParser {
    pub fn parse(&mut self, path: impl AsRef<Path>) -> bool {
        *self = Self::default();
        match crate::resources::host::read(path) {
            Ok(data) => self.parse_bytes(&data),
            Err(_) => false,
        }
    }
    pub fn parse_bytes(&mut self, data: &[u8]) -> bool {
        *self = Self::default();
        if data.starts_with(b"%!") {
            return self.parse_pfa(data);
        }
        let (mut pos, mut phase) = (0, 1);
        while pos + 2 <= data.len() {
            if data[pos] != 0x80 {
                return false;
            }
            let kind = data[pos + 1];
            pos += 2;
            if kind == 3 {
                return pos == data.len() && self.length1 != 0 && self.length2 != 0;
            }
            if ![1, 2].contains(&kind) || pos + 4 > data.len() {
                return false;
            }
            let size = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("PFB length word"))
                as usize;
            pos += 4;
            if size > data.len() - pos {
                return false;
            }
            if kind == 2 {
                if phase == 3 {
                    return false;
                }
                phase = 2;
                self.length2 += size;
            } else if phase == 1 {
                self.length1 += size;
            } else {
                phase = 3;
                self.length3 += size;
            }
            self.segments.push((kind, data[pos..pos + size].to_vec()));
            pos += size;
        }
        false
    }
    pub fn get_font_data(&self) -> Vec<u8> {
        self.segments
            .iter()
            .flat_map(|(_, payload)| payload.iter().copied())
            .collect()
    }
    fn parse_pfa(&mut self, data: &[u8]) -> bool {
        let start = ByteRegex::new(r"\bcurrentfile\s+eexec[ \t]*(?:\r\n|\r|\n)")
            .expect("PFA eexec grammar");
        let trailer =
            ByteRegex::new(r"(?:\s*0){512}\s*cleartomark\b").expect("PFA trailer grammar");
        let (Some(start), Some(trailer)) = (start.find(data), trailer.find(data)) else {
            return false;
        };
        if trailer.start() <= start.end() {
            return false;
        }
        let header = data[..start.end()].to_vec();
        let mut encrypted = data[start.end()..trailer.start()].to_vec();
        let first: Vec<_> = encrypted
            .iter()
            .copied()
            .skip_while(u8::is_ascii_whitespace)
            .take(4)
            .collect();
        if first.len() == 4 && first.iter().all(u8::is_ascii_hexdigit) {
            let digits: Vec<_> = encrypted
                .iter()
                .copied()
                .filter(|c| !c.is_ascii_whitespace())
                .collect();
            if digits.len() % 2 != 0 || !digits.iter().all(u8::is_ascii_hexdigit) {
                return false;
            }
            encrypted = digits
                .chunks_exact(2)
                .map(|pair| {
                    (char::from(pair[0]).to_digit(16).expect("hex digit") * 16
                        + char::from(pair[1]).to_digit(16).expect("hex digit"))
                        as u8
                })
                .collect();
        }
        self.length1 = header.len();
        self.length2 = encrypted.len();
        self.length3 = data.len() - trailer.start();
        self.segments = vec![
            (1, header),
            (2, encrypted),
            (1, data[trailer.start()..].to_vec()),
        ];
        self.length2 != 0
    }
}
pub fn ps_tokens(text: &str) -> Vec<String> {
    let clean = Regex::new(r"%[^\r\n]*")
        .expect("PostScript comments")
        .replace_all(text, "");
    Regex::new(&format!(r"/{PS_NAME}|[\[\]{{}}]|{PS_NAME}"))
        .expect("PostScript token grammar")
        .find_iter(&clean)
        .map(|m| m.as_str().to_string())
        .collect()
}
pub fn declarative_encoding(program: &str) -> Result<(Vec<String>, String)> {
    let mut tokens = ps_tokens(program);
    if tokens.last().is_some_and(|t| t == "readonly") {
        tokens.pop();
    }
    if tokens.first().is_some_and(|t| t == "[") {
        if !tokens.last().is_some_and(|t| t == "]")
            || tokens.len() > 258
            || tokens[1..tokens.len() - 1]
                .iter()
                .any(|t| !t.starts_with('/'))
        {
            return Err(Error::value("invalid Type1 literal Encoding")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        let mut encoding: Vec<_> = tokens[1..tokens.len() - 1]
            .iter()
            .map(|t| t[1..].to_string())
            .collect();
        encoding.resize(256, ".notdef".into());
        return Ok((encoding, "literal".into()));
    }
    let (prefix, mut encoding, source) = if tokens.first().is_some_and(|t| t == "StandardEncoding")
    {
        let encoding = STANDARD_ENCODING.iter().map(|&s| s.to_string()).collect();
        if tokens.len() == 1 {
            return Ok((encoding, "standard".into()));
        }
        (
            vec!["StandardEncoding", "256", "array", "copy"],
            encoding,
            "standard-copy",
        )
    } else {
        (vec!["256", "array"], vec![".notdef".into(); 256], "array")
    };
    if tokens.len() < prefix.len() || !tokens.iter().zip(&prefix).all(|(a, b)| a == b) {
        return Err(Error::value(
            "unsupported Type1 Encoding program (expected static array initialization)",
        )
        .with_issue(crate::core::recovery::Issue::Font));
    }
    tokens.drain(..prefix.len());
    if tokens.iter().any(|t| t == "{") {
        let end = tokens
            .iter()
            .position(|t| t == "}")
            .map_or(tokens.len(), |n| n + 1);
        tokens.drain(..end);
        if tokens.first().is_some_and(|t| t == "for") {
            tokens.remove(0);
        }
    }
    let code = |word: &str| -> Result<i64> {
        let (digits, radix) = if word.to_ascii_lowercase().starts_with("0x") {
            (&word[2..], 16)
        } else if word.len() > 1 && word.starts_with('0') {
            (word, 8)
        } else {
            (word, 10)
        };
        i64::from_str_radix(digits, radix).map_err(|_| {
            Error::value("invalid Type1 Encoding index")
                .with_issue(crate::core::recovery::Issue::Font)
        })
    };
    let mut at = 0;
    while at < tokens.len() {
        let t = &tokens[at..];
        if t.len() >= 4 && t[0] == "dup" && t[2].starts_with('/') && t[3] == "put" {
            let index = code(&t[1])?;
            if !(0..256).contains(&index) {
                return Err(Error::value("invalid Type1 Encoding index")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            encoding[index as usize] = t[2][1..].into();
            at += 4;
        } else if t.len() >= 7
            && t[0] == "dup"
            && t[1] == "dup"
            && t[3] == "exch"
            && t[5] == "get"
            && t[6] == "put"
        {
            let (target, origin) = (code(&t[2])?, code(&t[4])?);
            if !(0..256).contains(&target) || !(0..256).contains(&origin) {
                return Err(Error::value("invalid Type1 Encoding index")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            encoding[target as usize] = encoding[origin as usize].clone();
            at += 7;
        } else if t.len() >= 8
            && t[0] == "dup"
            && t[1] == "dup"
            && t[4] == "getinterval"
            && t[6] == "exch"
            && t[7] == "putinterval"
        {
            let (origin, count, target) = (code(&t[2])?, code(&t[3])?, code(&t[5])?);
            if origin < 0
                || count < 0
                || target < 0
                || origin as i128 + count as i128 > 256
                || target as i128 + count as i128 > 256
            {
                return Err(Error::value("invalid Type1 Encoding interval")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            for offset in 0..count {
                encoding[(target + offset) as usize] = encoding[(origin + offset) as usize].clone();
            }
            at += 8;
        } else {
            return Err(Error::value("unsupported Type1 Encoding program")
                .with_issue(crate::core::recovery::Issue::Font));
        }
    }
    Ok((encoding, source.into()))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Type1Resource {
    pub data: Vec<u8>,
    pub length1: usize,
    pub length2: usize,
    pub length3: usize,
    pub name: String,
    pub bbox: [f64; 4],
    pub italic_angle: f64,
    pub fixed_pitch: bool,
    pub matrix: [f64; 6],
    pub encoding: Vec<String>,
    pub encoding_source: String,
    pub descriptor_metrics: Vec<(String, i64)>,
}
impl Type1Resource {
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let mut pfb = PfbParser::default();
        if !pfb.parse(&path) {
            return Err(Error::value(format!(
                "invalid PFB {}",
                path.as_ref().display()
            )));
        }
        Self::from_pfb(&pfb)
    }
    pub fn from_pfb(pfb: &PfbParser) -> Result<Self> {
        let data = pfb.get_font_data();
        let header: String = data[..pfb.length1].iter().map(|&b| char::from(b)).collect();
        let header = Regex::new(r"%[^\r\n]*")
            .expect("PS comments")
            .replace_all(&header, "")
            .into_owned();
        let name_pattern = Regex::new(&format!(r"/FontName\s+/({PS_NAME})")).expect("Type1 name");
        let name = name_pattern.captures(&header).ok_or_else(|| {
            Error::value("missing Type1 FontName").with_issue(crate::core::recovery::Issue::Font)
        })?[1]
            .to_string();
        let font_type = Regex::new(r"/FontType\s+(\d+)\b").expect("Type1 type");
        if !font_type
            .captures(&header)
            .is_some_and(|m| m[1].parse::<i64>() == Ok(1))
        {
            return Err(Error::value("unsupported Type1 FontType (expected 1)")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        let numbers = |key: &str, count: usize| -> Result<Vec<f64>> {
            let pattern =
                Regex::new(&format!(r"/{key}\s*[\[{{]([^\]}}]*)[\]}}]")).expect("Type1 array");
            let found = pattern
                .captures(&header)
                .ok_or_else(|| Error::value(format!("missing Type1 {key}")))?;
            let words: Vec<_> = found[1].split_whitespace().collect();
            let valid = Regex::new(&format!("^{NUMBER}$")).expect("Type1 number");
            if words.len() != count || words.iter().any(|w| !valid.is_match(w)) {
                return Err(Error::value(format!("invalid Type1 {key}")));
            }
            words
                .into_iter()
                .map(|word| {
                    let value: f64 = word
                        .parse()
                        .map_err(|_| Error::value(format!("invalid Type1 {key}")))?;
                    if !value.is_finite() {
                        Err(Error::value(format!("invalid Type1 {key}")))
                    } else {
                        Ok(value)
                    }
                })
                .collect()
        };
        let matrix: [f64; 6] = numbers("FontMatrix", 6)?
            .try_into()
            .expect("six matrix entries");
        let bbox: [f64; 4] = numbers("FontBBox", 4)?.try_into().expect("four bounds");
        if bbox[0] > bbox[2] || bbox[1] > bbox[3] {
            return Err(Error::value("invalid Type1 FontBBox")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        let angle_pattern =
            Regex::new(&format!(r"/ItalicAngle\s+({NUMBER})")).expect("Type1 angle");
        let italic_angle: f64 = angle_pattern.captures(&header).map_or(Ok(0.0), |m| {
            m[1].parse().map_err(|_| {
                Error::value("invalid Type1 ItalicAngle")
                    .with_issue(crate::core::recovery::Issue::Font)
            })
        })?;
        if !italic_angle.is_finite() {
            return Err(Error::value("invalid Type1 ItalicAngle")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        let fixed_pitch = Regex::new(r"/isFixedPitch\s+(true|false)")
            .expect("Type1 pitch")
            .captures(&header)
            .is_some_and(|m| &m[1] == "true");
        let definition = Regex::new(r"(?s)/Encoding\s*(.*?)\bdef\b").expect("Type1 encoding");
        let definition = definition.captures(&header).ok_or_else(|| {
            Error::value("unsupported Type1 Encoding program")
                .with_issue(crate::core::recovery::Issue::Font)
        })?;
        let (encoding, encoding_source) = declarative_encoding(&definition[1])?;
        let mut plain = Vec::new();
        for byte in iter_cipher(
            data[pfb.length1..pfb.length1 + pfb.length2].iter().copied(),
            EEXEC_CIPHER_SEED,
            false,
        ) {
            plain.push(byte);
            if plain.ends_with(b"/Subrs") || plain.ends_with(b"/CharStrings") {
                break;
            }
        }
        let dictionary = header
            + "\n"
            + &plain
                .get(4..)
                .unwrap_or_default()
                .iter()
                .map(|&b| char::from(b))
                .collect::<String>();
        let mut descriptor_metrics = Vec::new();
        for (ps_key, pdf_key) in [
            ("Ascender", "Ascent"),
            ("Descender", "Descent"),
            ("CapHeight", "CapHeight"),
            ("XHeight", "XHeight"),
            ("StdVW", "StemV"),
        ] {
            let pattern =
                Regex::new(&format!(r"/{ps_key}\s*[\[{{]?\s*({NUMBER})")).expect("Type1 metric");
            if let Some(value) = pattern.captures(&dictionary) {
                let number: f64 = value[1]
                    .parse()
                    .map_err(|_| Error::value(format!("invalid Type1 {ps_key}")))?;
                if !number.is_finite() {
                    return Err(Error::value(format!("invalid Type1 {ps_key}")));
                }
                descriptor_metrics.push((pdf_key.into(), number as i64));
            }
        }
        Ok(Self {
            data,
            length1: pfb.length1,
            length2: pfb.length2,
            length3: pfb.length3,
            name,
            bbox,
            italic_angle,
            fixed_pitch,
            matrix,
            encoding,
            encoding_source,
            descriptor_metrics,
        })
    }
    pub fn transformed(&self, slant: i64, extend: i64) -> Result<Self> {
        if slant == 0 && extend == 0 {
            return Ok(self.clone());
        }
        let [a, b, c, d, e, f] = self.matrix;
        let factor = if extend == 0 {
            1.0
        } else {
            extend as f64 / 1000.0
        };
        let matrix = [
            (a + b * slant as f64 / 1000.0) * factor,
            b,
            (c + d * slant as f64 / 1000.0) * factor,
            d,
            (e + f * slant as f64 / 1000.0) * factor,
            f,
        ];
        let name = format!(
            "{}{}{}",
            self.name,
            if slant != 0 {
                format!("-Slant_{slant}")
            } else {
                String::new()
            },
            if extend != 0 {
                format!("-Extend_{extend}")
            } else {
                String::new()
            }
        );
        let angle = self.italic_angle - (slant as f64 / 1000.0).atan().to_degrees();
        let header: String = self.data[..self.length1]
            .iter()
            .map(|&b| char::from(b))
            .collect();
        let matrix_text = format!(
            "/FontMatrix [{}]",
            matrix
                .iter()
                .map(|&n| format_general(n, 9))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let header = Regex::new(r"/FontMatrix\s*[\[{][^\]}]*[\]}]")
            .expect("matrix rewrite")
            .replacen(&header, 1, regex::NoExpand(&matrix_text))
            .into_owned();
        let name_text = format!("/FontName /{name}");
        let header = Regex::new(&format!(r"/FontName\s+/({PS_NAME})"))
            .expect("name rewrite")
            .replacen(&header, 1, regex::NoExpand(&name_text))
            .into_owned();
        let angle_text = format!("/ItalicAngle {}", format_general(angle, 9));
        let header = Regex::new(&format!(r"/ItalicAngle\s+({NUMBER})"))
            .expect("angle rewrite")
            .replacen(&header, 1, regex::NoExpand(&angle_text))
            .into_owned();
        let mut resource = self.clone();
        let mut data = latin1(&header)?;
        resource.length1 = data.len();
        data.extend(&self.data[self.length1..]);
        resource.data = data;
        resource.name = name;
        resource.matrix = matrix;
        resource.italic_angle = angle;
        Ok(resource)
    }
}

/// Python's general float spelling at a fixed significant-digit precision.
pub fn format_general(value: f64, precision: usize) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    let scientific = format!("{:.*e}", precision.saturating_sub(1), value);
    let (mantissa, exponent) = scientific.split_once('e').expect("scientific spelling");
    let exponent: i32 = exponent.parse().expect("scientific exponent");
    if exponent < -4 || exponent >= precision as i32 {
        format!(
            "{}e{}{:02}",
            mantissa.trim_end_matches('0').trim_end_matches('.'),
            if exponent >= 0 { "+" } else { "-" },
            exponent.abs()
        )
    } else {
        let decimals = (precision as i32 - exponent - 1).max(0) as usize;
        let text = format!("{value:.decimals$}");
        if decimals > 0 {
            text.trim_end_matches('0').trim_end_matches('.').into()
        } else {
            text
        }
    }
}
