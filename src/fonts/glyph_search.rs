//! PK DPI search, aliases, generation and ordered fallbacks (`glyph_search.py`).
use crate::core::errors::Result;
use crate::resources::paths::SearchElement;
use crate::resources::search::ResourceSearch;
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use regex::Regex;
use std::path::{Path, PathBuf};
pub const NATIVE_DPI_MODULUS: i64 = 1_i64 << 32;
#[derive(Clone, Default)]
pub struct GlyphResources {
    pub roots: Option<Vec<SearchElement>>,
}
#[derive(Clone, Debug)]
pub struct GlyphResourceMatch {
    pub path: PathBuf,
    pub dpi: i64,
    pub name: String,
}
impl GlyphResources {
    pub fn resolution(
        &mut self,
        search: &mut ResourceSearch,
        directory: &Path,
        explicit_directory: bool,
        name: &str,
        dpi: i64,
    ) -> Result<Option<GlyphResourceMatch>> {
        search
            .policy
            .runtime_variables
            .insert("KPATHSEA_NAME".into(), name.into());
        search
            .policy
            .runtime_variables
            .insert("KPATHSEA_FORMAT".into(), "pk".into());
        let tolerance = (dpi as f64 / 500.0 + 1.0) as i64;
        let candidates = std::iter::once(dpi).chain(
            ((dpi - tolerance).max(0)..=dpi + tolerance).filter(|candidate| *candidate != dpi),
        );
        for candidate in candidates {
            search
                .policy
                .runtime_variables
                .insert("KPATHSEA_DPI".into(), candidate.to_string());
            if self.roots.is_none() {
                let roots = match crate::resources::host::var("PDFTEXFONTS") {
                    Ok(override_path) => search.policy.elements(&override_path),
                    Err(_) => {
                        let mut roots = Vec::new();
                        if explicit_directory {
                            roots
                                .push(SearchElement::new(directory.to_string_lossy().into_owned()));
                        }
                        roots.extend(search.roots("PKFONTS", &[(directory.to_path_buf(), false)]));
                        roots
                    }
                };
                self.roots = Some(roots);
            }
            let names = [
                PathBuf::from(format!("{name}.{candidate}pk")),
                PathBuf::from(format!("dpi{candidate}/{name}.pk")),
            ];
            if let Some(path) = search.resolve(&names, self.roots.as_ref().unwrap(), false, None)? {
                return Ok(Some(GlyphResourceMatch {
                    path,
                    dpi: candidate,
                    name: name.into(),
                }));
            }
        }
        Ok(None)
    }
    pub fn fallback_resolutions(dpi: i64, warn: &mut dyn FnMut(&str)) -> Vec<i64> {
        let raw = crate::resources::host::var("PDFTEXSIZES")
            .or_else(|_| crate::resources::host::var("TEXSIZES"))
            .unwrap_or_default();
        let mut sizes = Vec::new();
        let numeric = Regex::new(r"^\s*([+-]?\d+)").expect("fallback DPI grammar");
        for item in raw
            .replace(';', ":")
            .split(':')
            .filter(|item| !item.is_empty())
        {
            let value = numeric
                .captures(item)
                .and_then(|capture| capture[1].parse::<BigInt>().ok())
                .map_or(0, |value| {
                    ((value % NATIVE_DPI_MODULUS + NATIVE_DPI_MODULUS) % NATIVE_DPI_MODULUS)
                        .to_i64()
                        .unwrap()
                });
            if sizes.last().is_some_and(|last| value < *last) {
                warn(&format!(
                    "kpathsea: last resort size {item} not in ascending order; ignored"
                ));
                continue;
            }
            sizes.push(value);
        }
        if let Some(index) = sizes.iter().position(|value| *value == 0) {
            sizes.truncate(index);
        }
        if sizes.is_empty() {
            return Vec::new();
        }
        let mut location = (0..sizes.len())
            .min_by_key(|&index| (sizes[index] as i128 - dpi as i128).abs())
            .unwrap();
        let mut lower = location.checked_sub(1);
        let mut upper = location + 1;
        let mut result = Vec::new();
        loop {
            if sizes[location] != dpi {
                result.push(sizes[location]);
            }
            let below = lower.map(|index| dpi as i128 - sizes[index] as i128);
            let above = sizes.get(upper).map(|&size| size as i128 - dpi as i128);
            if below.is_none() && above.is_none() {
                break;
            }
            if below.is_some() && above.is_none_or(|above| below.unwrap() < above) {
                location = lower.unwrap();
                lower = location.checked_sub(1);
            } else {
                location = upper;
                upper += 1;
            }
        }
        result
    }
    pub fn find(
        &mut self,
        search: &mut ResourceSearch,
        directory: &Path,
        explicit_directory: bool,
        name: &str,
        dpi: i64,
        warn: &mut dyn FnMut(&str),
    ) -> Result<Option<GlyphResourceMatch>> {
        if let Some(found) = self.resolution(search, directory, explicit_directory, name, dpi)? {
            return Ok(Some(found));
        }
        let aliases = search.aliases(name)?;
        for alias in &aliases {
            if let Some(found) = self.resolution(
                search,
                directory,
                explicit_directory,
                &alias.to_string_lossy(),
                dpi,
            )? {
                return Ok(Some(found));
            }
        }
        let selected = if let Some(alias) = aliases.first() {
            if search.aliases(&alias.to_string_lossy())?.is_empty() {
                alias.to_string_lossy().into_owned()
            } else {
                name.into()
            }
        } else {
            name.into()
        };
        if !crate::resources::host::is_absolute(Path::new(&selected))
            && !selected.starts_with("./")
            && !selected.starts_with("../")
        {
            search
                .policy
                .runtime_variables
                .insert("KPATHSEA_NAME".into(), selected.clone());
            search
                .policy
                .runtime_variables
                .insert("KPATHSEA_DPI".into(), dpi.to_string());
            if let Some(path) = search.make("pk", &selected)? {
                return Ok(Some(GlyphResourceMatch {
                    path,
                    dpi,
                    name: selected,
                }));
            }
        }
        for resolution in Self::fallback_resolutions(dpi, warn) {
            if let Some(found) =
                self.resolution(search, directory, explicit_directory, &selected, resolution)?
            {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }
}
