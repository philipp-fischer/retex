//! Adobe AFM parser (`formats/afm.py`).
use crate::core::errors::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AfmMetrics {
    pub name: String,
    pub widths: BTreeMap<u32, i64>,
    pub heights: BTreeMap<u32, i64>,
    pub depths: BTreeMap<u32, i64>,
    pub glyph_names: BTreeMap<u32, String>,
    pub x_height: i64,
}
impl AfmMetrics {
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = crate::resources::host::read(&path)?;
        let text: String = bytes.iter().map(|&c| char::from(c)).collect();
        Self::parse(&text)
            .map_err(|_| Error::value(format!("Invalid AFM metrics: {}", path.as_ref().display())))
    }
    pub fn parse(text: &str) -> Result<Self> {
        let mut result = Self {
            name: String::new(),
            widths: BTreeMap::new(),
            heights: BTreeMap::new(),
            depths: BTreeMap::new(),
            glyph_names: BTreeMap::new(),
            x_height: 0,
        };
        let invalid =
            || Error::value("Invalid AFM metrics").with_issue(crate::core::recovery::Issue::Font);
        for line in text.lines() {
            if let Some(name) = line.strip_prefix("FontName ") {
                result.name = name.split_whitespace().next().ok_or_else(invalid)?.into();
            } else if let Some(value) = line.strip_prefix("XHeight ") {
                result.x_height = value
                    .split_whitespace()
                    .next()
                    .ok_or_else(invalid)?
                    .parse()
                    .map_err(|_| invalid())?;
            } else if line.starts_with("C ") {
                let fields: BTreeMap<_, _> = line
                    .split(';')
                    .filter_map(|part| {
                        let mut words = part.split_whitespace();
                        Some((words.next()?, words.collect::<Vec<_>>()))
                    })
                    .collect();
                let get = |name| fields.get(name).ok_or_else(invalid);
                let code: i64 = get("C")?
                    .first()
                    .ok_or_else(invalid)?
                    .parse()
                    .map_err(|_| invalid())?;
                if code < 0 {
                    continue;
                }
                let width = get("WX")?
                    .first()
                    .ok_or_else(invalid)?
                    .parse()
                    .map_err(|_| invalid())?;
                let bounds: Vec<i64> = get("B")?
                    .iter()
                    .map(|v| v.parse().map_err(|_| invalid()))
                    .collect::<Result<_>>()?;
                if bounds.len() < 4 {
                    return Err(invalid());
                }
                result.widths.insert(code as u32, width);
                result.heights.insert(code as u32, bounds[3].max(0));
                result.depths.insert(code as u32, (-bounds[1]).max(0));
                result.glyph_names.insert(
                    code as u32,
                    get("N")?.first().ok_or_else(invalid)?.to_string(),
                );
            }
        }
        if result.name.is_empty() || result.widths.is_empty() {
            return Err(invalid());
        }
        Ok(result)
    }
}
