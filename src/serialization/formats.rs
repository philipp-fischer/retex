//! Typed, checksummed initialized-state envelope. Job handles and execution stacks are excluded.
use crate::{
    core::{
        characters::LanguageProfile,
        constants::Constants as C,
        errors::{Error, Result},
    },
    fonts::catalog::{CatalogSnapshot, FontCatalog},
    language::{
        expansion::ExpansionSnapshot,
        input::InputSnapshot,
        state::{Environment, EnvironmentSnapshot},
    },
    pdf::{document::PDFDocument, resources::PDFResources},
    resources::{locations::package_root, paths::resolve_path},
    typesetting::{hyphenation::Hyphenation, nodes::NodeSnapshot},
};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
pub const FORMAT_VERSION: u32 = 8;
pub const MAGIC: &[u8] = b"RS-PDFTEX-FORMAT\0\x08\n";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileResource {
    pub path: PathBuf,
    pub sha256: String,
    pub anchor: Option<String>,
    pub relative: Option<PathBuf>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FormatSnapshot {
    pub version: u32,
    pub language_profile: LanguageProfile,
    pub engine: String,
    pub font_id_base: i64,
    pub primitive_signature: String,
    pub interaction: u8,
    pub environment: EnvironmentSnapshot,
    pub input: InputSnapshot,
    pub expansion: ExpansionSnapshot,
    pub hyphenation: Hyphenation,
    pub nodes: NodeSnapshot,
    pub unicode_maps: BTreeMap<String, Option<String>>,
    pub pdf_document: PDFDocument,
    pub pdf_resources: PDFResources,
    pub fonts: CatalogSnapshot<FileResource>,
    pub shared_micro_codes: Vec<usize>,
}
pub trait FormatEngine {
    fn format_is_fresh(&self) -> bool;
    fn format_is_quiescent(&self) -> bool;
    fn capture_format(&mut self) -> Result<FormatSnapshot>;
    fn initialize_format_primitives(&mut self) -> Result<()>;
    fn primitive_signature(&self) -> Result<String>;
    fn language_profile(&self) -> LanguageProfile;
    fn restore_format(&mut self, state: FormatSnapshot, directory: &Path) -> Result<()>;
}
pub fn primitive_signature(env: &Environment) -> Result<String> {
    let mut meanings = Vec::new();
    for (&cs, &frozen) in &env.frozen_primitives {
        let key = env.symbol_keys.get(&frozen);
        if let Some(meaning) = key.and_then(|k| env.meanings.get(k.as_str())) {
            meanings.push((
                env.cs_name(cs),
                meaning.command.clone(),
                meaning.value.clone(),
            ));
        }
    }
    meanings.sort_by(|a, b| a.0.cmp(&b.0));
    let bytes = serde_json::to_vec(&meanings).map_err(|e| Error::value(e.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
pub fn file_resource(
    filename: Option<&str>,
    directory: Option<&Path>,
) -> Result<Option<FileResource>> {
    let Some(filename) = filename else {
        return Ok(None);
    };
    let path = resolve_path(Path::new(filename));
    let mut record = FileResource {
        sha256: format!("{:x}", Sha256::digest(crate::resources::host::read(&path)?)),
        path: path.clone(),
        anchor: None,
        relative: None,
    };
    for (anchor, root) in [
        ("engine", Some(package_root())),
        ("font-directory", directory.map(Path::to_path_buf)),
    ] {
        if let Some(root) = root {
            if let Ok(relative) = path.strip_prefix(&root) {
                record.anchor = Some(anchor.into());
                record.relative = Some(relative.into());
                break;
            }
        }
    }
    Ok(Some(record))
}
pub fn checked_resource(
    resource: Option<FileResource>,
    fonts: Option<&mut FontCatalog>,
    file_type: &str,
    directory: &Path,
) -> Result<Option<PathBuf>> {
    let Some(record) = resource else {
        return Ok(None);
    };
    let mut path = record.path.clone();
    if let Some(fonts) = fonts {
        let root = match record.anchor.as_deref() {
            Some("engine") => Some(fonts.locations.engine_root.clone()),
            Some("font-directory") => Some(fonts.directory.clone()),
            _ => None,
        };
        if let Some(root) = root {
            let relative = record
                .relative
                .as_ref()
                .ok_or_else(|| Error::value("Invalid format resource locator"))?;
            if crate::resources::host::is_absolute(&relative)
                || relative.components().any(|c| c == Component::ParentDir)
            {
                return Err(Error::value("Invalid format resource locator"));
            }
            let located = root.join(relative);
            if crate::resources::host::is_file(&located) {
                path = located;
            }
        }
        if !crate::resources::host::is_file(&path) {
            path = fonts
                .resolve(
                    record
                        .path
                        .file_name()
                        .map(Path::new)
                        .ok_or_else(|| Error::value("Missing format font resource"))?,
                    directory,
                    file_type,
                )?
                .ok_or_else(|| {
                    Error::value(format!(
                        "Missing format font resource: {}",
                        record.path.display()
                    ))
                })?;
        }
    }
    if format!("{:x}", Sha256::digest(crate::resources::host::read(&path)?)) != record.sha256 {
        return Err(Error::value(format!(
            "Format font resource changed: {}",
            path.display()
        )));
    }
    Ok(Some(path))
}
pub fn snapshot(e: &mut impl FormatEngine) -> Result<FormatSnapshot> {
    if !e.format_is_quiescent() {
        return Err(Error::value(
            "Cannot dump a format with a group, conditional, nested list or unfinished typesetting material",
        ));
    }
    e.capture_format()
}
pub fn write_snapshot(state: &FormatSnapshot, path: &Path) -> Result<()> {
    if state.version != FORMAT_VERSION {
        return Err(Error::value("Unsupported Rust format version"));
    }
    let data = bincode::serialize(state).map_err(|e| Error::value(e.to_string()))?;
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&data)?;
    let compressed = encoder.finish()?;
    let temporary = path.with_file_name(format!(
        "{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let result = (|| {
        let mut file = crate::resources::host::open_output(&temporary, false)?;
        file.write_all(MAGIC)?;
        file.write_all(&Sha256::digest(&data))?;
        file.write_all(&compressed)?;
        file.flush()?;
        crate::resources::host::rename(&temporary, path)?;
        Ok(())
    })();
    let _ = crate::resources::host::remove_file(temporary);
    result
}
pub fn read_into(e: &mut impl FormatEngine, path: &Path) -> Result<()> {
    if !e.format_is_fresh() {
        return Err(Error::value(
            "Load a format into a fresh engine before opening input",
        ));
    }
    let raw = crate::resources::host::read(path)?;
    let state = decode_snapshot(&raw, None)?;
    let directory = resolve_path(path)
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    restore_into(e, state, &directory)
}
/// Decode once for a resident host. Native file loading retains its existing
/// unbounded format policy; byte-facing hosts supply an explicit decoded budget.
pub fn decode_snapshot(raw: &[u8], maximum_decoded_bytes: Option<usize>) -> Result<FormatSnapshot> {
    if !raw.starts_with(MAGIC) || raw.len() < MAGIC.len() + 32 {
        return Err(Error::value("Unsupported Rust format version"));
    }
    let digest = &raw[MAGIC.len()..MAGIC.len() + 32];
    let mut data = Vec::new();
    let mut decoder = ZlibDecoder::new(&raw[MAGIC.len() + 32..]);
    if let Some(limit) = maximum_decoded_bytes {
        let bounded = u64::try_from(limit)
            .ok()
            .and_then(|n| n.checked_add(1))
            .ok_or_else(|| Error::value("Invalid initialized format byte budget"))?;
        decoder.take(bounded).read_to_end(&mut data)?;
        if data.len() > limit {
            return Err(Error::value(
                "Initialized format decoded byte budget exceeded",
            ));
        }
    } else {
        decoder.read_to_end(&mut data)?;
    }
    if Sha256::digest(&data).as_slice() != digest {
        return Err(Error::value("Corrupted Rust format"));
    }
    let state: FormatSnapshot =
        bincode::deserialize(&data).map_err(|e| Error::value(e.to_string()))?;
    if state.version != FORMAT_VERSION
        || state.engine != "pdfTeX-1.40.25-rust"
        || state.font_id_base != C::FONT_ID_BASE
    {
        return Err(Error::value("Incompatible Rust format"));
    }
    Ok(state)
}
/// Resource and primitive checks are required for every restore, including
/// restores from an already decoded image. A format is not a live checkpoint.
pub fn restore_into(
    e: &mut impl FormatEngine,
    state: FormatSnapshot,
    directory: &Path,
) -> Result<()> {
    if !e.format_is_fresh() {
        return Err(Error::value(
            "Load a format into a fresh engine before opening input",
        ));
    }
    if state.version != FORMAT_VERSION
        || state.engine != "pdfTeX-1.40.25-rust"
        || state.font_id_base != C::FONT_ID_BASE
    {
        return Err(Error::value("Incompatible Rust format"));
    }
    if state.language_profile != e.language_profile() {
        return Err(Error::value(
            "Format language profile does not match this job",
        ));
    }
    e.initialize_format_primitives()?;
    if state.primitive_signature != e.primitive_signature()? {
        return Err(Error::value("Incompatible Rust format primitive inventory"));
    }
    e.restore_format(state, directory)
}
impl FormatSnapshot {
    /// Ordinary Clone intentionally preserves Rc aliases. A reusable initialized
    /// image needs an independent mutable microtypography graph for each job.
    pub fn detached_clone(&self) -> Result<Self> {
        let mut copy = self.clone();
        if copy.shared_micro_codes.len() != copy.fonts.fonts.len() {
            return Err(Error::value(
                "Invalid shared font state in initialized image",
            ));
        }
        for i in 0..copy.fonts.fonts.len() {
            let shared = copy.shared_micro_codes[i];
            if shared > i {
                return Err(Error::value(
                    "Invalid shared font state in initialized image",
                ));
            }
            copy.fonts.fonts[i].micro_codes = if shared == i {
                std::rc::Rc::new(std::cell::RefCell::new(
                    self.fonts.fonts[i].micro_codes.borrow().clone(),
                ))
            } else {
                copy.fonts.fonts[shared].micro_codes.clone()
            };
        }
        Ok(copy)
    }
}
