//! Raster paint from the already emitted PDF image data. No PDF serialization.
use crate::{
    core::errors::{Error, Result},
    pdf::{document::PDFDocument, image_records::ImageUse, parsing::PdfValue},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{io::Read, sync::Arc};
#[derive(Clone, Serialize)]
pub struct PreviewImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
    pub interpolate: bool,
    /// Device samples and display conversion are different observations. `rgba`
    /// retains the decoded sample conversion used by the independent extractor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_cmyk: Option<DeviceCmykPreview>,
}
#[derive(Clone, Serialize)]
pub struct DeviceCmykPreview {
    pub profile: &'static str,
    pub raw_samples_sha256: String,
    pub display_rgba: Arc<[u8]>,
}
impl PreviewImage {
    pub(super) fn byte_len(&self) -> usize {
        self.rgba.len()
            + self
                .device_cmyk
                .as_ref()
                .map_or(0, |color| color.display_rgba.len())
    }
}
/// Pure decoded resources, shared by scenes without copying samples. Exact
/// identities include the emitted dictionary, encoded bytes, palette/mask bytes
/// and the fixed display profile. Retained keys keep their bytes alive, so pointer
/// reuse cannot alias a previous resource. No execution or object state is reused.
struct CachedImage {
    dictionary: PdfValue,
    data: Arc<[u8]>,
    references: Vec<Arc<[u8]>>,
    identity: String,
    image: PreviewImage,
    age: u64,
    bytes: usize,
}
#[derive(Default)]
pub struct ImageCache {
    entries: Vec<CachedImage>,
    bytes: usize,
    tick: u64,
    hits: u64,
}
impl ImageCache {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn hits(&self) -> u64 {
        self.hits
    }
    pub(super) fn capture_stream(
        &mut self,
        doc: &PDFDocument,
        dictionary: &PdfValue,
        data: &[u8],
    ) -> Result<(String, PreviewImage)> {
        self.capture_stream_with_owner(doc, dictionary, data, None)
    }
    fn capture_stream_with_owner(
        &mut self,
        doc: &PDFDocument,
        dictionary: &PdfValue,
        data: &[u8],
        owner: Option<&Arc<[u8]>>,
    ) -> Result<(String, PreviewImage)> {
        #[cfg(feature = "latency-diagnostics")]
        let key_diag = crate::latency_diagnostics::phase("preview_image_key", "", None);
        let palette = match dictionary.get("ColorSpace") {
            Some(PdfValue::Array(values)) if values.len() == 4 => Some(&values[3]),
            _ => None,
        };
        let mut references = Vec::new();
        // These are precisely the indirect objects consulted by the decoder.
        for value in [palette, dictionary.get("SMask")].into_iter().flatten() {
            if let PdfValue::Reference(number, 0) = value {
                references.push(
                    doc.imported_objects
                        .get(&(*number as usize))
                        .and_then(Option::as_ref)
                        .ok_or_else(|| Error::value("Missing preview image resource"))?
                        .clone(),
                );
            }
        }
        // PDF object numbers are placements in a particular emission, not
        // decoded-sample inputs. Compare their complete resolved bytes instead.
        let mut normalized = dictionary.clone();
        if let PdfValue::Dictionary(entries)
        | PdfValue::Stream {
            dictionary: entries,
            ..
        } = &mut normalized
        {
            for (name, value) in entries {
                if name == "SMask" && matches!(value, PdfValue::Reference(_, 0)) {
                    *value = PdfValue::Null;
                }
                if name == "ColorSpace" {
                    if let PdfValue::Array(values) = value {
                        if values.len() == 4 && matches!(values[3], PdfValue::Reference(_, 0)) {
                            values[3] = PdfValue::Null;
                        }
                    }
                }
            }
        }
        #[cfg(feature = "latency-diagnostics")]
        drop(key_diag);
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase("preview_image_lookup_decode", "", None);
        self.tick = self.tick.wrapping_add(1);
        if let Some(entry) = self.entries.iter_mut().find(|entry| {
            entry.dictionary == normalized
                && entry.references == references
                && (owner.is_some_and(|owner| Arc::ptr_eq(owner, &entry.data))
                    || entry.data.as_ref() == data)
        }) {
            entry.age = self.tick;
            self.hits += 1;
            #[cfg(feature = "latency-diagnostics")]
            {
                crate::latency_diagnostics::count("hits", 1);
                crate::latency_diagnostics::count("decoded_bytes_reused", entry.image.byte_len());
            }
            return Ok((entry.identity.clone(), entry.image.clone()));
        }
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::count("misses", 1);
        let (identity, image) = capture_stream(doc, dictionary, data)?;
        // Include the complete identity in the memory budget, including inline
        // stream/string bytes. Over-budget resources remain correct uncached.
        let size = image.byte_len()
            + data.len()
            + references.iter().map(|v| v.len()).sum::<usize>()
            + bincode::serialized_size(dictionary).map_err(|e| Error::value(e.to_string()))?
                as usize;
        if size <= 32 * 1024 * 1024 {
            while self.bytes + size > 32 * 1024 * 1024 || self.entries.len() >= 128 {
                let oldest = self
                    .entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, entry)| entry.age)
                    .unwrap()
                    .0;
                self.bytes -= self.entries.remove(oldest).bytes;
            }
            self.bytes += size;
            self.entries.push(CachedImage {
                dictionary: normalized,
                data: owner.cloned().unwrap_or_else(|| data.into()),
                references,
                identity: identity.clone(),
                image: image.clone(),
                age: self.tick,
                bytes: size,
            });
        }
        Ok((identity, image))
    }
}
fn integer(value: &PdfValue, key: &str) -> Result<usize> {
    match value.get(key) {
        Some(PdfValue::Integer(n)) if *n > 0 => {
            usize::try_from(*n).map_err(|_| Error::value("Preview image dimension overflow"))
        }
        _ => Err(Error::value(format!("Invalid preview image {key}"))),
    }
}
pub(super) fn imported(doc: &PDFDocument, value: &PdfValue) -> Result<PdfValue> {
    match value {
        PdfValue::Reference(number, 0) => {
            let bytes = doc
                .imported_objects
                .get(&(*number as usize))
                .and_then(Option::as_ref)
                .ok_or_else(|| Error::value("Missing preview image resource"))?;
            if bytes.len() > 32 * 1024 * 1024 {
                return Err(Error::value("Preview imported image byte budget exceeded"));
            }
            crate::pdf::parsing::parse_indirect_body(bytes)
        }
        _ => Ok(value.clone()),
    }
}

pub(super) fn raw_stream(value: &PdfValue) -> Result<Vec<u8>> {
    let PdfValue::Stream { dictionary, data } = value else {
        return Err(Error::value("Preview image stream required"));
    };
    if data.len() > 32 * 1024 * 1024 {
        return Err(Error::value("Preview image stream byte budget exceeded"));
    }
    let dictionary = PdfValue::Dictionary(dictionary.clone());
    let Some(filter) = dictionary.get("Filter") else {
        return Ok(data.clone());
    };
    if filter != &PdfValue::Name("FlateDecode".into()) {
        return Err(Error::value("Preview image filter is not implemented"));
    }
    let mut output = Vec::new();
    flate2::read::ZlibDecoder::new(data.as_slice())
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut output)
        .map_err(|e| Error::value(e.to_string()))?;
    if output.len() > 32 * 1024 * 1024 {
        return Err(Error::value("Preview decoded image byte budget exceeded"));
    }
    if let Some(parameters) = dictionary.get("DecodeParms") {
        let predictor = match parameters.get("Predictor") {
            Some(PdfValue::Integer(n)) => *n,
            None => 1,
            _ => return Err(Error::value("Invalid preview image predictor")),
        };
        if predictor != 1 {
            return Err(Error::value(
                "Preview image predictor requires sample decoding",
            ));
        }
    }
    Ok(output)
}
fn samples(
    dictionary: &PdfValue,
    data: &[u8],
    colors: usize,
    width: usize,
    height: usize,
    depth: usize,
) -> Result<Vec<u16>> {
    if data.len() > 32 * 1024 * 1024 {
        return Err(Error::value("Preview encoded image byte budget exceeded"));
    }
    if dictionary.get("Filter") == Some(&PdfValue::Name("DCTDecode".into())) {
        if depth != 8 || !matches!(colors, 1 | 3 | 4) {
            return Err(Error::value("Preview JPEG color/depth is not implemented"));
        }
        let mut decoder = jpeg_decoder::Decoder::new(data);
        decoder.set_max_decoding_buffer_size(32 * 1024 * 1024);
        if let Some(parameters) = dictionary.get("DecodeParms") {
            for (key, _) in parameters.entries()? {
                if key != "ColorTransform" {
                    return Err(Error::value(
                        "Preview JPEG decode parameter is not implemented",
                    ));
                }
            }
            if let Some(transform) = parameters.get("ColorTransform") {
                let transform = match (colors, transform) {
                    (1, PdfValue::Integer(0 | 1)) => jpeg_decoder::ColorTransform::Grayscale,
                    (3, PdfValue::Integer(0)) => jpeg_decoder::ColorTransform::RGB,
                    (3, PdfValue::Integer(1)) => jpeg_decoder::ColorTransform::YCbCr,
                    (4, PdfValue::Integer(0)) => jpeg_decoder::ColorTransform::CMYK,
                    (4, PdfValue::Integer(1)) => jpeg_decoder::ColorTransform::YCCK,
                    _ => return Err(Error::value("Invalid preview JPEG color transform")),
                };
                // PDF Reference 1.7 table 3.11: an Adobe marker overrides
                // the dictionary's transform. Let the decoder honor it.
                let adobe = super::raster::jpeg_segments(data)?
                    .iter()
                    .any(|(marker, payload)| {
                        *marker == 0xee && payload.starts_with(b"Adobe") && payload.len() >= 12
                    });
                if !adobe {
                    decoder.set_color_transform(transform);
                }
            }
        }
        decoder
            .read_info()
            .map_err(|e| Error::value(e.to_string()))?;
        let info = decoder
            .info()
            .ok_or_else(|| Error::value("Missing preview JPEG metadata"))?;
        if (info.width as usize, info.height as usize) != (width, height)
            || info.pixel_format.pixel_bytes() != colors
        {
            return Err(Error::value(
                "Preview JPEG dictionary/sample geometry disagrees",
            ));
        }
        let decoded = decoder.decode().map_err(|e| Error::value(e.to_string()))?;
        if decoded.len() != width * height * colors {
            return Err(Error::value("Invalid preview JPEG sample length"));
        }
        // jpeg-decoder exposes normalized CMYK; PDF Decode applies to the raw
        // decoded DCT samples. Undo its four-channel complement before Decode.
        return Ok(decoded
            .into_iter()
            .map(|value| u16::from(if colors == 4 { 255 - value } else { value }))
            .collect());
    }
    if ![1, 2, 4, 8, 16].contains(&depth) {
        return Err(Error::value("Unsupported preview image sample depth"));
    }
    let row_bits = width
        .checked_mul(colors)
        .and_then(|n| n.checked_mul(depth))
        .ok_or_else(|| Error::value("Preview image dimension overflow"))?;
    let row_bytes = row_bits.div_ceil(8);
    let mut decoded = if dictionary.get("Filter").is_some() {
        let filter = dictionary.get("Filter").unwrap();
        if filter != &PdfValue::Name("FlateDecode".into()) {
            return Err(Error::value("Preview image filter is not implemented"));
        }
        let mut decoded = Vec::new();
        flate2::read::ZlibDecoder::new(data)
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut decoded)
            .map_err(|e| Error::value(e.to_string()))?;
        if decoded.len() > 32 * 1024 * 1024 {
            return Err(Error::value("Preview decoded image byte budget exceeded"));
        }
        decoded
    } else {
        data.to_vec()
    };
    let parameters = dictionary.get("DecodeParms");
    let predictor = parameters
        .and_then(|p| p.get("Predictor"))
        .map_or(Ok(1), |v| match v {
            PdfValue::Integer(n) => Ok(*n),
            _ => Err(Error::value("Invalid preview image predictor")),
        })?;
    if (10..=15).contains(&predictor) {
        let p = parameters.unwrap();
        if integer(p, "Columns")? != width
            || integer(p, "Colors")? != colors
            || integer(p, "BitsPerComponent")? != depth
        {
            return Err(Error::value("Invalid preview image predictor geometry"));
        }
        if decoded.len() != (row_bytes + 1) * height {
            return Err(Error::value("Invalid preview image predictor length"));
        }
        let bpp = (colors * depth).div_ceil(8);
        let mut output = Vec::with_capacity(row_bytes * height);
        let mut previous = vec![0u8; row_bytes];
        for row in decoded.chunks_exact(row_bytes + 1) {
            let mut current = row[1..].to_vec();
            for i in 0..row_bytes {
                let left = if i >= bpp { current[i - bpp] } else { 0 };
                let above = previous[i];
                let corner = if i >= bpp { previous[i - bpp] } else { 0 };
                let add = match row[0] {
                    0 => 0,
                    1 => left,
                    2 => above,
                    3 => ((left as u16 + above as u16) / 2) as u8,
                    4 => {
                        let p = left as i32 + above as i32 - corner as i32;
                        let a = (p - left as i32).abs();
                        let b = (p - above as i32).abs();
                        let c = (p - corner as i32).abs();
                        if a <= b && a <= c {
                            left
                        } else if b <= c {
                            above
                        } else {
                            corner
                        }
                    }
                    _ => return Err(Error::value("Invalid preview PNG row filter")),
                };
                current[i] = current[i].wrapping_add(add);
            }
            output.extend_from_slice(&current);
            previous = current;
        }
        decoded = output;
    } else if predictor != 1 {
        return Err(Error::value("Preview image predictor is not implemented"));
    }
    if decoded.len() != row_bytes * height {
        return Err(Error::value("Invalid preview image sample length"));
    }
    let mut result = Vec::with_capacity(width * height * colors);
    for row in decoded.chunks_exact(row_bytes) {
        for sample in 0..width * colors {
            let bit = sample * depth;
            let value = if depth == 16 {
                u16::from_be_bytes([row[bit / 8], row[bit / 8 + 1]])
            } else {
                ((row[bit / 8] >> (8 - depth - bit % 8)) & ((1u16 << depth) - 1) as u8) as u16
            };
            result.push(value);
        }
    }
    Ok(result)
}
pub fn capture(doc: &PDFDocument, image: &ImageUse) -> Result<(String, PreviewImage)> {
    capture_cached(doc, image, &mut ImageCache::default())
}
pub fn capture_cached(
    doc: &PDFDocument,
    image: &ImageUse,
    cache: &mut ImageCache,
) -> Result<(String, PreviewImage)> {
    if !matches!(image.format.as_str(), "PNG" | "JPEG") {
        return Err(Error::value(format!(
            "Retained {} image painting is not implemented",
            image.format
        )));
    }
    let attributes = crate::pdf::parsing::dictionary(&image.attributes)?;
    for (key, _) in attributes.entries()? {
        if matches!(
            key.as_str(),
            "Type"
                | "Subtype"
                | "Width"
                | "Height"
                | "BitsPerComponent"
                | "ColorSpace"
                | "Filter"
                | "DecodeParms"
                | "SMask"
                | "Decode"
                | "Length"
        ) {
            return Err(Error::value(
                "Preview image dictionary override is not implemented",
            ));
        }
    }
    // Emission already incorporated attributes into this frozen dictionary.
    let dictionary = crate::pdf::parsing::dictionary(&image.dictionary)?;
    let data = image
        .data
        .as_ref()
        .ok_or_else(|| Error::value("Image has not been emitted"))?;
    cache.capture_stream_with_owner(doc, &dictionary, data, Some(data))
}
pub(super) fn capture_stream(
    doc: &PDFDocument,
    dictionary: &PdfValue,
    data: &[u8],
) -> Result<(String, PreviewImage)> {
    let width = integer(&dictionary, "Width")?;
    let height = integer(&dictionary, "Height")?;
    if width > 16384 || height > 16384 {
        return Err(Error::value("Preview image dimension budget exceeded"));
    }
    let depth = integer(&dictionary, "BitsPerComponent")?;
    let pixels = width
        .checked_mul(height)
        .filter(|n| *n <= 4 * 1024 * 1024)
        .ok_or_else(|| Error::value("Preview image pixel budget exceeded"))?;
    for key in ["Mask", "ImageMask", "Intent", "OC", "Alternates"] {
        if dictionary.get(key).is_some() {
            return Err(Error::value(format!(
                "Preview image {key} is not implemented"
            )));
        }
    }
    let colorspace = dictionary
        .get("ColorSpace")
        .ok_or_else(|| Error::value("Missing preview image color space"))?;
    let (colors, palette) = match colorspace {
        PdfValue::Name(name) if name == "DeviceRGB" => (3, None),
        PdfValue::Name(name) if name == "DeviceGray" => (1, None),
        PdfValue::Name(name) if name == "DeviceCMYK" && depth == 8 => (4, None),
        PdfValue::Array(values)
            if values.len() == 4
                && values[0] == PdfValue::Name("Indexed".into())
                && values[1] == PdfValue::Name("DeviceRGB".into()) =>
        {
            let palette = imported(doc, &values[3])?;
            let bytes = match palette {
                PdfValue::String(bytes) => bytes,
                value => raw_stream(&value)?,
            };
            let PdfValue::Integer(last) = values[2] else {
                return Err(Error::value("Invalid preview palette size"));
            };
            if !(0..=255).contains(&last) || bytes.len() != (last as usize + 1) * 3 {
                return Err(Error::value("Invalid preview image palette"));
            }
            (1, Some(bytes))
        }
        _ => return Err(Error::value("Preview image color space is not implemented")),
    };
    let color_samples = samples(dictionary, data, colors, width, height, depth)?;
    let decode = decode_ranges(dictionary, colors, palette.is_some())?;
    let alpha = if let Some(mask) = dictionary.get("SMask") {
        let mask = imported(doc, mask)?;
        let PdfValue::Stream {
            dictionary: md,
            data,
        } = mask
        else {
            return Err(Error::value("Preview image soft mask stream required"));
        };
        let md = PdfValue::Dictionary(md);
        if integer(&md, "Width")? != width
            || integer(&md, "Height")? != height
            || integer(&md, "BitsPerComponent")? != 8
            || md.get("ColorSpace") != Some(&PdfValue::Name("DeviceGray".into()))
        {
            return Err(Error::value("Invalid preview image soft mask"));
        }
        let mask_decode = decode_ranges(&md, 1, false)?;
        samples(&md, &data, 1, width, height, 8)?
            .into_iter()
            .map(|value| u16::from(decoded_byte(value, 255, 0, mask_decode.as_deref())))
            .collect()
    } else {
        vec![255; pixels]
    };
    let maximum = (1u32 << depth) - 1;
    let mut rgba = Vec::with_capacity(pixels * 4);
    let mut display_rgba = (colors == 4).then(|| Vec::with_capacity(pixels * 4));
    let mut raw_hash = Sha256::new();
    for (index, pixel) in color_samples.chunks_exact(colors).enumerate() {
        if let Some(palette) = &palette {
            let at = pixel[0] as usize * 3;
            rgba.extend_from_slice(
                palette
                    .get(at..at + 3)
                    .ok_or_else(|| Error::value("Preview palette index out of range"))?,
            );
        } else if colors == 1 {
            rgba.extend_from_slice(&[decoded_byte(pixel[0], maximum, 0, decode.as_deref()); 3]);
        } else if colors == 4 {
            let components = std::array::from_fn(|channel| {
                decoded_byte(pixel[channel], maximum, channel, decode.as_deref())
            });
            raw_hash.update(components);
            // Preserve the independently extracted source RGB observation.
            let black = 255 - components[3] as u32;
            rgba.extend(
                components[..3]
                    .iter()
                    .map(|value| (((255 - *value as u32) * black + 127) / 255) as u8),
            );
            let display = display_rgba.as_mut().unwrap();
            display.extend_from_slice(&super::device_color::cmyk_image_rgb(components));
            display.push(alpha[index] as u8);
        } else {
            rgba.extend(
                pixel.iter().enumerate().map(|(channel, value)| {
                    decoded_byte(*value, maximum, channel, decode.as_deref())
                }),
            );
        }
        rgba.push(alpha[index] as u8);
    }
    let interpolate = match dictionary.get("Interpolate") {
        None => false,
        Some(PdfValue::Boolean(value)) => *value,
        _ => return Err(Error::value("Invalid preview image interpolation")),
    };
    let mut hash = Sha256::new();
    hash.update(b"retex-rs2-rgba-v1\0");
    hash.update((width as u32).to_le_bytes());
    hash.update((height as u32).to_le_bytes());
    hash.update([interpolate as u8]);
    hash.update(&rgba);
    let device_cmyk = display_rgba.map(|display_rgba| DeviceCmykPreview {
        profile: super::device_color::PROFILE,
        raw_samples_sha256: format!("{:x}", raw_hash.finalize()),
        display_rgba: display_rgba.into(),
    });
    if let Some(color) = &device_cmyk {
        hash.update(color.profile.as_bytes());
        hash.update(color.raw_samples_sha256.as_bytes());
        hash.update(&color.display_rgba);
    }
    Ok((
        format!("{:x}", hash.finalize()),
        PreviewImage {
            width: width as u32,
            height: height as u32,
            rgba: rgba.into(),
            interpolate,
            device_cmyk,
        },
    ))
}
fn decode_ranges(
    dictionary: &PdfValue,
    colors: usize,
    indexed: bool,
) -> Result<Option<Vec<[f64; 2]>>> {
    let Some(decode) = dictionary.get("Decode") else {
        return Ok(None);
    };
    if indexed {
        return Err(Error::value(
            "Preview indexed image Decode is not implemented",
        ));
    }
    let PdfValue::Array(values) = decode else {
        return Err(Error::value("Invalid preview image Decode array"));
    };
    if values.len() != colors * 2 {
        return Err(Error::value("Invalid preview image Decode component count"));
    }
    values
        .chunks_exact(2)
        .map(|pair| {
            let range = [pair[0].number()?, pair[1].number()?];
            if range.iter().any(|value| !value.is_finite()) {
                return Err(Error::value("Nonfinite preview image Decode value"));
            }
            Ok(range)
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}
fn decoded_byte(value: u16, maximum: u32, channel: usize, decode: Option<&[[f64; 2]]>) -> u8 {
    if let Some(ranges) = decode {
        let [low, high] = ranges[channel];
        ((low + value as f64 / maximum as f64 * (high - low)).clamp(0., 1.) * 255.).round() as u8
    } else {
        ((value as u32 * 255 + maximum / 2) / maximum) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn immutable_encoded_owners_shortcut_identity_without_allowing_mutable_aliases() {
        let doc = PDFDocument::default();
        let mut cache = ImageCache::default();
        let dictionary = crate::pdf::parsing::dictionary(
            "/Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray",
        )
        .unwrap();
        let mut data: Arc<[u8]> = Arc::from([64]);
        let first = cache
            .capture_stream_with_owner(&doc, &dictionary, &data, Some(&data))
            .unwrap();
        assert!(Arc::ptr_eq(&cache.entries[0].data, &data));
        let hit = cache
            .capture_stream_with_owner(&doc, &dictionary, &data, Some(&data))
            .unwrap();
        assert!(Arc::ptr_eq(&first.1.rgba, &hit.1.rgba));
        Arc::make_mut(&mut data)[0] = 128;
        let changed = cache
            .capture_stream_with_owner(&doc, &dictionary, &data, Some(&data))
            .unwrap();
        assert_ne!(first.0, changed.0);
        assert_eq!(first.1.rgba.as_ref(), &[64, 64, 64, 255]);
        assert_eq!(changed.1.rgba.as_ref(), &[128, 128, 128, 255]);
    }
    #[test]
    fn cached_samples_share_storage_and_invalidate_encoded_dictionary_mask_and_palette() {
        let mut doc = PDFDocument::default();
        let mut cache = ImageCache::default();
        let value = crate::pdf::parsing::dictionary(
            "/Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /SMask 7 0 R",
        )
        .unwrap();
        let mask = |sample| {
            PdfValue::Stream {
                dictionary: crate::pdf::parsing::dictionary(
                    "/Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray",
                )
                .unwrap()
                .entries()
                .unwrap()
                .to_vec(),
                data: vec![sample],
            }
            .serialize()
            .unwrap()
            .into()
        };
        doc.imported_objects.insert(7, Some(mask(128)));
        let first = cache.capture_stream(&doc, &value, &[64]).unwrap();
        let hit = cache.capture_stream(&doc, &value, &[64]).unwrap();
        assert_eq!(cache.hits(), 1);
        doc.imported_objects
            .insert(99, doc.imported_objects[&7].clone());
        let renumbered = crate::pdf::parsing::dictionary(
            "/Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /SMask 99 0 R",
        )
        .unwrap();
        let same = cache.capture_stream(&doc, &renumbered, &[64]).unwrap();
        assert_eq!(cache.hits(), 2);
        assert!(Arc::ptr_eq(&first.1.rgba, &same.1.rgba));
        assert!(Arc::ptr_eq(&first.1.rgba, &hit.1.rgba));
        doc.imported_objects.insert(7, Some(mask(255)));
        let changed = cache.capture_stream(&doc, &value, &[64]).unwrap();
        assert_ne!(changed.0, first.0);
        assert_eq!(
            changed.1.rgba.as_ref(),
            capture_stream(&doc, &value, &[64]).unwrap().1.rgba.as_ref()
        );
        assert_ne!(
            cache.capture_stream(&doc, &value, &[65]).unwrap().0,
            changed.0
        );
        let reverse = crate::pdf::parsing::dictionary("/Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /SMask 7 0 R /Decode [1 0] /Interpolate true").unwrap();
        assert_ne!(
            cache.capture_stream(&doc, &reverse, &[64]).unwrap().0,
            changed.0
        );
        let indexed = crate::pdf::parsing::dictionary(
            "/Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace [/Indexed /DeviceRGB 0 8 0 R]",
        )
        .unwrap();
        doc.imported_objects.insert(
            8,
            Some(
                PdfValue::String(vec![255, 0, 0])
                    .serialize()
                    .unwrap()
                    .into(),
            ),
        );
        let red = cache.capture_stream(&doc, &indexed, &[0]).unwrap();
        doc.imported_objects.insert(
            8,
            Some(
                PdfValue::String(vec![0, 255, 0])
                    .serialize()
                    .unwrap()
                    .into(),
            ),
        );
        let green = cache.capture_stream(&doc, &indexed, &[0]).unwrap();
        assert_ne!(red.0, green.0);
        assert_eq!(green.1.rgba.as_ref(), &[0, 255, 0, 255]);
        for i in 0..=255 {
            cache.capture_stream(&doc, &value, &[i]).unwrap();
            assert!(cache.entries.len() <= 128 && cache.bytes() <= 32 * 1024 * 1024);
        }
        assert_eq!(
            cache
                .capture_stream(&doc, &value, &[64])
                .unwrap()
                .1
                .rgba
                .as_ref(),
            capture_stream(&doc, &value, &[64]).unwrap().1.rgba.as_ref()
        );
    }
    #[test]
    fn emitted_decode_ranges_and_soft_mask_are_applied_and_invalid_ranges_rejected() {
        let doc = PDFDocument::default();
        let value = crate::pdf::parsing::dictionary(
            "/Width 4 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Decode [1 0]",
        )
        .unwrap();
        let (_, image) = capture_stream(&doc, &value, &[0, 64, 128, 255]).unwrap();
        assert_eq!(
            image.rgba.as_ref(),
            [
                255, 255, 255, 255, 191, 191, 191, 255, 127, 127, 127, 255, 0, 0, 0, 255
            ]
        );
        let mut value = value;
        let PdfValue::Dictionary(ref mut entries) = value else {
            unreachable!()
        };
        let mask = crate::pdf::parsing::dictionary(
            "/Width 4 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Decode [1 0]",
        )
        .unwrap();
        let PdfValue::Dictionary(mask) = mask else {
            unreachable!()
        };
        entries.push((
            "SMask".into(),
            PdfValue::Stream {
                dictionary: mask,
                data: vec![0, 64, 128, 255],
            },
        ));
        let (_, masked) = capture_stream(&doc, &value, &[0, 64, 128, 255]).unwrap();
        assert_eq!(
            masked.rgba.as_ref(),
            [
                255, 255, 255, 255, 191, 191, 191, 191, 127, 127, 127, 127, 0, 0, 0, 0
            ]
        );
        for decode in ["[1]", "[1 0 1 0]", "[/bad 0]"] {
            let invalid = crate::pdf::parsing::dictionary(&format!(
                "/Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Decode {decode}",
            ))
            .unwrap();
            assert!(capture_stream(&doc, &invalid, &[64]).is_err(), "{decode}");
        }
    }
}
