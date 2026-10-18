//! PK run-length rasters and declarative PGC glyphs (`fonts/formats/bitmap.py`).
use crate::core::errors::{Error, Result};
use crate::fonts::scaling::{divide_scaled, pdf_font_size};
use regex::bytes::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const BITS_PER_BYTE: usize = 8;
pub const BYTE_MSB: u8 = 0x80;
pub const BYTE_RADIX: i64 = 256;
pub const NIBBLE_BITS: u32 = 4;
pub const NIBBLE_MASK: u8 = 15;
pub const PACKET_FORM_MASK: u8 = 7;
pub const SHORT_PACKET_FORM_COUNT: u8 = 4;
pub const EXTENDED_LENGTH_MASK: u8 = 3;
pub const UNPACKED_RASTER: u8 = 14;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketForm {
    Short,
    Extended,
    Long,
}
#[derive(Clone, Copy, Debug)]
pub struct PacketHeader {
    pub packing: u8,
    pub form: PacketForm,
    pub length_high: i64,
    pub initial_black: bool,
}
impl PacketHeader {
    pub fn from_byte(flag: u8) -> Self {
        let form = flag & PACKET_FORM_MASK;
        let (layout, high) = if form < SHORT_PACKET_FORM_COUNT {
            (PacketForm::Short, form)
        } else if form < PACKET_FORM_MASK {
            (PacketForm::Extended, form & EXTENDED_LENGTH_MASK)
        } else {
            (PacketForm::Long, 0)
        };
        Self {
            packing: flag >> NIBBLE_BITS,
            form: layout,
            length_high: high as i64,
            initial_black: flag & 8 != 0,
        }
    }
}
pub struct Reader<'a> {
    pub data: &'a [u8],
    pub cursor: usize,
}
impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, cursor: 0 }
    }
    pub fn take(&mut self, count: i64) -> Result<&'a [u8]> {
        let count = usize::try_from(count).map_err(|_| {
            Error::value("Unexpected EOF in PK font").with_issue(crate::core::recovery::Issue::Font)
        })?;
        let end = self
            .cursor
            .checked_add(count)
            .filter(|end| *end <= self.data.len())
            .ok_or_else(|| {
                Error::value("Unexpected EOF in PK font")
                    .with_issue(crate::core::recovery::Issue::Font)
            })?;
        let bytes = &self.data[self.cursor..end];
        self.cursor = end;
        Ok(bytes)
    }
    pub fn number(&mut self, count: usize, signed: bool) -> Result<i64> {
        let bytes = self.take(count as i64)?;
        let mut value: i64 = if signed && bytes.first().is_some_and(|byte| byte & 0x80 != 0) {
            -1
        } else {
            0
        };
        for &byte in bytes {
            value = (value << 8) | byte as i64;
        }
        Ok(value)
    }
}
pub struct PackedRaster<'a> {
    pub data: &'a [u8],
    pub dyn_parameter: u8,
    pub cursor: usize,
    pub repeat: usize,
}
impl<'a> PackedRaster<'a> {
    pub fn new(data: &'a [u8], dyn_parameter: u8) -> Self {
        Self {
            data,
            dyn_parameter,
            cursor: 0,
            repeat: 0,
        }
    }
    pub fn nibble(&mut self) -> Result<u8> {
        let byte = self.data.get(self.cursor / 2).ok_or_else(|| {
            Error::value("Truncated PK packed raster")
                .with_issue(crate::core::recovery::Issue::Font)
        })?;
        let shift = if self.cursor % 2 == 0 { NIBBLE_BITS } else { 0 };
        self.cursor += 1;
        Ok((byte >> shift) & NIBBLE_MASK)
    }
    pub fn number(&mut self) -> Result<usize> {
        let mut pending = 0;
        loop {
            let first = self.nibble()? as usize;
            if first == 14 {
                pending += 1;
                continue;
            }
            if first == 15 {
                self.repeat = 1;
                continue;
            }
            let dyn_parameter = self.dyn_parameter as usize;
            let result = if first == 0 {
                let mut zeros = 0;
                let mut result;
                loop {
                    result = self.nibble()? as usize;
                    zeros += 1;
                    if result != 0 {
                        break;
                    }
                }
                for _ in 0..zeros {
                    let nibble = self.nibble()? as usize;
                    result = result
                        .checked_mul(16)
                        .and_then(|value| value.checked_add(nibble))
                        .ok_or_else(|| {
                            Error::value("Invalid PK packed run")
                                .with_issue(crate::core::recovery::Issue::Font)
                        })?;
                }
                result - 15 + (13 - dyn_parameter) * 16 + dyn_parameter
            } else if first <= dyn_parameter {
                first
            } else {
                (first - dyn_parameter - 1) * 16 + self.nibble()? as usize + dyn_parameter + 1
            };
            if pending != 0 {
                pending -= 1;
                self.repeat = result;
                continue;
            }
            return Ok(result);
        }
    }
    pub fn decode(&mut self, width: usize, height: usize, mut on: bool) -> Result<Vec<u8>> {
        let stride = width.div_ceil(BITS_PER_BYTE);
        let mut rows = Vec::new();
        let mut row = vec![0; stride];
        let mut column = 0;
        let mut row_count = 0;
        while row_count < height {
            let mut count = self.number()?;
            if count == 0 || width == 0 {
                return Err(Error::value("Invalid PK packed run")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            while count != 0 {
                let available = (width - column).min(count);
                if on {
                    for pixel in column..column + available {
                        row[pixel / 8] |= BYTE_MSB >> (pixel % 8);
                    }
                }
                column += available;
                count -= available;
                if column == width {
                    let new_count = row_count
                        .checked_add(self.repeat)
                        .and_then(|n| n.checked_add(1))
                        .filter(|n| *n <= height)
                        .ok_or_else(|| {
                            Error::value("PK raster has too many rows")
                                .with_issue(crate::core::recovery::Issue::Font)
                        })?;
                    for _ in row_count..new_count {
                        rows.extend_from_slice(&row);
                    }
                    row_count = new_count;
                    self.repeat = 0;
                    row.fill(0);
                    column = 0;
                    if count != 0 && row_count == height {
                        return Err(Error::value("PK raster has too many bits")
                            .with_issue(crate::core::recovery::Issue::Font));
                    }
                }
            }
            on = !on;
        }
        if column != 0 {
            return Err(Error::value("PK raster ends inside a row")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        Ok(rows)
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BitmapGlyph {
    pub width: i64,
    pub height: i64,
    pub xoff: i64,
    pub yoff: i64,
    pub raster: Vec<u8>,
    pub advance: i64,
    pub bbox: Option<[i64; 4]>,
    pub content: Vec<u8>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BitmapKind {
    Pk,
    Pgc,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BitmapResource {
    pub data: Vec<u8>,
    pub name: String,
    pub glyphs: BTreeMap<u8, BitmapGlyph>,
    pub dpi: i64,
    pub scale: f64,
    pub kind: BitmapKind,
    pub encoding: Vec<String>,
}
#[derive(Clone, Copy, Debug)]
pub struct BitmapOptions {
    pub digits: u32,
    pub resolution: i64,
}
impl BitmapResource {
    fn resource(
        data: Vec<u8>,
        name: String,
        glyphs: BTreeMap<u8, BitmapGlyph>,
        dpi: i64,
        scale: f64,
        kind: BitmapKind,
    ) -> Self {
        Self {
            data,
            name,
            glyphs,
            dpi,
            scale,
            kind,
            encoding: vec![".notdef".into(); 256],
        }
    }
    pub fn read_pk(path: &Path, dpi: i64) -> Result<Self> {
        let data = crate::resources::host::read(path)?;
        Self::parse_pk(
            data,
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            dpi,
        )
    }
    pub fn parse_pk(data: Vec<u8>, name: String, dpi: i64) -> Result<Self> {
        let mut reader = Reader::new(&data);
        if reader.take(2)? != b"\xf7Y" {
            return Err(Error::value("Invalid PK preamble/version")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        let count = reader.number(1, false)?;
        reader.take(count)?;
        reader.take(16)?;
        let mut glyphs = BTreeMap::new();
        while reader.cursor < data.len() {
            let flag = reader.number(1, false)? as u8;
            if flag == 245 {
                return Ok(Self::resource(data, name, glyphs, dpi, 0.0, BitmapKind::Pk));
            }
            if (240..=243).contains(&flag) {
                let count = reader.number((flag - 239) as usize, false)?;
                reader.take(count)?;
                continue;
            }
            if flag == 244 {
                reader.take(4)?;
                continue;
            }
            if flag == 246 {
                continue;
            }
            if flag >= 240 {
                return Err(Error::value("Unexpected PK command")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            let header = PacketHeader::from_byte(flag);
            if header.packing > UNPACKED_RASTER {
                return Err(Error::value("Invalid PK packing parameter")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            let (end, code, width, height, xoff, yoff) = match header.form {
                PacketForm::Short => {
                    let length = header.length_high * BYTE_RADIX + reader.number(1, false)?;
                    let code = reader.number(1, false)?;
                    let end = reader.cursor as i64 + length;
                    reader.take(4)?;
                    (
                        end,
                        code,
                        reader.number(1, false)?,
                        reader.number(1, false)?,
                        reader.number(1, true)?,
                        reader.number(1, true)?,
                    )
                }
                PacketForm::Extended => {
                    let length =
                        header.length_high * BYTE_RADIX * BYTE_RADIX + reader.number(2, false)?;
                    let code = reader.number(1, false)?;
                    let end = reader.cursor as i64 + length;
                    reader.take(5)?;
                    (
                        end,
                        code,
                        reader.number(2, true)?,
                        reader.number(2, true)?,
                        reader.number(2, true)?,
                        reader.number(2, true)?,
                    )
                }
                PacketForm::Long => {
                    let length = reader.number(4, true)?;
                    let code = reader.number(4, true)?;
                    let end = reader.cursor as i64 + length;
                    reader.take(12)?;
                    (
                        end,
                        code,
                        reader.number(4, true)?,
                        reader.number(4, true)?,
                        reader.number(4, true)?,
                        reader.number(4, true)?,
                    )
                }
            };
            let packed = reader.take(end - reader.cursor as i64)?;
            if !(0..=255).contains(&code) {
                return Err(Error::value("Invalid PK character code")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            let raster = if width <= 0 || height <= 0 {
                Vec::new()
            } else if header.packing == UNPACKED_RASTER {
                let width = width as usize;
                let height = height as usize;
                let bits = width.checked_mul(height).ok_or_else(|| {
                    Error::value("Truncated PK bit raster")
                        .with_issue(crate::core::recovery::Issue::Font)
                })?;
                if bits > packed.len().saturating_mul(8) {
                    return Err(Error::value("Truncated PK bit raster")
                        .with_issue(crate::core::recovery::Issue::Font));
                }
                let stride = width.div_ceil(8);
                let mut rows = vec![0; stride * height];
                for bit in 0..bits {
                    if packed[bit / 8] & (BYTE_MSB >> (bit % 8)) != 0 {
                        let row = bit / width;
                        let column = bit % width;
                        rows[row * stride + column / 8] |= BYTE_MSB >> (column % 8);
                    }
                }
                rows
            } else {
                PackedRaster::new(packed, header.packing).decode(
                    width as usize,
                    height as usize,
                    header.initial_black,
                )?
            };
            glyphs.insert(
                code as u8,
                BitmapGlyph {
                    width,
                    height,
                    xoff,
                    yoff,
                    raster,
                    ..BitmapGlyph::default()
                },
            );
        }
        Err(Error::value("Missing PK postamble").with_issue(crate::core::recovery::Issue::Font))
    }
    pub fn read_pgc(path: &Path) -> Result<Self> {
        let data = crate::resources::host::read(path)?;
        Self::parse_pgc(
            data,
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        )
    }
    pub fn parse_pgc(data: Vec<u8>, name: String) -> Result<Self> {
        // bytes.splitlines recognises CR, LF and CRLF, without trimming glyph paint.
        let mut lines = Vec::new();
        let mut start = 0;
        let mut position = 0;
        while position < data.len() {
            if matches!(data[position], b'\r' | b'\n') {
                let line = &data[start..position];
                if !line.is_empty() && !line.starts_with(b"%") {
                    lines.push(line);
                }
                if data[position] == b'\r' && data.get(position + 1) == Some(&b'\n') {
                    position += 1;
                }
                start = position + 1;
            }
            position += 1;
        }
        if start < data.len() {
            let line = &data[start..];
            if !line.starts_with(b"%") {
                lines.push(line);
            }
        }
        if !lines
            .first()
            .is_some_and(|line| line.starts_with(b"\\pdffontscale "))
        {
            return Err(Error::value("Missing PGC font scale")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        let scale = lines[0]
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|word| !word.is_empty())
            .nth(1)
            .and_then(|word| std::str::from_utf8(word).ok())
            .and_then(|word| word.parse::<f64>().ok())
            .filter(|scale| scale.is_finite() && *scale > 0.0 && *scale <= 1000.0)
            .ok_or_else(|| {
                Error::value("Invalid PGC font scale")
                    .with_issue(crate::core::recovery::Issue::Font)
            })?;
        let preamble = Regex::new(r"(?-u)^\\pdfglyph\s+(-?\d+(?:\s+-?\d+){7})\s*=\s*$")
            .expect("constant PGC grammar");
        let mut glyphs = BTreeMap::new();
        let mut cursor = 1;
        while cursor < lines.len() {
            let line = lines[cursor];
            cursor += 1;
            if !line.starts_with(b"\\pdfglyph") {
                continue;
            }
            let capture = preamble.captures(line).ok_or_else(|| {
                Error::value("Invalid PGC glyph preamble")
                    .with_issue(crate::core::recovery::Issue::Font)
            })?;
            let fields = capture[1]
                .split(|byte| byte.is_ascii_whitespace())
                .filter(|word| !word.is_empty())
                .map(|word| {
                    std::str::from_utf8(word)
                        .unwrap()
                        .parse::<i64>()
                        .map_err(|_| {
                            Error::value("Invalid PGC glyph preamble")
                                .with_issue(crate::core::recovery::Issue::Font)
                        })
                })
                .collect::<Result<Vec<_>>>()?;
            if !(0..=255).contains(&fields[0]) {
                return Err(Error::value("Invalid PGC character code")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            let mut content = Vec::new();
            while cursor < lines.len() && !lines[cursor].starts_with(b"\\endglyph") {
                content.extend_from_slice(lines[cursor]);
                content.push(b'\n');
                cursor += 1;
            }
            if cursor == lines.len() {
                return Err(Error::value("Unexpected EOF in PGC glyph")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            cursor += 1;
            if content.is_empty() {
                content.push(b'\n');
            }
            glyphs.insert(
                fields[0] as u8,
                BitmapGlyph {
                    advance: fields[1],
                    bbox: Some([fields[4], fields[5], fields[6], fields[7]]),
                    content,
                    ..BitmapGlyph::default()
                },
            );
        }
        Ok(Self::resource(
            data,
            name,
            glyphs,
            0,
            scale,
            BitmapKind::Pgc,
        ))
    }
    pub fn pk_scale_units(size: i64, options: &BitmapOptions) -> Result<i64> {
        let factor = divide_scaled(72, options.resolution, 5 + options.digits)?;
        divide_scaled(
            factor,
            divide_scaled(pdf_font_size(size), 6_578_176, options.digits + 2)?,
            0,
        )
    }
    pub fn cursor_advance(
        &self,
        size: i64,
        selected_width: i64,
        options: &BitmapOptions,
    ) -> Result<i64> {
        let scale = Self::pk_scale_units(size, options)?;
        let size = pdf_font_size(size);
        let width = divide_scaled(divide_scaled(selected_width, size, 7)?, scale, 0)?;
        Ok(((scale as f64 / 100000.0) * (width as f64 / 100.0) * size as f64) as i64)
    }
    pub fn font_scale(&self, size: i64, options: &BitmapOptions) -> Result<f64> {
        if self.kind == BitmapKind::Pgc {
            Ok(self.scale)
        } else {
            Ok(Self::pk_scale_units(size, options)? as f64 / 100000.0)
        }
    }
    pub fn char_width(
        &self,
        size: i64,
        code: u8,
        selected_width: i64,
        options: &BitmapOptions,
    ) -> Result<f64> {
        if self.kind == BitmapKind::Pgc {
            Ok(self.glyphs.get(&code).map_or(0.0, |glyph| {
                if glyph.advance != 0 {
                    glyph.advance as f64
                } else {
                    (selected_width as f64 / self.scale / pdf_font_size(size) as f64).trunc()
                }
            }))
        } else {
            Ok(divide_scaled(
                divide_scaled(selected_width, pdf_font_size(size), 7)?,
                Self::pk_scale_units(size, options)?,
                0,
            )? as f64
                / 100.0)
        }
    }
}
