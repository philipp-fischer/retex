//! Imported Type1 font replacement obeys the same map policy as native fonts.
use crate::{
    core::errors::{Error, Result},
    fonts::{
        catalog::FontCatalog,
        formats::{
            type1::{EXPERT_ENCODING, STANDARD_ENCODING, Type1Resource, declarative_encoding},
            type1_subsets::Type1Program,
        },
    },
    pdf::{
        font_embedding::type1_descriptor,
        imports::{PDFImportGraph, SourceKey, resolve, stream_data},
        parsing::{PdfValue, dictionary},
    },
};
use regex::Regex;
use std::path::Path;
pub fn replace_font(
    fonts: &mut FontCatalog,
    suppress: bool,
    warn: &mut dyn FnMut(&str),
    g: &mut PDFImportGraph<'_>,
    value: &PdfValue,
    source: &SourceKey,
    reader: &lopdf::Document,
    copy_fonts: bool,
) -> Result<Option<Vec<u8>>> {
    if copy_fonts
        || !matches!(value, PdfValue::Dictionary(_))
        || value.get("Subtype") != Some(&PdfValue::Name("Type1".into()))
        || value.get("BaseFont").is_none()
    {
        return Ok(None);
    }
    let Some(descriptor_ref @ PdfValue::Reference(..)) = value.get("FontDescriptor") else {
        return Ok(None);
    };
    let descriptor = resolve(descriptor_ref, reader)?;
    let Some(program) = descriptor
        .get("FontFile")
        .or_else(|| descriptor.get("FontFile3"))
    else {
        return Ok(None);
    };
    let program = resolve(program, reader)?;
    if descriptor.get("FontFile").is_none()
        && program.get("Subtype") != Some(&PdfValue::Name("Type1C".into()))
    {
        return Ok(None);
    }
    fonts.load_default_map(suppress, warn)?;
    let Some(PdfValue::Name(base)) = value.get("BaseFont") else {
        return Ok(None);
    };
    let base = Regex::new(r"^[A-Z]{6}\+").unwrap().replace(base, "");
    let mapping = fonts
        .maps
        .fonts
        .values()
        .find(|m| m.ps_name.as_deref() == Some(base.as_ref()) && m.included && m.fontfile.is_some())
        .cloned();
    let Some(mapping) = mapping else {
        return Ok(None);
    };
    let filename = mapping.fontfile.as_ref().unwrap();
    let path = fonts
        .resolve(
            Path::new(filename),
            Path::new(mapping.directory.as_deref().unwrap_or("")),
            "type1 fonts",
        )?
        .ok_or_else(|| Error::value(format!("Cannot resolve mapped inclusion font: {filename}")))?;
    let mut resource = Type1Resource::read(path)?;
    if mapping.subset {
        if let Some(PdfValue::String(charset)) = descriptor.get("CharSet") {
            let charset = charset.iter().map(|&c| char::from(c)).collect::<String>();
            let names = charset
                .split('/')
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            (resource, _) = Type1Program::new(resource)?.subset(names, warn)?;
        }
    }
    if mapping.slant != 0 || mapping.extend != 0 {
        resource = resource.transformed(mapping.slant, mapping.extend)?;
    }
    let encoding = if let Some(v) = value.get("Encoding") {
        v.clone()
    } else {
        let data = stream_data(&program, reader)?;
        let names = if descriptor.get("FontFile").is_some() {
            let length = program
                .get("Length1")
                .ok_or_else(|| Error::value("Imported Type1 font has no Length1"))?
                .number()? as usize;
            let header = data
                .get(..length)
                .ok_or_else(|| Error::value("Truncated imported Type1 program"))?
                .iter()
                .map(|&c| char::from(c))
                .collect::<String>();
            let re = Regex::new(r"(?s)/Encoding\s*(.*?)\bdef\b").unwrap();
            let captures = re
                .captures(&header)
                .ok_or_else(|| Error::value("Cannot determine imported Type1 encoding"))?;
            declarative_encoding(&captures[1])?.0
        } else {
            let table = cff_parser::Table::parse(&data)
                .ok_or_else(|| Error::value("Cannot determine imported CFF encoding"))?;
            match table.encoding.kind {
                cff_parser::EncodingKind::Standard => {
                    STANDARD_ENCODING.iter().map(|s| s.to_string()).collect()
                }
                cff_parser::EncodingKind::Expert => (0..256)
                    .map(|slot| {
                        EXPERT_ENCODING
                            .iter()
                            .find(|(code, _)| *code == slot)
                            .map_or(".notdef", |(_, s)| *s)
                            .to_string()
                    })
                    .collect(),
                _ => (0..=255)
                    .map(|c| {
                        table
                            .glyph_index(c)
                            .and_then(|gid| table.glyph_name(gid))
                            .unwrap_or(".notdef")
                            .to_string()
                    })
                    .collect(),
            }
        };
        let differences = std::iter::once(PdfValue::Integer(0))
            .chain(names.into_iter().map(PdfValue::Name))
            .collect();
        PdfValue::Dictionary(vec![
            ("Type".into(), PdfValue::Name("Encoding".into())),
            ("Differences".into(), PdfValue::Array(differences)),
        ])
    };
    let stream = PdfValue::Stream {
        dictionary: vec![
            ("Length1".into(), PdfValue::Integer(resource.length1 as i64)),
            ("Length2".into(), PdfValue::Integer(resource.length2 as i64)),
            ("Length3".into(), PdfValue::Integer(resource.length3 as i64)),
        ],
        data: resource.data.clone(),
    };
    let pn = g.resources.reserve_object();
    g.objects.insert(pn, Some(stream.serialize()?.into()));
    let stem = descriptor
        .get("StemV")
        .map(PdfValue::number)
        .transpose()?
        .unwrap_or(0.0)
        .round_ties_even() as i64;
    let PdfValue::Dictionary(mut desc) = dictionary(&type1_descriptor(&resource, stem))? else {
        unreachable!()
    };
    desc.extend([
        ("Type".into(), PdfValue::Name("FontDescriptor".into())),
        ("FontName".into(), PdfValue::Name(resource.name.clone())),
        ("FontFile".into(), PdfValue::Reference(pn as u32, 0)),
    ]);
    let dn = g.resources.reserve_object();
    g.objects
        .insert(dn, Some(PdfValue::Dictionary(desc).serialize()?.into()));
    let mut entries = value.entries()?.to_vec();
    entries.retain(|(k, _)| !matches!(k.as_str(), "FontDescriptor" | "BaseFont" | "Encoding"));
    let mut replacement =
        |g: &mut PDFImportGraph<'_>, v: &PdfValue, key: &SourceKey, pdf: &lopdf::Document| {
            replace_font(fonts, suppress, warn, g, v, key, pdf, copy_fonts)
        };
    let PdfValue::Dictionary(mut font) = g.clone(
        &PdfValue::Dictionary(entries),
        source,
        reader,
        &mut replacement,
    )?
    else {
        unreachable!()
    };
    font.extend([
        ("BaseFont".into(), PdfValue::Name(resource.name)),
        (
            "Encoding".into(),
            g.clone(&encoding, source, reader, &mut replacement)?,
        ),
        ("FontDescriptor".into(), PdfValue::Reference(dn as u32, 0)),
    ]);
    fonts.used_maps.insert(mapping.tfm_name);
    Ok(Some(PdfValue::Dictionary(font).serialize()?))
}
