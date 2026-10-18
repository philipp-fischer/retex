//! Injected resolution of complete map file sets (`fonts/resolve.py`).
use super::maps::MapFileParser;
use crate::core::errors::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};
#[derive(Clone, Debug)]
pub struct ResolvedFontFiles {
    pub ps_name: Option<String>,
    pub tfm_path: PathBuf,
    pub font_path: Option<PathBuf>,
    pub enc_path: Option<PathBuf>,
}
pub fn resolve_font_files(
    tfm_name: &str,
    maps: &MapFileParser,
    mut resolve: impl FnMut(&str, &Path, &str) -> Result<Option<PathBuf>>,
) -> Result<ResolvedFontFiles> {
    let mapping = maps
        .fonts
        .get(tfm_name)
        .ok_or_else(|| Error::value(format!("Font {tfm_name:?} is absent from the map")))?;
    let directory = Path::new(mapping.directory.as_deref().unwrap_or("."));
    let mut required = |filename: &str, kind: &str| {
        resolve(filename, directory, kind)?.ok_or_else(|| {
            Error::new(
                ErrorKind::Io,
                format!("Missing {kind} resource: {filename}"),
            )
        })
    };
    let tfm_path = required(&format!("{tfm_name}.tfm"), "tfm")?;
    let font_path = mapping
        .fontfile
        .as_deref()
        .map(|file| {
            let suffix = Path::new(file)
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_ascii_lowercase();
            let kind = match suffix.as_str() {
                "ttf" | "ttc" => "truetype fonts",
                "otf" => "opentype fonts",
                _ => "type1 fonts",
            };
            required(file, kind)
        })
        .transpose()?;
    let enc_path = mapping
        .encoding
        .as_deref()
        .map(|file| required(file, "enc files"))
        .transpose()?;
    Ok(ResolvedFontFiles {
        ps_name: mapping.ps_name.clone(),
        tfm_path,
        font_path,
        enc_path,
    })
}
