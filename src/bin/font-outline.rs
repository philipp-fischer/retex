//! Development oracle adapter; runtime outlines use the same portable module.
use retex_rs2::fonts::formats::{type1::Type1Resource, type1_outlines::Type1Outlines};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("Expected original font path")?;
    let resource = Type1Resource::read(path)?;
    let outlines = Type1Outlines::parse(&resource)?;
    let glyphs = outlines
        .glyph_names()
        .map(|name| (name.to_string(), outlines.outline(name)))
        .collect::<std::collections::BTreeMap<_, _>>();
    println!("{}", serde_json::to_string(&glyphs)?);
    Ok(())
}
