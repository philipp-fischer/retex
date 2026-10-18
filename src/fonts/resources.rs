//! Selected-font factories and outline dispatch (`fonts/resources.py`).
use super::encoding::EncFileParser;
use super::formats::{afm::AfmMetrics, sfnt::SfntResource, tfm::TfmReader, type1::Type1Resource};
use super::materialization::{FontMaterialization, FontResource};
use super::model::FontInfo;
use super::scaling::scale_fix_word;
use crate::core::constants::Constants as C;
use crate::core::errors::{Error, Result};
use crate::resources::locations::ResourceLocations;
use crate::resources::paths::resolve_path;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

struct ProgramEntry {
    resource: Arc<FontResource>,
    warnings: Vec<String>,
    bytes: usize,
    age: u64,
}
#[derive(Default)]
struct ProgramCache {
    entries: BTreeMap<(bool, [u8; 32]), ProgramEntry>,
    bytes: usize,
    tick: u64,
}
thread_local! { static PROGRAMS: RefCell<ProgramCache> = RefCell::new(ProgramCache::default()); }
#[cfg(feature = "latency-diagnostics")]
#[allow(dead_code)]
pub(crate) fn diagnostics() -> serde_json::Value {
    PROGRAMS.with(|cache| { let cache=cache.borrow(); serde_json::json!({"entries":cache.entries.len(),"bytes":cache.bytes,"byte_budget":32*1024*1024,"entry_budget":64}) })
}
impl ProgramCache {
    fn load(
        &mut self,
        data: &[u8],
        sfnt: bool,
        path: &Path,
    ) -> Result<(Arc<FontResource>, Vec<String>)> {
        #[cfg(feature = "latency-diagnostics")]
        let _hash_diag = crate::latency_diagnostics::phase(
            "font_program_hash",
            &path.display().to_string(),
            None,
        );
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::count("hashed_source_bytes", data.len());
        let key = (sfnt, Sha256::digest(data).into());
        #[cfg(feature = "latency-diagnostics")]
        drop(_hash_diag);
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase(
            "font_program_cache",
            &path.display().to_string(),
            None,
        );
        self.tick = self.tick.wrapping_add(1);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.age = self.tick;
            #[cfg(feature = "latency-diagnostics")]
            {
                crate::latency_diagnostics::count("hits", 1);
                crate::latency_diagnostics::count(
                    "prepared_serialized_bytes_returned",
                    entry.bytes,
                );
            }
            return Ok((entry.resource.clone(), entry.warnings.clone()));
        }
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::count("misses", 1);
        let mut warnings = Vec::new();
        let resource = Arc::new(if sfnt {
            FontResource::Sfnt(SfntResource::parse(data, &mut |w| {
                warnings.push(w.to_string())
            })?)
        } else {
            let mut pfb = super::formats::type1::PfbParser::default();
            if !pfb.parse_bytes(data) {
                return Err(Error::value(format!("invalid PFB {}", path.display())));
            }
            FontResource::Type1(Type1Resource::from_pfb(&pfb)?)
        });
        let bytes = bincode::serialized_size(resource.as_ref())
            .map_err(|e| Error::value(e.to_string()))? as usize
            + warnings.iter().map(|w| w.len()).sum::<usize>();
        if bytes <= 32 * 1024 * 1024 {
            while self.bytes + bytes > 32 * 1024 * 1024 || self.entries.len() >= 64 {
                let key = *self
                    .entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.age)
                    .unwrap()
                    .0;
                self.bytes -= self.entries.remove(&key).unwrap().bytes;
                #[cfg(feature = "latency-diagnostics")]
                crate::latency_diagnostics::count("evictions", 1);
            }
            self.bytes += bytes;
            self.entries.insert(
                key,
                ProgramEntry {
                    resource: resource.clone(),
                    warnings: warnings.clone(),
                    bytes,
                    age: self.tick,
                },
            );
        }
        Ok((resource, warnings))
    }
}
/// Always perform the physical read, observing current bytes and dependencies.
/// Only pure parsing is memoized; warnings are replayed on every materialization.
pub(crate) fn read_shared_font_resource(
    path: &Path,
    warn: &mut dyn FnMut(&str),
) -> Result<Arc<FontResource>> {
    let suffix = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    let sfnt = ["ttf", "ttc", "otf"].contains(&suffix.as_str());
    let bytes = crate::resources::host::read(path).map_err(|error| {
        if sfnt {
            Error::from(error)
        } else {
            Error::value(format!("invalid PFB {}", path.display()))
        }
    })?;
    let (resource, warnings) =
        PROGRAMS.with(|cache| cache.borrow_mut().load(&bytes, sfnt, path))?;
    for warning in warnings {
        warn(&warning);
    }
    Ok(resource)
}
/// Owned compatibility API; callers that mutate a program keep their own copy.
pub fn read_font_resource(path: &Path, warn: &mut dyn FnMut(&str)) -> Result<FontResource> {
    let resource = read_shared_font_resource(path, warn)?;
    #[cfg(feature = "latency-diagnostics")]
    let _diag = crate::latency_diagnostics::phase(
        "font_program_owned_clone",
        &path.display().to_string(),
        None,
    );
    Ok(resource.as_ref().clone())
}
pub fn create_font(
    name: &str,
    pdf_name: &str,
    size_pt: f64,
    tfm_file: Option<&Path>,
    pfb_file: Option<&Path>,
    enc_file: Option<&Path>,
    locations: &ResourceLocations,
) -> Result<FontInfo> {
    let size = (size_pt * C::UNITY as f64).round_ties_even() as i64;
    let mut font = FontInfo {
        name: name.into(),
        size,
        char_width: size.div_euclid(2),
        char_height: (size * 7).div_euclid(10),
        char_depth: size.div_euclid(5),
        space_width: size.div_euclid(4),
        materialization: FontMaterialization {
            pdf_name: pdf_name.into(),
            ..FontMaterialization::default()
        },
        ..FontInfo::default()
    };
    if let Some(path) = tfm_file {
        if let Some(tfm) = TfmReader::default().read_tfm(path) {
            font.tfm_file = Some(resolve_path(path).to_string_lossy().into_owned());
            font.parameter_count = 7.max(tfm.param_fix.len());
            font.tfm_widths = Some(tfm.widths.clone().into());
            font.tfm_heights = Some(tfm.heights.clone().into());
            font.tfm_depths = Some(tfm.depths.clone().into());
            font.tfm_lig_kern = Some(tfm.lig_kern.clone().into());
            font.tfm_lig_start = Some(tfm.lig_kern_start.clone().into());
            font.boundary_char = tfm.boundary_char;
            font.boundary_label = tfm.boundary_label;
            font.false_boundary_char = tfm.false_boundary_char;
            font.tfm_kerns = Some(
                tfm.kern_fix
                    .iter()
                    .map(|&value| scale_fix_word(value, size))
                    .collect(),
            );
            font.tfm_design_size = Some(tfm.design_size);
            font.tfm_data = Some(Arc::new(tfm));
            font.space_width = font.get_parameter(2);
        } else {
            font.materialization.resource_error = Some(format!(
                "Font {name} not loaded: invalid or missing TFM {}",
                path.display()
            ));
        }
    }
    if ["Times-Bold", "Times-Italic"].contains(&pdf_name)
        && font
            .tfm_widths
            .as_ref()
            .is_none_or(|widths| widths.is_empty())
    {
        font.afm = Some(Arc::new(AfmMetrics::read(
            locations.font_metrics.join(format!("{pdf_name}.afm")),
        )?));
        font.space_width = font.get_parameter(2);
    }
    if let Some(path) = pfb_file {
        match read_font_resource(path, &mut |_| {}) {
            Ok(resource) => {
                font.materialization.pfb_data = Some(Arc::from(resource.data()));
                font.materialization.native_encoding = Some(Arc::from(resource.encoding()));
                font.materialization.resource = Some(Arc::new(resource));
                font.materialization.pfb_file =
                    Some(resolve_path(path).to_string_lossy().into_owned());
            }
            Err(error) => {
                if font.materialization.resource_error.is_none() {
                    font.materialization.resource_error = Some(error.to_string());
                }
            }
        }
    }
    if let Some(path) = enc_file {
        if crate::resources::host::metadata(path).is_ok() {
            let mut encoding = EncFileParser::default();
            if encoding.parse(path, true) {
                font.materialization.encoding = Some(Rc::new(RefCell::new(encoding)));
                font.materialization.enc_file = Some(path.to_string_lossy().into_owned());
            } else {
                font.materialization.resource_error =
                    Some(format!("Invalid font encoding: {}", path.display()));
            }
        } else {
            font.materialization.resource_error =
                Some(format!("Missing font encoding: {}", path.display()));
        }
    }
    if !crate::resources::host::native_capabilities() {
        if let Some(error) = &font.materialization.resource_error {
            return Err(crate::core::errors::Error::value(format!(
                "{error}; supply valid font resources in the project catalog"
            )));
        }
    }
    Ok(font)
}

#[cfg(test)]
mod program_cache_tests {
    use super::*;
    #[test]
    fn parsed_programs_share_and_changed_content_reparses_without_mutable_aliases() {
        let bytes = include_bytes!("../../assets/fonts/assets/cmr10.pfb");
        let mut cache = ProgramCache::default();
        let path = Path::new("font.pfb");
        let (first, _) = cache.load(bytes, false, path).unwrap();
        let (hit, _) = cache.load(bytes, false, path).unwrap();
        assert!(Arc::ptr_eq(&first, &hit));
        let mut changed = bytes.to_vec();
        let needle = b"/FontName /CMR10";
        let at = changed
            .windows(needle.len())
            .position(|s| s == needle)
            .unwrap()
            + needle.len()
            - 1;
        changed[at] = b'1';
        let (next, _) = cache.load(&changed, false, path).unwrap();
        assert_ne!(first.name(), next.name());
        assert_eq!(first.name(), "CMR10");
        assert!(!Arc::ptr_eq(&first, &next));
        assert!(cache.load(b"invalid", false, path).is_err());
        assert!(cache.entries.len() <= 64 && cache.bytes <= 32 * 1024 * 1024);
    }
}
