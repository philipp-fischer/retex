//! Identity-H embedding with original source-cluster CMaps (`unicode_fonts.py`).
use super::resources::FontUse;
use super::syntax::pdf_name;
use super::writer::PdfWriter;
use crate::core::errors::{Error, Result};
use crate::fonts::model::FontInfo;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
pub fn cmap(codes: &BTreeMap<(u16, String), u16>) -> Vec<u8> {
    let mut entries = codes
        .iter()
        .map(|((_, text), &cid)| {
            (
                cid,
                text.encode_utf16()
                    .map(|word| format!("{word:04X}"))
                    .collect::<String>(),
            )
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|(cid, _)| *cid);
    let mut lines = [
        "/CIDInit /ProcSet findresource begin",
        "12 dict begin begincmap",
        "/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def",
        "/CMapName /UnicodeSourceClusters def /CMapType 2 def",
        "1 begincodespacerange <0000> <FFFF> endcodespacerange",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    for chunk in entries.chunks(100) {
        lines.push(format!("{} beginbfchar", chunk.len()));
        for (cid, text) in chunk {
            lines.push(format!("<{cid:04X}> <{text}>"))
        }
        lines.push("endbfchar".into())
    }
    lines.push("endcmap CMapName currentdict /CMap defineresource pop end end".into());
    lines.join("\n").into_bytes()
}
pub fn embed_unicode_font(pdf: &mut PdfWriter, font: &FontInfo, group: &FontUse) -> Result<usize> {
    let program = font
        .unicode_font
        .as_ref()
        .ok_or_else(|| Error::value("Missing Unicode font program"))?;
    let gids = group
        .unicode_codes
        .keys()
        .map(|(gid, _)| *gid)
        .collect::<BTreeSet<_>>();
    let data = program.subset(&gids)?;
    let digest = Sha256::digest(&data);
    let tag = digest[..6]
        .iter()
        .map(|byte| char::from(b'A' + byte % 26))
        .collect::<String>();
    let name = pdf_name(&format!("{tag}+{}", program.name))?;
    let font_file =
        pdf.add_stream_object(&data, &format!("/Length1 {}", data.len()), None, None)?;
    let scaled = |value: i64| value as f64 * 1000.0 / program.upem as f64;
    let bbox = program
        .bbox
        .iter()
        .map(|&value| format!("{:.6}", scaled(value)))
        .collect::<Vec<_>>()
        .join(" ");
    let descriptor=pdf.add_text_object(&format!("<< /Type /FontDescriptor /FontName /{name} /Flags 4 /FontBBox [{bbox}] /ItalicAngle {:.6} /Ascent {:.6} /Descent {:.6} /CapHeight {:.6} /StemV 80 /FontFile2 {font_file} 0 R >>",program.italic_angle,scaled(program.ascent),scaled(program.descent),scaled(program.ascent)),None,1)?;
    let mut ordered = group
        .unicode_codes
        .iter()
        .map(|(&(gid, ref text), &cid)| {
            let _ = text;
            (cid, gid)
        })
        .collect::<Vec<_>>();
    ordered.sort_by_key(|(cid, _)| *cid);
    let mut mapping = vec![0, 0];
    for &(_, gid) in &ordered {
        mapping.extend(gid.to_be_bytes())
    }
    let glyph_map = pdf.add_stream_object(&mapping, "", None, None)?;
    let widths = ordered
        .iter()
        .map(|&(_, gid)| format!("{:.6}", scaled(program.advances[gid as usize])))
        .collect::<Vec<_>>()
        .join(" ");
    let descendant=pdf.add_text_object(&format!("<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{name} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {descriptor} 0 R /CIDToGIDMap {glyph_map} 0 R /DW 0 /W [1 [{widths}]] >>"),None,1)?;
    let unicode_map = pdf.add_stream_object(&cmap(&group.unicode_codes), "", None, None)?;
    pdf.add_text_object(&format!("<< /Type /Font /Subtype /Type0 /Name /F{} /BaseFont /{name} /Encoding /Identity-H /DescendantFonts [{descendant} 0 R] /ToUnicode {unicode_map} 0 R >>",group.font),Some(group.object_number),1)
}
