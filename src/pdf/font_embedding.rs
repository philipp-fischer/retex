//! Font descriptors, encodings, scalable outlines and Type3 glyphs (`font_embedding.py`).
use super::syntax::pdf_name;
use super::unicode_maps::UnicodeMaps;
use super::writer::PdfWriter;
use crate::core::errors::{Error, Result};
use crate::fonts::encoding::EncFileParser;
use crate::fonts::formats::bitmap::{BitmapKind, BitmapOptions, BitmapResource};
use crate::fonts::formats::sfnt::SfntResource;
use crate::fonts::formats::type1::{STANDARD_FONT_FLAGS, Type1Resource};
use crate::fonts::formats::type1_subsets::Type1Program;
use crate::fonts::materialization::FontResource;
use crate::fonts::model::FontInfo;
use crate::fonts::scaling::{divide_scaled, pdf_font_size};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
pub fn nonembedded_descriptor(font: &FontInfo, stem: i64) -> Result<String> {
    let size = pdf_font_size(font.size);
    let ascent = divide_scaled(font.get_char_height(b'h' as u32), size, 3)?;
    let cap = divide_scaled(font.get_char_height(b'H' as u32), size, 3)?;
    let descent = (-divide_scaled(font.get_char_depth(b'y' as u32), size, 3)?).min(0);
    let degrees = -(font.get_parameter(1) as f64 / 65536.0).atan().to_degrees();
    let angle = divide_scaled(degrees as i64, size, 3)?;
    let quad = divide_scaled(font.get_parameter(6), size, 3)?;
    let xheight = divide_scaled(font.get_parameter(5), size, 3)?;
    let flags = font.materialization.map_flags.unwrap_or_else(|| {
        STANDARD_FONT_FLAGS
            .iter()
            .find(|(name, _)| *name == font.materialization.pdf_name)
            .map_or(34, |(_, flags)| *flags)
    });
    Ok(format!(
        "/Flags {flags} /FontBBox [0 {descent} {quad} {}] /ItalicAngle {angle} /Ascent {ascent} /CapHeight {cap} /Descent {descent} /StemV {stem} /XHeight {xheight}",
        ascent.max(cap)
    ))
}
pub fn type1_descriptor(resource: &Type1Resource, default_stem: i64) -> String {
    let bbox = resource
        .bbox
        .iter()
        .map(|value| (*value as i64).to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let mut metrics = vec![
        ("Ascent".to_string(), resource.bbox[3] as i64),
        ("Descent".to_string(), resource.bbox[1] as i64),
        ("CapHeight".to_string(), resource.bbox[3] as i64),
        ("StemV".to_string(), default_stem),
    ];
    for (key, value) in &resource.descriptor_metrics {
        if let Some((_, existing)) = metrics.iter_mut().find(|(existing, _)| existing == key) {
            *existing = *value
        } else {
            metrics.push((key.clone(), *value))
        }
    }
    format!(
        "/Flags 4 /FontBBox [{bbox}] /ItalicAngle {} {}",
        resource.italic_angle as i64,
        metrics
            .iter()
            .map(|(key, value)| format!("/{key} {value}"))
            .collect::<Vec<_>>()
            .join(" ")
    )
}
pub fn sfnt_descriptor(resource: &SfntResource, stem: i64, font: &FontInfo) -> Result<String> {
    let mut result = nonembedded_descriptor(font, stem)?;
    if font.materialization.map_flags.is_none() {
        result = Regex::new(r"/Flags \d+")
            .unwrap()
            .replace_all(&result, "/Flags 4")
            .into_owned()
    }
    let bbox = resource
        .bbox
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    result = Regex::new(r"/FontBBox \[[^]]*\]")
        .unwrap()
        .replace_all(&result, format!("/FontBBox [{bbox}]"))
        .into_owned();
    let metrics = std::iter::once(("ItalicAngle".to_string(), resource.italic_angle as i64))
        .chain(resource.descriptor_metrics.iter().cloned());
    for (key, value) in metrics {
        let pattern = Regex::new(&format!("/{}\\s+-?[\\d.]+", regex::escape(&key))).unwrap();
        if Regex::new(&format!("/{}\\b", regex::escape(&key)))
            .unwrap()
            .is_match(&result)
        {
            result = pattern
                .replace_all(&result, format!("/{key} {value}"))
                .into_owned()
        } else {
            result.push_str(&format!(" /{key} {value}"))
        }
    }
    Ok(result)
}
pub fn encoding_dictionary(resource: &EncFileParser) -> Result<String> {
    if resource.glyph_names.is_empty() {
        return Ok(String::new());
    }
    let mut result = "<<\n  /Type /Encoding\n  /Differences [\n".to_string();
    for (index, chunk) in resource.glyph_names.chunks(8).enumerate() {
        let position = index * 8;
        if position % 16 == 0 {
            result.push_str(&format!("    {position}"))
        }
        let glyphs = chunk
            .iter()
            .map(|name| pdf_name(name).map(|name| format!("/{name}")))
            .collect::<Result<Vec<_>>>()?;
        result.push_str(&format!(" {}\n", glyphs.join(" ")));
    }
    result.push_str("  ]\n>>");
    Ok(result)
}
fn font_cmap(
    font: &FontInfo,
    maps: Option<&UnicodeMaps>,
    enabled: bool,
    warn: &mut dyn FnMut(&str),
) -> Option<Vec<u8>> {
    maps.and_then(|maps| {
        maps.cmap(
            &font.glyph_encoding(),
            &font.name,
            enabled,
            font.materialization.no_builtin_tounicode,
            warn,
        )
    })
}
pub fn write_bitmap_font(
    resource: &BitmapResource,
    pdf: &mut PdfWriter,
    font: &FontInfo,
    number: usize,
    codes: &BTreeSet<u32>,
    object_number: usize,
    options: &BitmapOptions,
    maps: Option<&UnicodeMaps>,
    unicode_enabled: bool,
    warn: &mut dyn FnMut(&str),
) -> Result<usize> {
    let scale = resource.font_scale(font.size, options)?;
    let mut procedures = BTreeMap::new();
    let mut widths = BTreeMap::new();
    let mut boxes = Vec::new();
    let names = font.materialization.encoding.as_ref().map_or_else(
        || vec![".notdef".into(); 256],
        |encoding| encoding.borrow().glyph_names.clone(),
    );
    for &code in codes {
        let Ok(octet) = u8::try_from(code) else {
            continue;
        };
        let Some(glyph) = resource.glyphs.get(&octet) else {
            continue;
        };
        let width = resource.char_width(font.size, octet, font.get_char_width(code), options)?;
        widths.insert(code, width);
        let (bbox, content) = if resource.kind == BitmapKind::Pk {
            let (mut w, mut h, mut xoff, mut yoff) =
                (glyph.width, glyph.height, glyph.xoff, glyph.yoff);
            let empty = w <= 0 || h <= 0;
            if empty {
                w = (width + 0.5) as i64;
                h = 1;
                xoff = 0;
                yoff = 0
            }
            let bbox = [-xoff, yoff - h + 1, w - xoff + 1, yoff + 1];
            let mut content = format!(
                "{width:.2} 0 {} d1\n",
                bbox.iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes();
            if !empty {
                content.extend(format!("q\n{w} 0 0 {h} {} {} cm\nBI\n/W {w}\n/H {h}\n/IM true\n/BPC 1\n/D [1 0]\nID ",bbox[0],bbox[1]).into_bytes());
                content.extend_from_slice(&glyph.raster);
                content.extend(b"\nEI\nQ\n")
            }
            (bbox, content)
        } else {
            let bbox = glyph
                .bbox
                .ok_or_else(|| Error::value("Missing PGC glyph bounds"))?;
            let mut content = format!(
                "{} 0 {} d1\nq\n",
                width as i64,
                bbox.iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes();
            content.extend_from_slice(&glyph.content);
            content.extend(b"Q\n");
            (bbox, content)
        };
        boxes.push(bbox);
        procedures.insert(code, pdf.add_stream_object(&content, "", None, None)?);
    }
    let first = *codes
        .first()
        .ok_or_else(|| Error::value("Empty bitmap font character set"))?;
    let last = *codes.last().unwrap();
    let values = (first..=last)
        .map(|code| {
            widths.get(&code).map_or_else(
                || "0".into(),
                |&width| {
                    if resource.kind == BitmapKind::Pgc {
                        (width as i64).to_string()
                    } else {
                        format!("{width:?}")
                    }
                },
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let widths_obj = pdf.add_text_object(&format!("[ {values} ]"), None, 1)?;
    let used_names = procedures
        .keys()
        .map(|&code| {
            (
                code,
                if names
                    .get(code as usize)
                    .is_some_and(|name| name != ".notdef")
                {
                    names[code as usize].clone()
                } else {
                    format!("a{code}")
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let differences = (first..=last)
        .map(|code| {
            pdf_name(used_names.get(&code).map_or(".notdef", String::as_str))
                .map(|name| format!("{code} /{name}"))
        })
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    let encoding = pdf.add_text_object(
        &format!("<< /Type /Encoding /Differences [{differences}] >>"),
        None,
        1,
    )?;
    let procedures = procedures
        .iter()
        .map(|(code, object)| {
            pdf_name(&used_names[code]).map(|name| format!("/{name} {object} 0 R"))
        })
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    let procs = pdf.add_text_object(&format!("<< {procedures} >>"), None, 1)?;
    let bbox = if boxes.is_empty() {
        [0; 4]
    } else {
        [
            boxes.iter().map(|b| b[0]).min().unwrap(),
            boxes.iter().map(|b| b[1]).min().unwrap(),
            boxes.iter().map(|b| b[2]).max().unwrap(),
            boxes.iter().map(|b| b[3]).max().unwrap(),
        ]
    };
    let cmap = if font.materialization.encoding.is_some() {
        font_cmap(font, maps, unicode_enabled, warn)
    } else {
        None
    };
    let unicode_ref = if let Some(cmap) = cmap {
        format!(
            " /ToUnicode {} 0 R",
            pdf.add_stream_object(&cmap, "", None, None)?
        )
    } else {
        String::new()
    };
    let bbox = bbox
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    pdf.add_text_object(&format!("<< /Type /Font /Subtype /Type3 /Name /F{number} /FontMatrix [{scale:?} 0 0 {scale:?} 0 0] /FontBBox [{bbox}] /Resources << /ProcSet [/PDF /ImageB] >> /FirstChar {first} /LastChar {last} /Widths {widths_obj} 0 R /Encoding {encoding} 0 R /CharProcs {procs} 0 R{unicode_ref} {} >>",font.materialization.font_attributes),Some(object_number),1)
}
pub fn create_embedded_font(
    pdf: &mut PdfWriter,
    font: &FontInfo,
    _font_num: usize,
    maps: Option<&UnicodeMaps>,
    used_codes: Option<&BTreeSet<u32>>,
    object_number: Option<usize>,
    omit_charset: bool,
    unicode_enabled: bool,
    warn: &mut dyn FnMut(&str),
) -> Result<usize> {
    let mut resource = font.materialization.resource.as_deref().cloned();
    if resource.is_none() && !font.materialization.nonembedded {
        return Ok(0);
    }
    let mut used = used_codes.cloned().unwrap_or_else(|| {
        font.tfm_widths
            .as_ref()
            .map_or_else(BTreeSet::new, |widths| widths.keys().copied().collect())
    });
    used.extend(font.materialization.included_codes.iter().copied());
    let mut font_name = font.materialization.pdf_name.clone();
    let mut charset = None;
    if font.materialization.subset {
        if let Some(original) = resource.take() {
            resource = Some(match original {
                FontResource::Type1(resource) => {
                    let encoding = font.glyph_encoding();
                    let names =
                        used.iter()
                            .map(|&code| {
                                encoding.get(code as usize).cloned().ok_or_else(|| {
                                    Error::value("Missing Type1 subset encoding slot")
                                })
                            })
                            .collect::<Result<Vec<_>>>()?;
                    let (subset, names) = Type1Program::new(resource)?.subset(names, warn)?;
                    charset = Some(names);
                    FontResource::Type1(subset)
                }
                FontResource::Sfnt(resource) => {
                    let encoding = font
                        .materialization
                        .encoding
                        .as_ref()
                        .map(|encoding| encoding.borrow().glyph_names.clone());
                    let codes = used
                        .iter()
                        .map(|&code| {
                            u8::try_from(code)
                                .map_err(|_| Error::value("Non-octet scalable subset code"))
                        })
                        .collect::<Result<BTreeSet<_>>>()?;
                    FontResource::Sfnt(
                        resource
                            .subset(
                                encoding.as_deref(),
                                &codes,
                                warn,
                                font.materialization.subfont.as_deref(),
                                font.materialization.pid_eid,
                            )?
                            .0,
                    )
                }
                FontResource::Bitmap(_) => {
                    return Err(Error::value("Bitmap font passed to scalable embedding"));
                }
            });
            font_name = resource.as_ref().unwrap().name().into();
        }
    }
    let kind = resource.as_ref().map_or("type1", FontResource::kind);
    let stem = divide_scaled(
        font.get_char_width(b'.' as u32).div_euclid(3),
        pdf_font_size(font.size),
        3,
    )?
    .max(1);
    let descriptor = if let Some(resource) = &resource {
        let dictionary = match resource {
            FontResource::Type1(resource) => format!(
                "/Length1 {} /Length2 {} /Length3 {}",
                resource.length1, resource.length2, resource.length3
            ),
            FontResource::Sfnt(resource)
                if resource.kind == crate::fonts::formats::sfnt::SfntKind::Opentype =>
            {
                "/Subtype /Type1C".into()
            }
            _ => format!("/Length1 {}", resource.data().len()),
        };
        let file = pdf.add_stream_object(resource.data(), &dictionary, None, None)?;
        let mut descriptor = match resource {
            FontResource::Type1(resource) => type1_descriptor(resource, stem),
            FontResource::Sfnt(resource) => sfnt_descriptor(resource, stem, font)?,
            _ => return Err(Error::value("Bitmap font passed to scalable embedding")),
        };
        if let Some(flags) = font.materialization.map_flags {
            descriptor = descriptor.replacen("/Flags 4", &format!("/Flags {flags}"), 1)
        }
        let charset = if !omit_charset {
            if let Some(charset) = charset.filter(|names| !names.is_empty()) {
                format!(
                    " /CharSet ({})",
                    charset
                        .iter()
                        .map(|name| pdf_name(name).map(|name| format!("/{name}")))
                        .collect::<Result<Vec<_>>>()?
                        .join("")
                )
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        pdf.add_text_object(
            &format!(
                "<< /Type /FontDescriptor /FontName /{} {descriptor} /{} {file} 0 R{charset} >>",
                pdf_name(&font_name)?,
                if kind == "type1" {
                    "FontFile"
                } else if kind == "opentype" {
                    "FontFile3"
                } else {
                    "FontFile2"
                }
            ),
            None,
            1,
        )?
    } else {
        pdf.add_text_object(
            &format!(
                "<< /Type /FontDescriptor /FontName /{} {} >>",
                pdf_name(&font.materialization.pdf_name)?,
                nonembedded_descriptor(font, stem)?
            ),
            None,
            1,
        )?
    };
    if used.is_empty() {
        used.insert(0);
    }
    let first = *used.first().unwrap();
    let last = *used.last().unwrap();
    let widths = (first..=last)
        .map(|code| font.pdf_char_width(code).map(|width| format!("{width:?}")))
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    let widths = pdf.add_text_object(&format!("[ {widths} ]"), None, 1)?;
    let encoding = if kind != "truetype" {
        if let Some(encoding) = &font.materialization.encoding {
            let encoding = encoding.borrow();
            if !encoding.glyph_names.is_empty() {
                Some(pdf.add_text_object(&encoding_dictionary(&encoding)?, None, 1)?)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    let unicode = font_cmap(font, maps, unicode_enabled, warn)
        .map(|cmap| pdf.add_stream_object(&cmap, "", None, None))
        .transpose()?;
    pdf.add_text_object(&format!("<<\n  /Type /Font\n  /Subtype /{}\n  /BaseFont /{}\n  /FirstChar {first}\n  /LastChar {last}\n  /Widths {widths} 0 R\n  /FontDescriptor {descriptor} 0 R\n  {}\n  {}\n  {}\n>>",if kind=="truetype"{"TrueType"}else{"Type1"},pdf_name(&font_name)?,encoding.map_or_else(String::new,|number|format!("/Encoding {number} 0 R")),unicode.map_or_else(String::new,|number|format!("/ToUnicode {number} 0 R")),font.materialization.font_attributes),object_number,1)
}
