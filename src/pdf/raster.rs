//! Raster metadata and exact PNG/JPEG samples (`raster.py`).
use crate::core::errors::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Cursor, sync::Arc};
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RasterInfo {
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub depth: u8,
    pub dpi: (i64, i64),
    pub grayscale: bool,
    pub alpha: bool,
}
#[derive(Clone, Debug)]
pub struct RasterPayload {
    pub data: Arc<[u8]>,
    pub depth: u8,
    pub colorspace: String,
    pub filter: String,
    pub mask: Option<Arc<[u8]>>,
    pub palette: Option<Arc<[u8]>>,
}
/// Non-semantic memoization of exact PNG/JPEG decoding. Shared across engine
/// snapshots; entries contain only immutable bytes, never PDF object numbers.
#[derive(Debug, Default)]
pub struct RasterCache {
    entries: BTreeMap<[u8; 32], (RasterPayload, u64)>,
    bytes: usize,
    tick: u64,
}
impl RasterCache {
    pub fn decode(
        &mut self,
        source: &[u8],
        jpeg: bool,
        high: bool,
        alpha: bool,
        gamma: Option<f64>,
    ) -> Result<RasterPayload> {
        #[cfg(feature = "latency-diagnostics")]
        let _hash_diag = crate::latency_diagnostics::phase("raster_key_hash", "", None);
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::count("hashed_source_bytes", source.len());
        let mut hash = Sha256::new();
        hash.update(b"retex-raster-v1\0");
        hash.update([jpeg as u8, high as u8, alpha as u8, gamma.is_some() as u8]);
        hash.update(gamma.unwrap_or_default().to_bits().to_le_bytes());
        hash.update(source);
        let key = hash.finalize().into();
        #[cfg(feature = "latency-diagnostics")]
        drop(_hash_diag);
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase("raster_cache_lookup_decode", "", None);
        self.tick = self.tick.wrapping_add(1);
        if let Some((payload, age)) = self.entries.get_mut(&key) {
            *age = self.tick;
            #[cfg(feature = "latency-diagnostics")]
            {
                crate::latency_diagnostics::count("hits", 1);
                crate::latency_diagnostics::count("decoded_bytes_reused", Self::size(payload));
            }
            return Ok(payload.clone());
        }
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::count("misses", 1);
        let payload = if jpeg {
            decode_jpeg(source)?
        } else {
            decode_png(source, high, alpha, gamma)?
        };
        let size = Self::size(&payload);
        if size <= 32 * 1024 * 1024 {
            while self.bytes + size > 32 * 1024 * 1024 || self.entries.len() >= 128 {
                let oldest = *self
                    .entries
                    .iter()
                    .min_by_key(|(_, (_, age))| *age)
                    .unwrap()
                    .0;
                self.bytes -= Self::size(&self.entries.remove(&oldest).unwrap().0);
            }
            self.bytes += size;
            self.entries.insert(key, (payload.clone(), self.tick));
        }
        Ok(payload)
    }
    fn size(payload: &RasterPayload) -> usize {
        payload.data.len()
            + payload.mask.as_ref().map_or(0, |v| v.len())
            + payload.palette.as_ref().map_or(0, |v| v.len())
    }
}
#[derive(Clone, Debug)]
pub struct PngChunk {
    pub kind: [u8; 4],
    pub data: Vec<u8>,
}
pub fn png_chunks(data: &[u8]) -> Result<Vec<PngChunk>> {
    if !data.starts_with(PNG_SIGNATURE) {
        return Err(
            Error::value("Invalid PNG signature").with_issue(crate::core::recovery::Issue::Image)
        );
    }
    let mut offset = 8;
    let mut result = Vec::new();
    while offset < data.len() {
        let header = data.get(offset..offset + 8).ok_or_else(|| {
            Error::value("Truncated PNG chunk header")
                .with_issue(crate::core::recovery::Issue::Image)
        })?;
        let size = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let end = offset
            .checked_add(size)
            .and_then(|end| end.checked_add(12))
            .ok_or_else(|| {
                Error::value("PNG chunk size overflow")
                    .with_issue(crate::core::recovery::Issue::Image)
            })?;
        let chunk = data.get(offset..end).ok_or_else(|| {
            Error::value("Truncated PNG chunk").with_issue(crate::core::recovery::Issue::Image)
        })?;
        let expected = u32::from_be_bytes(chunk[size + 8..].try_into().unwrap());
        if crc32fast::hash(&chunk[4..size + 8]) != expected {
            return Err(Error::value("PNG chunk checksum mismatch")
                .with_issue(crate::core::recovery::Issue::Image));
        }
        let kind = chunk[4..8].try_into().unwrap();
        result.push(PngChunk {
            kind,
            data: chunk[8..size + 8].to_vec(),
        });
        offset = end;
        if kind == *b"IEND" {
            return Ok(result);
        }
    }
    Err(Error::value("Missing PNG IEND chunk").with_issue(crate::core::recovery::Issue::Image))
}
fn png_decoder(data: &[u8], expand: bool) -> png::Decoder<Cursor<&[u8]>> {
    let mut limits = png::Limits::default();
    limits.bytes = usize::MAX;
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(data), limits);
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    if expand {
        decoder.set_transformations(png::Transformations::EXPAND)
    }
    decoder
}
fn png_error(error: png::DecodingError) -> Error {
    Error::value(format!("Invalid PNG image: {error}"))
}
pub(super) fn jpeg_segments(data: &[u8]) -> Result<Vec<(u8, &[u8])>> {
    if !data.starts_with(b"\xff\xd8") {
        return Err(
            Error::value("Invalid JPEG signature").with_issue(crate::core::recovery::Issue::Image)
        );
    }
    let mut offset = 2;
    let mut result = Vec::new();
    while offset < data.len() {
        if data[offset] != 255 {
            return Err(
                Error::value("Invalid JPEG marker").with_issue(crate::core::recovery::Issue::Image)
            );
        }
        while data.get(offset) == Some(&255) {
            offset += 1
        }
        let marker = *data.get(offset).ok_or_else(|| {
            Error::value("Truncated JPEG marker").with_issue(crate::core::recovery::Issue::Image)
        })?;
        offset += 1;
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
            continue;
        }
        let bytes = data.get(offset..offset + 2).ok_or_else(|| {
            Error::value("Truncated JPEG length").with_issue(crate::core::recovery::Issue::Image)
        })?;
        let length = u16::from_be_bytes(bytes.try_into().unwrap()) as usize;
        if length < 2 {
            return Err(Error::value("Invalid JPEG segment length")
                .with_issue(crate::core::recovery::Issue::Image));
        }
        let payload = data.get(offset + 2..offset + length).ok_or_else(|| {
            Error::value("Truncated JPEG segment").with_issue(crate::core::recovery::Issue::Image)
        })?;
        result.push((marker, payload));
        offset += length;
    }
    Ok(result)
}
pub fn metadata(data: &[u8]) -> Result<RasterInfo> {
    #[cfg(feature = "latency-diagnostics")]
    let _diag = crate::latency_diagnostics::phase("image_metadata", "", None);
    #[cfg(feature = "latency-diagnostics")]
    crate::latency_diagnostics::count("source_bytes", data.len());
    if data.starts_with(PNG_SIGNATURE) {
        let reader = png_decoder(data, false).read_info().map_err(png_error)?;
        let info = reader.info();
        let chunks = png_chunks(data)?;
        let physical = chunks
            .iter()
            .find(|chunk| chunk.kind == *b"pHYs")
            .filter(|chunk| chunk.data.len() == 9 && chunk.data[8] == 1);
        let dpi = physical.map_or((0, 0), |chunk| {
            let x = u32::from_be_bytes(chunk.data[..4].try_into().unwrap());
            let y = u32::from_be_bytes(chunk.data[4..8].try_into().unwrap());
            (
                (x as f64 * 0.0254 + 0.5) as i64,
                (y as f64 * 0.0254 + 0.5) as i64,
            )
        });
        return Ok(RasterInfo {
            format: "PNG".into(),
            width: info.width,
            height: info.height,
            depth: info.bit_depth as u8,
            dpi,
            grayscale: matches!(
                info.color_type,
                png::ColorType::Grayscale | png::ColorType::GrayscaleAlpha
            ),
            alpha: matches!(
                info.color_type,
                png::ColorType::GrayscaleAlpha | png::ColorType::Rgba
            ),
        });
    }
    if data.starts_with(b"\xff\xd8") {
        let segments = jpeg_segments(data)?;
        let frame = segments
            .iter()
            .find(|(marker, _)| matches!(marker,0xc0..=0xc3|0xc5..=0xc7|0xc9..=0xcb|0xcd..=0xcf))
            .map(|(_, data)| *data)
            .filter(|frame| frame.len() >= 6)
            .ok_or_else(|| {
                Error::value("Missing JPEG frame header")
                    .with_issue(crate::core::recovery::Issue::Image)
            })?;
        let height = u16::from_be_bytes(frame[1..3].try_into().unwrap()) as u32;
        let width = u16::from_be_bytes(frame[3..5].try_into().unwrap()) as u32;
        let dpi = segments
            .iter()
            .find(|(marker, payload)| {
                *marker == 0xe0 && payload.starts_with(b"JFIF\0") && payload.len() >= 12
            })
            .map_or((0, 0), |(_, payload)| {
                let x = u16::from_be_bytes(payload[8..10].try_into().unwrap()) as i64;
                let y = u16::from_be_bytes(payload[10..12].try_into().unwrap()) as i64;
                match payload[7] {
                    1 => (x, y),
                    2 => ((x as f64 * 2.54) as i64, (y as f64 * 2.54) as i64),
                    _ => (0, 0),
                }
            });
        return Ok(RasterInfo {
            format: "JPEG".into(),
            width,
            height,
            depth: 8,
            dpi,
            grayscale: frame[5] == 1,
            alpha: false,
        });
    }
    Err(
        Error::value("Unsupported image format; expected PNG or JPEG")
            .with_issue(crate::core::recovery::Issue::Image),
    )
}
pub fn gamma_table(depth: u8, exponent: f64) -> Vec<u16> {
    let maximum = (1_u32 << depth) - 1;
    (0..=maximum)
        .map(|value| (maximum as f64 * (value as f64 / maximum as f64).powf(exponent) + 0.5) as u16)
        .collect()
}
pub fn decode_png(
    data: &[u8],
    high_color: bool,
    allow_alpha: bool,
    gamma: Option<f64>,
) -> Result<RasterPayload> {
    let chunks = png_chunks(data)?;
    let info = metadata(data)?;
    let header = chunks
        .iter()
        .find(|chunk| chunk.kind == *b"IHDR")
        .ok_or_else(|| {
            Error::value("Missing PNG header").with_issue(crate::core::recovery::Issue::Image)
        })?;
    let interlaced = header.data.get(12).copied().unwrap_or(1) != 0;
    let alpha = info.alpha || chunks.iter().any(|chunk| chunk.kind == *b"tRNS");
    let palette = chunks
        .iter()
        .find(|chunk| chunk.kind == *b"PLTE")
        .map(|chunk| chunk.data.clone());
    let colorspace = if info.grayscale {
        "/DeviceGray"
    } else {
        "/DeviceRGB"
    }
    .to_string();
    let gamma = gamma.filter(|&gamma| gamma != 0.0);
    if !alpha && gamma.is_none() && !interlaced && (info.depth != 16 || high_color) {
        let compressed: Vec<u8> = chunks
            .iter()
            .filter(|chunk| chunk.kind == *b"IDAT")
            .flat_map(|chunk| chunk.data.iter().copied())
            .collect();
        let colors = if info.grayscale || palette.is_some() {
            1
        } else {
            3
        };
        return Ok(RasterPayload {
            data: compressed.into(),
            depth: info.depth,
            colorspace,
            filter: format!(
                "/Filter /FlateDecode /DecodeParms << /Predictor 15 /Colors {colors} /BitsPerComponent {} /Columns {} >>",
                info.depth, info.width
            ),
            mask: None,
            palette: palette.map(Into::into),
        });
    }
    let mut reader = png_decoder(data, true).read_info().map_err(png_error)?;
    let size = reader.output_buffer_size().ok_or_else(|| {
        Error::value("PNG samples exceed host address range")
            .with_issue(crate::core::recovery::Issue::Image)
    })?;
    let mut output = vec![0; size];
    let decoded = reader.next_frame(&mut output).map_err(png_error)?;
    output.truncate(decoded.buffer_size());
    let planes = match decoded.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => {
            return Err(Error::value("PNG palette expansion failed")
                .with_issue(crate::core::recovery::Issue::Image));
        }
    };
    let color_planes = if info.grayscale { 1 } else { 3 };
    let sixteen = decoded.bit_depth == png::BitDepth::Sixteen;
    let output_depth = if sixteen && high_color { 16 } else { 8 };
    let sample_depth = if info.depth < 8 && alpha && info.grayscale {
        info.depth
    } else if sixteen {
        16
    } else {
        8
    };
    let table = gamma.map(|gamma| gamma_table(sample_depth, gamma));
    let pixel_size = planes * if sixteen { 2 } else { 1 };
    let mut colors = Vec::new();
    let mut mask = if alpha && allow_alpha {
        Some(Vec::new())
    } else {
        None
    };
    for pixel in output.chunks_exact(pixel_size) {
        for channel in 0..color_planes {
            let original = if sixteen {
                u16::from_be_bytes(pixel[2 * channel..2 * channel + 2].try_into().unwrap())
            } else if sample_depth < 8 {
                (pixel[channel] as u16 * ((1_u16 << sample_depth) - 1) + 127) / 255
            } else {
                pixel[channel] as u16
            };
            let value = table
                .as_ref()
                .map_or(original, |table| table[original as usize]);
            if output_depth == 16 {
                colors.extend(value.to_be_bytes())
            } else if sixteen {
                colors.push((value >> 8) as u8)
            } else if sample_depth < 8 {
                colors
                    .push((value as f64 * 255.0 / ((1_u16 << sample_depth) - 1) as f64 + 0.5) as u8)
            } else {
                colors.push(value as u8)
            }
        }
        if let Some(mask) = &mut mask {
            let index = color_planes * if sixteen { 2 } else { 1 };
            mask.push(pixel[index]);
        }
    }
    Ok(RasterPayload {
        data: colors.into(),
        depth: output_depth,
        colorspace,
        filter: String::new(),
        mask: mask.map(Into::into),
        palette: None,
    })
}
pub fn decode_jpeg(data: &[u8]) -> Result<RasterPayload> {
    let segments = jpeg_segments(data)?;
    let frame = segments
        .iter()
        .find(|(marker, _)| matches!(marker,0xc0..=0xc3|0xc5..=0xc7|0xc9..=0xcb|0xcd..=0xcf))
        .map(|(_, data)| *data)
        .filter(|data| data.len() >= 6)
        .ok_or_else(|| {
            Error::value("Missing JPEG frame header")
                .with_issue(crate::core::recovery::Issue::Image)
        })?;
    let colorspace = match frame[5] {
        1 => "/DeviceGray",
        3 => "/DeviceRGB",
        4 => "/DeviceCMYK",
        _ => {
            return Err(Error::value("Unsupported JPEG color space")
                .with_issue(crate::core::recovery::Issue::Image));
        }
    };
    let adobe = segments
        .iter()
        .any(|(marker, payload)| *marker == 0xee && payload.starts_with(b"Adobe"));
    let filter = if frame[5] == 4 && adobe {
        "/Filter /DCTDecode /Decode [1 0 1 0 1 0 1 0]"
    } else {
        "/Filter /DCTDecode"
    };
    Ok(RasterPayload {
        data: data.into(),
        depth: 8,
        colorspace: colorspace.into(),
        filter: filter.into(),
        mask: None,
        palette: None,
    })
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    fn png_bytes(sample: u8) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[sample, 64, 128, 180])
                .unwrap();
        }
        bytes
    }
    #[test]
    fn raster_cache_uses_complete_policy_and_source_with_bounded_shared_payloads() {
        let mut cache = RasterCache::default();
        let source = png_bytes(128);
        let first = cache.decode(&source, false, false, true, None).unwrap();
        let hit = cache.decode(&source, false, false, true, None).unwrap();
        assert!(Arc::ptr_eq(&first.data, &hit.data));
        for (high, alpha, gamma) in [
            (true, true, None),
            (false, false, None),
            (false, true, Some(2.0)),
        ] {
            let cached = cache.decode(&source, false, high, alpha, gamma).unwrap();
            let fresh = decode_png(&source, high, alpha, gamma).unwrap();
            assert_eq!(cached.data, fresh.data);
            assert_eq!(cached.mask, fresh.mask);
            assert!(!Arc::ptr_eq(&first.data, &cached.data));
        }
        for sample in 0..=255 {
            cache
                .decode(&png_bytes(sample), false, false, true, None)
                .unwrap();
            assert!(cache.entries.len() <= 128 && cache.bytes <= 32 * 1024 * 1024);
        }
        let restored = cache.decode(&source, false, false, true, None).unwrap();
        assert_eq!(restored.data, first.data);
        assert_eq!(restored.mask, first.mask);
    }
}
