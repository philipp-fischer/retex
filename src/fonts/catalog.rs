//! Job-owned selection, map freezing and font materialization (`fonts/catalog.py`).
use super::encoding::EncFileParser;
use super::expansion::{self, ExpansionCatalog, ExpansionPolicy};
use super::formats::{
    bitmap::{BitmapOptions, BitmapResource},
    tfm::{CharacterTag, TfmReader},
    type1::STANDARD_FONT_FLAGS,
    vf::{VirtualCatalog, VirtualFont, VirtualSelection},
};
use super::glyph_search::GlyphResources;
use super::maps::{FontMapEntry, MapFileParser};
use super::materialization::{FontMaterialization, FontResource};
use super::microtypography::{FontCode, install_letterspacing, set_code};
use super::model::{FontInfo, MicroCodes};
use super::resources::{create_font, read_shared_font_resource};
use super::scaling::{pdf_font_size, scale_fix_word};
use super::unicode::UnicodeFont;
use crate::core::capacities::{Capacities, reached};
use crate::core::characters::LanguageProfile;
use crate::core::config::EngineConfig;
use crate::core::constants::Constants as C;
use crate::core::errors::{Error, Result};
use crate::core::units::sp_to_bp;
use crate::language::records::{Quantity, ValueKey};
use crate::resources::locations::ResourceLocations;
use crate::resources::paths::{SearchElement, resolve_path};
use crate::resources::search::{FONT_PATHS, ResourceSearch, filesystem_name};
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

fn missing_required(error: Error, kind: &str, name: &Path, directory: Option<&Path>) -> Error {
    use crate::language::input_frames::filesystem_bytes;
    error.with_missing_resource(crate::core::errors::MissingResource {
        kind: kind.into(),
        name: filesystem_bytes(name.as_os_str()),
        directory: directory.map(|path| filesystem_bytes(path.as_os_str())),
    })
}

pub type FontFactory = fn(
    &str,
    &str,
    f64,
    Option<&Path>,
    Option<&Path>,
    Option<&Path>,
    &ResourceLocations,
) -> Result<FontInfo>;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FontSnapshot<R> {
    pub name: String,
    pub unicode: Option<R>,
    pub pdf_name: String,
    pub size: i64,
    pub tfm: Option<R>,
    pub pfb: Option<R>,
    pub enc: Option<R>,
    pub parameters: crate::core::persistence::SharedTable<usize, i64>,
    pub parameter_count: usize,
    pub hyphenchar: i64,
    pub skewchar: i64,
    pub null_font: bool,
    pub dynamic: bool,
    pub ligatures_disabled: bool,
    pub micro_codes: Rc<RefCell<MicroCodes>>,
    pub width_overrides: crate::core::persistence::SharedTable<u32, i64>,
    pub letterspacing: Option<(usize, i64)>,
    pub italic_overrides: crate::core::persistence::SharedTable<u32, i64>,
    pub expansion_policy: Option<ExpansionPolicy>,
    pub expansion_base: Option<usize>,
    pub expansion_ratio: i64,
    pub expansion_variants: crate::core::persistence::SharedTable<i64, usize>,
    pub expanded_kerns: Option<Vec<i64>>,
    pub table_words: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogSnapshot<R> {
    pub fonts: Vec<FontSnapshot<R>>,
    pub loaded: Vec<((String, i64), usize)>,
    pub current: usize,
}
#[derive(Clone)]
pub struct FontCatalog {
    pub(crate) construction: std::rc::Weak<super::construction::Scope>,
    pub ini: bool,
    pub language_profile: LanguageProfile,
    pub exact_repro: bool,
    pub locations: ResourceLocations,
    pub directory: PathBuf,
    pub encoding_file: Option<String>,
    pub explicit_directory: bool,
    pub search: ResourceSearch,
    pub capacities: Capacities,
    pub factory: FontFactory,
    pub loaded: BTreeMap<(String, i64), usize>,
    pub maps: MapFileParser,
    pub fallback_maps: MapFileParser,
    pub default_map_pending: bool,
    pub encoding_cache: BTreeMap<String, (String, Rc<RefCell<EncFileParser>>)>,
    pub pdf_options: Option<BitmapOptions>,
    pub glyph_resources: GlyphResources,
    pub used_maps: HashSet<String>,
    pub items: Vec<FontInfo>,
    pub current: usize,
    map_identities: BTreeMap<String, usize>,
}
fn integer(values: &BTreeMap<ValueKey, Quantity>, name: &str, default: i64) -> i64 {
    match values.get(&ValueKey::parameter(name)) {
        Some(Quantity::Integer(value)) => *value,
        _ => default,
    }
}
fn octet_path(name: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(filesystem_name(name)?))
}
impl FontCatalog {
    fn missing_resolved(&self, error: Error, kind: &str, name: &Path, directory: &Path) -> Error {
        // Initialized formats preserve the reference's ini-mode lookup: the
        // source directory is skipped. Use the configured font directory so
        // a normal supplied resource participates in that same ordered search.
        let directory = if self.ini { &self.directory } else { directory };
        missing_required(error, kind, name, Some(directory))
    }

    pub(crate) fn detached_clone(&self) -> Result<Self> {
        let mut copy = self.clone();
        copy.construction = Default::default();
        let mut micro = BTreeMap::new();
        let mut encodings = BTreeMap::new();
        for font in &mut copy.items {
            let key = Rc::as_ptr(&font.micro_codes) as usize;
            let detached = micro
                .entry(key)
                .or_insert_with(|| Rc::new(RefCell::new(font.micro_codes.borrow().clone())));
            font.micro_codes = detached.clone();
            if let Some(encoding) = &mut font.materialization.encoding {
                let key = Rc::as_ptr(encoding) as usize;
                let detached = encodings
                    .entry(key)
                    .or_insert_with(|| Rc::new(RefCell::new(encoding.borrow().clone())));
                *encoding = detached.clone();
            }
        }
        for (_, encoding) in copy.encoding_cache.values_mut() {
            let key = Rc::as_ptr(encoding) as usize;
            let detached = encodings
                .entry(key)
                .or_insert_with(|| Rc::new(RefCell::new(encoding.borrow().clone())));
            *encoding = detached.clone();
        }
        Ok(copy)
    }

    pub fn new(config: &EngineConfig, factory: Option<FontFactory>) -> Result<Self> {
        let locations = ResourceLocations::configured(config)?;
        let directory = resolve_path(&locations.font_assets);
        let search = ResourceSearch::new(
            &config.program_name,
            config.output_directory.as_ref().map(PathBuf::from),
            config.exact_repro,
            locations.clone(),
        )?;
        let capacities = Capacities::configured(&config.capacities)?;
        let factory = factory.unwrap_or(create_font);
        let mut catalog = Self {
            construction: Default::default(),
            ini: config.ini,
            language_profile: config.language_profile,
            exact_repro: config.exact_repro,
            locations,
            directory,
            encoding_file: config.encoding_file.clone(),
            explicit_directory: config.font_dir.is_some(),
            search,
            capacities,
            factory,
            loaded: BTreeMap::new(),
            maps: MapFileParser::default(),
            fallback_maps: MapFileParser::default(),
            default_map_pending: true,
            encoding_cache: BTreeMap::new(),
            pdf_options: None,
            glyph_resources: GlyphResources::default(),
            used_maps: HashSet::new(),
            items: vec![Self::null_font()],
            current: if config.ini { 0 } else { 1 },
            map_identities: BTreeMap::new(),
        };
        let bundled_map = catalog.directory.join("latxfont.map");
        if crate::resources::host::is_file(&bundled_map) {
            let bytes = crate::resources::host::read(&bundled_map)?;
            let text: String = bytes.into_iter().map(char::from).collect();
            for line in text.lines() {
                catalog.apply_map_line(
                    line,
                    "+",
                    bundled_map.parent(),
                    true,
                    false,
                    &mut |_| {},
                )?;
            }
        }
        if !catalog.ini {
            let tfm = catalog.directory.join("cmr10.tfm");
            let pfb = catalog.directory.join("cmr10.pfb");
            let enc = catalog.encoding_file.as_deref().map(Path::new);
            catalog.items.push(factory(
                "CMR10",
                "CMR10",
                11.0,
                Some(&tfm),
                Some(&pfb),
                enc,
                &catalog.locations,
            )?);
            catalog.items.push(factory(
                "Times-Bold",
                "Times-Bold",
                11.0,
                None,
                None,
                None,
                &catalog.locations,
            )?);
            catalog.items.push(factory(
                "Times-Italic",
                "Times-Italic",
                11.0,
                None,
                None,
                None,
                &catalog.locations,
            )?);
            catalog.items.push(factory(
                "CMR10",
                "CMR10",
                14.0,
                Some(&tfm),
                Some(&pfb),
                enc,
                &catalog.locations,
            )?);
            catalog.items.push(factory(
                "CMR10",
                "CMR10",
                17.0,
                Some(&tfm),
                Some(&pfb),
                enc,
                &catalog.locations,
            )?);
        }
        for font in &mut catalog.items {
            font.materialization.embedding_available = font.materialization.resource.is_some()
                && font
                    .tfm_widths
                    .as_ref()
                    .is_some_and(|widths| !widths.is_empty());
        }
        Ok(catalog)
    }
    fn null_font() -> FontInfo {
        FontInfo {
            name: "nullfont".into(),
            size: 0,
            tfm_design_size: Some(0),
            null_font: true,
            materialization: FontMaterialization {
                pdf_name: "nullfont".into(),
                ..FontMaterialization::default()
            },
            ..FontInfo::default()
        }
    }
    pub fn resolve(
        &mut self,
        filename: &Path,
        directory: &Path,
        file_type: &str,
    ) -> Result<Option<PathBuf>> {
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase(
            "font_resolution",
            &format!(
                "{file_type}:{} from {}",
                filename.display(),
                directory.display()
            ),
            None,
        );
        if crate::resources::host::is_absolute(&filename) {
            if crate::resources::host::is_file(&filename) {
                return Ok(Some(resolve_path(filename)));
            }
            let generated = if file_type == "tfm"
                && filename.extension().is_some_and(|suffix| suffix == "tfm")
            {
                filename.with_extension("")
            } else {
                filename.into()
            };
            return self.search.make(file_type, &generated.to_string_lossy());
        }
        let mut roots = Vec::new();
        if self.explicit_directory {
            roots.push(SearchElement::new(
                self.directory.to_string_lossy().into_owned(),
            ));
        }
        let cwd = crate::resources::host::current_dir()?;
        if !self.ini {
            roots.extend([
                SearchElement::new(directory.to_string_lossy().into_owned()),
                SearchElement::new(cwd.to_string_lossy().into_owned()),
            ]);
        }
        let defaults = if file_type == "misc fonts" {
            vec![(cwd, false), (self.directory.clone(), false)]
        } else {
            vec![(self.directory.clone(), false)]
        };
        let variable = FONT_PATHS
            .iter()
            .find(|(kind, _)| *kind == file_type)
            .map(|(_, variable)| *variable)
            .ok_or_else(|| Error::value(format!("Unknown font file type: {file_type}")))?;
        roots.extend(self.search.roots(variable, &defaults));
        let mut names = vec![filename.to_path_buf()];
        if file_type == "tfm" {
            names.extend(self.search.aliases(&filename.to_string_lossy())?);
        }
        if let Some(found) =
            self.search
                .resolve(&names, &roots, file_type != "vf", Some(&[filename.into()]))?
        {
            return Ok(Some(found));
        }
        let generated =
            if file_type == "tfm" && filename.extension().is_some_and(|suffix| suffix == "tfm") {
                filename.with_extension("")
            } else {
                filename.into()
            };
        self.search.make(file_type, &generated.to_string_lossy())
    }
    pub fn memory_words(&self) -> usize {
        self.items
            .iter()
            .map(|font| {
                font.tfm_data.as_ref().map_or(7, |tfm| tfm.memory_words)
                    + font.parameter_count.saturating_sub(
                        font.tfm_data
                            .as_ref()
                            .map_or(7, |tfm| 7.max(tfm.param_fix.len())),
                    )
            })
            .sum()
    }
    pub fn load_unicode(
        &mut self,
        filename: &str,
        directory: Option<&Path>,
        size: Option<i64>,
        scaled: i64,
    ) -> Result<usize> {
        let mut path = PathBuf::from(
            filename
                .strip_prefix('[')
                .and_then(|text| text.strip_suffix(']'))
                .unwrap_or(filename),
        );
        if !crate::resources::host::is_absolute(&path) {
            path = directory
                .map(Path::to_path_buf)
                .unwrap_or(crate::resources::host::current_dir()?)
                .join(path);
        }
        if !crate::resources::host::is_file(&path) {
            return Err(missing_required(
                Error::value(format!("Unicode font file not found: {}", path.display())),
                "unicode font",
                &path,
                None,
            ));
        }
        let size = size.unwrap_or((10 * C::UNITY * scaled).div_euclid(1000));
        if size <= 0 {
            return Err(Error::value("Font size must be positive"));
        }
        let key = (resolve_path(&path).to_string_lossy().into_owned(), size);
        if let Some(&number) = self.loaded.get(&key) {
            return Ok(number);
        }
        let program = UnicodeFont::read(&path, size)?;
        let font = FontInfo {
            name: program.name.clone(),
            size,
            tfm_design_size: Some(size),
            materialization: FontMaterialization {
                pdf_name: program.name.clone(),
                map_bound: true,
                embedding_available: true,
                ..FontMaterialization::default()
            },
            unicode_font: Some(Arc::new(program)),
            ..FontInfo::default()
        };
        let number = self.items.len();
        self.items.push(font);
        self.loaded.insert(key, number);
        Ok(number)
    }
    pub(crate) fn observe_construction_read(&self, number: usize) {
        if let Some(observer) = self.construction.upgrade() {
            observer.observe(self, number);
        }
    }
    pub(crate) fn can_mutate_constructed(&self, number: usize) -> bool {
        self.construction
            .upgrade()
            .is_some_and(|o| o.can_mutate(number))
    }
    pub fn load(
        &mut self,
        filename: &str,
        directory: &Path,
        size: Option<i64>,
        scaled: i64,
    ) -> Result<usize> {
        let observer = self.construction.upgrade();
        let receipt = observer.as_ref().and_then(|o| o.start(self));
        let result = self.load_constructed(filename, directory, size, scaled);
        if let Some(observer) = observer {
            observer.end(receipt, self, result.as_ref().ok().copied());
        }
        result
    }
    fn load_constructed(
        &mut self,
        filename: &str,
        directory: &Path,
        size: Option<i64>,
        scaled: i64,
    ) -> Result<usize> {
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase(
            "font_load",
            &format!("{filename} size={size:?} scaled={scaled}"),
            None,
        );
        let suffix = Path::new(filename)
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        if self.language_profile == LanguageProfile::Unicode
            && (filename.starts_with('[') || ["ttf", "otf", "ttc"].contains(&suffix.as_str()))
        {
            return self.load_unicode(filename, Some(directory), size, scaled);
        }
        let path = if Path::new(filename)
            .extension()
            .is_some_and(|suffix| suffix == "tfm")
        {
            Path::new(filename).with_extension("")
        } else {
            filename.into()
        };
        let name = path.to_string_lossy().into_owned();
        for ((loaded_name, loaded_size), &number) in &self.loaded {
            if loaded_name == &name {
                let requested = size.unwrap_or(
                    (self.items[number].tfm_design_size.unwrap_or(0) * scaled).div_euclid(1000),
                );
                if requested == *loaded_size {
                    return Ok(number);
                }
            }
        }
        let requested = PathBuf::from(format!("{name}.tfm"));
        let tfm = self
            .resolve(&requested, directory, "tfm")
            .map_err(|error| {
                if error.missing_resource.is_some() {
                    self.missing_resolved(error, "tfm", &requested, directory)
                } else {
                    error
                }
            })?
            .ok_or_else(|| {
                self.missing_resolved(
                    Error::value(format!(
                        "Font {filename} not loaded: missing TFM (searched {}, cwd, {}, kpathsea)",
                        directory.display(),
                        self.directory.display()
                    )),
                    "tfm",
                    &requested,
                    directory,
                )
            })?;
        let metrics = TfmReader::default().read_tfm(&tfm).ok_or_else(|| {
            Error::value(format!(
                "Font {filename} not loaded: invalid TFM {}",
                tfm.display()
            ))
        })?;
        let size = size.unwrap_or((metrics.design_size * scaled).div_euclid(1000));
        if !(1..2048 * C::UNITY).contains(&size) {
            return Err(Error::value(
                "Font size must be positive and less than 2048pt",
            ));
        }
        let key = (name.clone(), size);
        if let Some(&number) = self.loaded.get(&key) {
            return Ok(number);
        }
        if reached(self.capacities.font_max, self.items.len() - 1) {
            return Err(Error::value("Font capacity exceeded")
                .with_issue(crate::core::recovery::Issue::Capacity));
        }
        let mut pfb = self.resolve(Path::new(&format!("{name}.pfb")), directory, "type1 fonts")?;
        if pfb.is_none() {
            pfb = self.resolve(Path::new(&format!("{name}.pfa")), directory, "type1 fonts")?;
        }
        let font_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if pfb.is_none() {
            if let Some(outline) = self
                .fallback_maps
                .fonts
                .get(&font_name)
                .and_then(|mapping| mapping.fontfile.clone())
            {
                pfb = self.resolve(Path::new(&outline), directory, "type1 fonts")?;
            }
        }
        let enc = if let Some(encoding) = self.encoding_file.clone() {
            let path = self
                .resolve(Path::new(&encoding), directory, "enc files")?
                .ok_or_else(|| {
                    self.missing_resolved(
                        Error::value(format!("Missing font encoding: {encoding}")),
                        "enc files",
                        Path::new(&encoding),
                        directory,
                    )
                })?;
            if !EncFileParser::default().parse(&path, true) {
                return Err(Error::value(format!(
                    "Invalid font encoding: {}",
                    path.display()
                )));
            }
            Some(path)
        } else {
            None
        };
        let mut font = (self.factory)(
            &font_name,
            &font_name,
            size as f64 / C::UNITY as f64,
            Some(&tfm),
            None,
            enc.as_deref(),
            &self.locations,
        )?;
        if font.tfm_data.is_none() {
            return Err(Error::value(format!(
                "Font {filename} not loaded: invalid TFM {}",
                tfm.display()
            )));
        }
        font.dynamic = true;
        font.materialization.pfb_file =
            pfb.as_ref().map(|path| path.to_string_lossy().into_owned());
        font.size = size;
        font.tfm_kerns = Some(
            metrics
                .kern_fix
                .iter()
                .map(|&value| scale_fix_word(value, size))
                .collect(),
        );
        font.space_width = font.get_parameter(2);
        font.materialization.embedding_available = pfb.is_some();
        let number = self.items.len();
        self.items.push(font);
        self.loaded.insert(key, number);
        Ok(number)
    }
    fn apply_map_line(
        &mut self,
        text: &str,
        mode: &str,
        directory: Option<&Path>,
        fallback: bool,
        suppress_duplicates: bool,
        warn: &mut dyn FnMut(&str),
    ) -> Result<()> {
        let entry = match MapFileParser::parse_map_line(text) {
            Ok(Some(entry)) => entry,
            Ok(None) => return Ok(()),
            Err(error) => {
                warn(&error.to_string());
                return Ok(());
            }
        };
        let sfd = super::maps::subfont_pattern();
        let pattern = sfd.captures(&entry.tfm_name);
        let declarations = if let Some(pattern) = pattern {
            let (prefix, name) = (&pattern[1], &pattern[2]);
            let known = if fallback {
                self.fallback_maps.subfont_definitions.contains_key(name)
            } else {
                self.maps.subfont_definitions.contains_key(name)
            };
            if !known {
                let cwd = crate::resources::host::current_dir()?;
                if let Some(path) = self.resolve(
                    Path::new(&format!("{name}.sfd")),
                    directory.unwrap_or(&cwd),
                    "sfd",
                )? {
                    let definitions = super::subfonts::read_definitions(path)?;
                    let parser = if fallback {
                        &mut self.fallback_maps
                    } else {
                        &mut self.maps
                    };
                    parser.subfont_definitions.insert(name.into(), definitions);
                } else {
                    warn(&format!("Cannot open SFD file for reading: {name}"));
                }
            }
            let parser = if fallback {
                &self.fallback_maps
            } else {
                &self.maps
            };
            if let Some(definitions) = parser.subfont_definitions.get(name) {
                definitions
                    .iter()
                    .rev()
                    .map(|(infix, mapping)| {
                        let mut expanded = entry.clone();
                        expanded.tfm_name = format!("{prefix}{infix}");
                        expanded.subfont = Some(mapping.clone());
                        expanded.pid_eid = entry.pid_eid.or(Some((3, 1)));
                        expanded.ps_name = None;
                        expanded.flags = None;
                        expanded
                    })
                    .collect()
            } else {
                vec![entry]
            }
        } else {
            vec![entry]
        };
        let parser = if fallback {
            &mut self.fallback_maps
        } else {
            &mut self.maps
        };
        parser.suppress_duplicates = Rc::new(move || suppress_duplicates);
        let unused = HashSet::new();
        for entry in declarations {
            parser.insert(
                entry,
                mode,
                warn,
                if fallback { &unused } else { &self.used_maps },
                directory,
            )?;
        }
        Ok(())
    }
    fn parse_map(
        &mut self,
        path: &Path,
        mode: &str,
        suppress_duplicates: bool,
        warn: &mut dyn FnMut(&str),
    ) -> Result<()> {
        let bytes = crate::resources::host::read_shared(path)?;
        if mode == "+" && self.used_maps.is_empty() {
            if let Some((fonts, warnings)) =
                super::map_preparation::prepare(&self.maps, path, &bytes, suppress_duplicates)
            {
                self.maps.fonts = fonts;
                self.maps.suppress_duplicates = Rc::new(move || suppress_duplicates);
                for warning in warnings {
                    warn(&warning);
                }
                return Ok(());
            }
        }
        let text: String = bytes.iter().copied().map(char::from).collect();
        for line in text.lines() {
            self.apply_map_line(line, mode, path.parent(), false, suppress_duplicates, warn)?;
        }
        Ok(())
    }
    pub fn load_default_map(
        &mut self,
        suppress_duplicates: bool,
        warn: &mut dyn FnMut(&str),
    ) -> Result<()> {
        if !self.default_map_pending {
            return Ok(());
        }
        self.default_map_pending = false;
        let mut roots = Vec::new();
        if self.explicit_directory {
            roots.push(SearchElement::new(
                self.directory.to_string_lossy().into_owned(),
            ));
        }
        roots.extend(self.search.roots("TEXFONTMAPS", &[]));
        let mut path = self
            .search
            .resolve(&["pdftex.map".into()], &roots, false, None)?;
        if path.is_none() {
            path = self.resolve(
                Path::new("pdftex.map"),
                &crate::resources::host::current_dir()?,
                "map",
            )?;
        }
        if let Some(path) = path {
            self.parse_map(&path, "+", suppress_duplicates, warn)?;
        }
        Ok(())
    }
    pub fn map_command(
        &mut self,
        text: &str,
        line: bool,
        directory: &Path,
        suppress_duplicates: bool,
        warn: &mut dyn FnMut(&str),
    ) -> Result<()> {
        let mut text = text.trim_start();
        let mode = if let Some(mode) = text.chars().next().filter(|ch| "+-=".contains(*ch)) {
            self.load_default_map(suppress_duplicates, warn)?;
            text = text[mode.len_utf8()..].trim_start();
            mode.to_string()
        } else {
            self.default_map_pending = false;
            "+".into()
        };
        if text.is_empty() {
            return Ok(());
        }
        if line {
            self.apply_map_line(
                text,
                &mode,
                Some(directory),
                false,
                suppress_duplicates,
                warn,
            )
        } else {
            let filename = octet_path(text.split_whitespace().next().unwrap())?;
            if let Some(path) = self.resolve(&filename, directory, "map")? {
                self.parse_map(&path, &mode, suppress_duplicates, warn)
            } else {
                warn(&format!(
                    "Cannot open font map file: {}",
                    filename.display()
                ));
                Ok(())
            }
        }
    }
    pub fn bind(
        &mut self,
        number: usize,
        suppress_duplicates: bool,
        warn: &mut dyn FnMut(&str),
    ) -> Result<()> {
        #[cfg(feature = "latency-diagnostics")]
        let _diag = (crate::latency_diagnostics::enabled() || crate::output_diagnostics::active())
            .then(|| {
                if self.items[number].materialization.map_bound {
                    crate::latency_diagnostics::count("already_bound_font_calls", 1);
                    None
                } else {
                    Some(crate::latency_diagnostics::phase(
                        "font_bind",
                        &format!(
                            "{}@{} vf={}",
                            self.items[number].name,
                            self.items[number].size,
                            self.items[number].virtual_checked
                        ),
                        None,
                    ))
                }
            });
        if self.items[number].unicode_font.is_some() {
            self.items[number].materialization.map_bound = true;
            return Ok(());
        }
        if self.items[number].materialization.map_bound {
            return Ok(());
        }
        if let Some(base_number) = self.items[number].expansion_base.filter(|_| {
            self.items[number]
                .expansion_policy
                .is_some_and(|policy| policy.automatic)
        }) {
            self.bind(base_number, suppress_duplicates, warn)?;
            let base = self.items[base_number].clone();
            if base.virtual_font.is_some() {
                let font = self.items[number].clone();
                let virtual_font = expansion::virtual_variant(self, &font, &base)?;
                self.items[number].virtual_font = Some(Arc::new(virtual_font));
            } else {
                let source = base.materialization;
                let target = &mut self.items[number].materialization;
                target.bound_mapping = source.bound_mapping;
                target.map_identity = source.map_identity;
                target.pdf_kind = source.pdf_kind;
                target.pdf_name = source.pdf_name;
                target.nonembedded = source.nonembedded;
                target.embedding_available = source.embedding_available;
                target.resource_ready = source.resource_ready;
                target.resource = source.resource;
                target.pfb_data = source.pfb_data;
                target.pfb_file = source.pfb_file;
                target.native_encoding = source.native_encoding;
                target.encoding = source.encoding;
                target.map_flags = source.map_flags;
                target.subset = source.subset;
                target.subfont = source.subfont;
                target.pid_eid = source.pid_eid;
            }
            self.items[number].materialization.map_bound = true;
            return Ok(());
        }
        if !self.items[number].virtual_checked {
            self.items[number].virtual_checked = true;
            if let Some(file) = self.items[number].tfm_file.clone() {
                let name = format!("{}.vf", self.items[number].name);
                if let Some(path) = self.resolve(
                    Path::new(&name),
                    Path::new(&file).parent().unwrap_or(Path::new(".")),
                    "vf",
                )? {
                    let virtual_font = VirtualFont::read(&path, number, self, warn)?;
                    self.items[number].virtual_font = Some(Arc::new(virtual_font));
                }
            }
        }
        if let Some(virtual_font) = self.items[number].virtual_font.clone() {
            if self.items[number].expansion_base.is_none() {
                if let Some(policy) = self.items[number].expansion_policy {
                    for &local in virtual_font.fonts.values() {
                        expansion::configure(
                            self,
                            local,
                            policy.stretch,
                            policy.shrink,
                            policy.step,
                            policy.automatic,
                        )?;
                    }
                }
            }
            self.items[number].materialization.map_bound = true;
            return Ok(());
        }
        self.load_default_map(suppress_duplicates, warn)?;
        let name = self.items[number].name.clone();
        let mapping = self.maps.fonts.get(&name).cloned();
        let identity = if mapping.is_some() {
            let next = self.map_identities.len() + 1;
            *self.map_identities.entry(name.clone()).or_insert(next)
        } else {
            0
        };
        let font = &mut self.items[number];
        font.materialization.map_identity = identity;
        font.materialization.bound_mapping = mapping.clone();
        if let Some(mapping) = &mapping {
            self.used_maps.insert(name);
            let outline = mapping.fontfile.as_deref();
            font.materialization.map_flags = mapping.flags;
            font.materialization.subfont = mapping.subfont.clone();
            font.materialization.pid_eid = mapping
                .pid_eid
                .map(|(pid, eid)| (pid as u16, eid as u16))
                .unwrap_or((3, 1));
            font.materialization.subset = mapping.subset;
            let bitmap = (outline.is_none() && mapping.ps_name.is_none())
                || outline.is_some_and(|name| {
                    Path::new(name)
                        .extension()
                        .is_some_and(|suffix| suffix.to_string_lossy().eq_ignore_ascii_case("pk"))
                });
            font.materialization.nonembedded = !bitmap && !mapping.included;
            font.materialization.pdf_kind = if bitmap { "bitmap" } else { "scalable" }.into();
            font.materialization.pdf_name =
                mapping.ps_name.clone().unwrap_or_else(|| font.name.clone());
        } else {
            font.materialization.pdf_kind = if (font.dynamic && self.ini)
                || (font.materialization.resource.is_none()
                    && font.materialization.pfb_file.is_none()
                    && font.tfm_data.is_some())
            {
                "bitmap"
            } else {
                "scalable"
            }
            .into();
        }
        font.materialization.map_bound = true;
        if let Some(mapping) =
            mapping.filter(|_| self.items[number].materialization.pdf_kind == "scalable")
        {
            self.bind_encoding(number, &mapping)?;
        }
        Ok(())
    }
    pub fn bind_encoding(&mut self, number: usize, mapping: &FontMapEntry) -> Result<()> {
        let Some(encoding) = self
            .encoding_file
            .clone()
            .or_else(|| mapping.encoding.clone())
            .filter(|text| !text.is_empty())
        else {
            return Ok(());
        };
        if !self.encoding_cache.contains_key(&encoding) {
            let directory = mapping
                .directory
                .as_deref()
                .map(PathBuf::from)
                .or_else(|| {
                    self.items[number]
                        .tfm_file
                        .as_deref()
                        .and_then(|file| Path::new(file).parent())
                        .map(Path::to_path_buf)
                })
                .unwrap_or_else(|| PathBuf::from("."));
            let filename = if self.encoding_file.is_some() {
                PathBuf::from(&encoding)
            } else {
                octet_path(&encoding)?
            };
            let path = self
                .resolve(&filename, &directory, "enc files")?
                .ok_or_else(|| {
                    self.missing_resolved(
                        Error::value(format!("Missing mapped font encoding: {encoding}")),
                        "enc files",
                        &filename,
                        &directory,
                    )
                })?;
            let mut parser = EncFileParser::default();
            if !parser.parse(&path, true) {
                return Err(Error::value(format!(
                    "Invalid mapped font encoding: {}",
                    path.display()
                )));
            }
            self.encoding_cache.insert(
                encoding.clone(),
                (
                    path.to_string_lossy().into_owned(),
                    Rc::new(RefCell::new(parser)),
                ),
            );
        }
        let (path, parser) = self.encoding_cache.get(&encoding).unwrap();
        self.items[number].materialization.enc_file = Some(path.clone());
        self.items[number].materialization.encoding = Some(parser.clone());
        Ok(())
    }
    pub fn materialize(
        &mut self,
        number: usize,
        suppress_duplicates: bool,
        warn: &mut dyn FnMut(&str),
    ) -> Result<()> {
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase(
            "font_materialize",
            &format!(
                "{}@{} ready={}",
                self.items[number].name,
                self.items[number].size,
                self.items[number].materialization.resource_ready
            ),
            None,
        );
        if self.items[number].materialization.resource_ready {
            return Ok(());
        }
        self.bind(number, suppress_duplicates, warn)?;
        if let Some(mapping) = self.items[number].materialization.bound_mapping.clone() {
            self.bind_encoding(number, &mapping)?;
            let directory = mapping
                .directory
                .as_deref()
                .map(PathBuf::from)
                .or_else(|| {
                    self.items[number]
                        .tfm_file
                        .as_deref()
                        .and_then(|file| Path::new(file).parent())
                        .map(Path::to_path_buf)
                })
                .unwrap_or_else(|| PathBuf::from("."));
            if self.items[number].materialization.pdf_kind == "bitmap" {
                if let Some(encoding) = &self.items[number].materialization.encoding {
                    let mut encoding = encoding.borrow_mut();
                    let mut seen = HashSet::new();
                    for (code, name) in encoding.glyph_names.iter_mut().enumerate() {
                        if name == ".notdef" {
                            continue;
                        }
                        if !seen.insert(name.clone()) {
                            warn(&format!(
                                "{}: duplicate glyph name at position {code}: {name}",
                                mapping
                                    .encoding
                                    .as_deref()
                                    .or(self.encoding_file.as_deref())
                                    .unwrap_or("None")
                            ));
                            *name = ".notdef".into();
                        }
                    }
                }
                self.bind_bitmap(number, warn)?;
            } else if let Some(outline) = &mapping.fontfile {
                let suffix = Path::new(outline)
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_ascii_lowercase();
                let kind = match suffix.as_str() {
                    "ttf" | "ttc" => "truetype fonts",
                    "otf" => "opentype fonts",
                    _ => "type1 fonts",
                };
                let filename = octet_path(outline)?;
                let path = self.resolve(&filename, &directory, kind)?.ok_or_else(|| {
                    self.missing_resolved(
                        Error::value(format!("Missing mapped font outline: {outline}")),
                        kind,
                        &filename,
                        &directory,
                    )
                })?;
                let mut resource = read_shared_font_resource(&path, warn)?;
                if mapping.slant != 0 || mapping.extend != 0 {
                    resource = match resource.as_ref() {
                        FontResource::Type1(resource) => Arc::new(FontResource::Type1(
                            resource.transformed(mapping.slant, mapping.extend)?,
                        )),
                        _ => {
                            return Err(Error::value(
                                "Outline transformations require a Type1 font",
                            ));
                        }
                    };
                }
                self.install_resource(number, path, resource, true);
            } else if self.items[number].materialization.nonembedded {
                let font = &mut self.items[number];
                font.materialization.resource = None;
                font.materialization.pfb_data = None;
                font.materialization.pfb_file = None;
                font.materialization.pdf_name =
                    mapping.ps_name.unwrap_or_else(|| font.name.clone());
                font.materialization.embedding_available = false;
                if font.materialization.map_flags.is_none() {
                    let flags = STANDARD_FONT_FLAGS
                        .iter()
                        .find(|(name, _)| *name == font.materialization.pdf_name)
                        .map_or(34, |(_, flags)| *flags);
                    warn(&format!(
                        "No flags specified for non-embedded font `{}' ({}) (I'm using {flags}): fix your map entry.",
                        font.materialization.pdf_name, font.name
                    ));
                }
            }
        } else {
            let font = &self.items[number];
            if font.materialization.pdf_kind == "bitmap"
                && font.tfm_data.is_some()
                && font.materialization.resource_error.is_none()
            {
                self.bind_bitmap(number, warn)?;
            } else if font.materialization.resource.is_none()
                && font.materialization.pfb_file.is_some()
            {
                let path = PathBuf::from(font.materialization.pfb_file.as_ref().unwrap());
                let resource = read_shared_font_resource(&path, warn)?;
                self.install_resource(number, path, resource, true);
            } else if font.materialization.resource.is_none()
                && font.tfm_data.is_some()
                && font.materialization.resource_error.is_none()
            {
                self.bind_bitmap(number, warn)?;
            }
        }
        self.items[number].materialization.resource_ready = true;
        Ok(())
    }
    fn install_resource(
        &mut self,
        number: usize,
        path: PathBuf,
        resource: impl Into<Arc<FontResource>>,
        native_encoding: bool,
    ) {
        let resource = resource.into();
        let font = &mut self.items[number];
        font.materialization.pfb_file = Some(path.to_string_lossy().into_owned());
        font.materialization.pfb_data = Some(Arc::from(resource.data()));
        font.materialization.native_encoding = if native_encoding {
            Some(Arc::from(resource.encoding()))
        } else {
            None
        };
        font.materialization.pdf_name = resource.name().into();
        font.materialization.resource = Some(resource);
        font.materialization.embedding_available = true;
    }
    pub fn freeze_pdf_options(
        &mut self,
        values: &mut BTreeMap<ValueKey, Quantity>,
        tokens_text: &dyn Fn(&[crate::language::character_tokens::Token]) -> String,
    ) -> Result<()> {
        if self.pdf_options.is_some() {
            return Ok(());
        }
        let mut resolution = integer(values, "pdfpkresolution", 0);
        if resolution == 0 {
            let raw = self.search.policy.raw_value("DPI", true);
            resolution = if raw.is_empty() {
                72
            } else {
                raw.parse()
                    .map_err(|_| Error::value("Invalid bitmap resolution"))?
            };
            values.insert(
                ValueKey::parameter("pdfpkresolution"),
                Quantity::Integer(resolution),
            );
        }
        self.pdf_options = Some(BitmapOptions {
            resolution: resolution.clamp(72, 8000),
            digits: 3,
        });
        let mode = match values.get(&ValueKey::parameter("pdfpkmode")) {
            Some(Quantity::Tokens(tokens)) if !tokens.is_empty() => tokens_text(tokens),
            _ => "/".into(),
        };
        self.search
            .policy
            .runtime_variables
            .insert("MAKETEX_MODE".into(), mode);
        self.search.policy.runtime_variables.insert(
            "MAKETEX_BASE_DPI".into(),
            self.pdf_options.unwrap().resolution.to_string(),
        );
        Ok(())
    }
    pub fn note_pdf_use(
        &self,
        values: &mut BTreeMap<ValueKey, Quantity>,
        warn: &mut dyn FnMut(&str),
    ) {
        if integer(values, "pdfmovechars", 0) > 0 {
            warn(r"Primitive \pdfmovechars is obsolete.");
            values.insert(ValueKey::parameter("pdfmovechars"), Quantity::Integer(0));
        }
    }
    pub fn bind_bitmap(&mut self, number: usize, warn: &mut dyn FnMut(&str)) -> Result<()> {
        let font = &self.items[number];
        let name = font.name.clone();
        let directory = font
            .tfm_file
            .as_deref()
            .and_then(|file| Path::new(file).parent())
            .ok_or_else(|| Error::value("Bitmap font requires metric path"))?
            .to_path_buf();
        let (path, resource) = if let Some(path) =
            self.resolve(Path::new(&format!("{name}.pgc")), &directory, "misc fonts")?
        {
            let resource = BitmapResource::read_pgc(&path)?;
            (path, resource)
        } else {
            let font = &self.items[number];
            let base = self
                .pdf_options
                .ok_or_else(|| Error::value("PDF bitmap options not frozen"))?
                .resolution;
            let design = font
                .tfm_design_size
                .filter(|size| *size != 0)
                .ok_or_else(|| Error::value("Invalid bitmap font design size"))?;
            let dpi = (base as f64 * pdf_font_size(font.size) as f64 / design as f64 + 0.5) as i64;
            let (dpi, _) = crate::resources::generation::magstep_fix(
                &BigInt::from(dpi),
                &BigInt::from(base),
                self.exact_repro,
            )?;
            let dpi = dpi
                .to_i64()
                .ok_or_else(|| Error::value("Bitmap DPI exceeds host integer"))?;
            let found = self.glyph_resources.find(
                &mut self.search,
                &self.directory,
                self.explicit_directory,
                &name,
                dpi,
                warn,
            )?;
            let tolerance = (dpi as f64 / 500.0 + 1.0) as i64;
            let Some(found) = found.filter(|found| {
                found.name == name && (found.dpi as i128 - dpi as i128).abs() <= tolerance as i128
            }) else {
                self.items[number].materialization.resource_error =
                    Some(format!("Font {name} at {dpi} not found (PK)"));
                return Ok(());
            };
            let resource = BitmapResource::read_pk(&found.path, found.dpi)?;
            (found.path, resource)
        };
        self.install_resource(number, path, FontResource::Bitmap(resource), false);
        self.items[number].materialization.pdf_name = name;
        self.items[number].materialization.nonembedded = false;
        Ok(())
    }
    pub fn check_parameter(&mut self, number: usize, index: i64) -> Result<()> {
        if index <= 0 {
            return Err(Error::value("Font parameter out of range"));
        }
        let length = self.items.len();
        let font = &mut self.items[number];
        if index as usize > font.parameter_count {
            if number != length - 1 {
                return Err(Error::value(format!(
                    "Font {} has only {} fontdimen parameters; only the last loaded font can be extended",
                    font.name, font.parameter_count
                )));
            }
            font.parameter_count = index as usize;
        }
        Ok(())
    }
    pub fn disable_ligatures(font: &mut FontInfo) {
        font.ligatures_disabled = true;
        font.tfm_lig_start = Some(Default::default());
        if let Some(tfm) = &mut font.tfm_data {
            let tfm = Arc::make_mut(tfm);
            tfm.lig_kern_start.clear();
            for (tag, _) in tfm.char_tags.values_mut() {
                if *tag == CharacterTag::Ligature {
                    *tag = CharacterTag::None;
                }
            }
        }
    }
    pub fn copy(&mut self, number: usize) -> Result<usize> {
        if reached(self.capacities.font_max, self.items.len() - 1) {
            return Err(Error::value("Font capacity exceeded")
                .with_issue(crate::core::recovery::Issue::Capacity));
        }
        let source = &self.items[number];
        let mut font = source.clone();
        font.micro_codes = Rc::new(RefCell::new(source.micro_codes.borrow().clone()));
        font.virtual_font = None;
        font.virtual_checked = false;
        let materialization = &mut font.materialization;
        materialization.map_bound = false;
        materialization.bound_mapping = None;
        materialization.resource_ready = false;
        materialization.resource = None;
        materialization.pfb_data = None;
        materialization.included_codes.clear();
        materialization.font_attributes.clear();
        materialization.no_builtin_tounicode = false;
        materialization.subset = false;
        materialization.map_identity = 0;
        if let Some(tfm) = &mut font.tfm_data {
            let tfm = Arc::make_mut(tfm);
            tfm.memory_words += tfm.bc as usize;
        }
        if source.letterspacing.is_some() {
            font.virtual_font = source.virtual_font.clone();
            font.virtual_checked = true;
        }
        font.materialization.embedding_available = font.materialization.pfb_file.is_some();
        let number = self.items.len();
        self.items.push(font);
        Ok(number)
    }
    pub fn letterspace(&mut self, number: usize, amount: i64, no_ligatures: bool) -> Result<usize> {
        let source = &self.items[number];
        let tfm = source
            .tfm_file
            .as_deref()
            .filter(|_| !source.null_font)
            .ok_or_else(|| Error::value("Letterspacing requires a loaded TFM font"))?;
        let mut font = (self.factory)(
            &source.name,
            &source.materialization.pdf_name,
            source.size as f64 / C::UNITY as f64,
            Some(Path::new(tfm)),
            None,
            None,
            &self.locations,
        )?;
        font.name = format!(
            "{}{}{amount}ls",
            source.name,
            if amount > 0 { "+" } else { "" }
        );
        if font.get_parameter(6) == 0 && source.get_parameter(6) != 0 {
            font.parameter_overrides.insert(6, source.get_parameter(6));
        }
        if no_ligatures {
            Self::disable_ligatures(&mut font);
        }
        install_letterspacing(&mut font, number, amount)?;
        let number = self.items.len();
        self.items.push(font);
        Ok(number)
    }
    pub fn glyph_name(&self, number: usize, code: u32) -> (Option<String>, &'static str) {
        let font = &self.items[number];
        if let Some(program) = &font.unicode_font {
            return (
                program
                    .cmap
                    .get(&code)
                    .map(|&glyph| program.glyph_names[glyph as usize].clone()),
                "unicode_cmap",
            );
        }
        // A glyph lookup needs one name, not a cloned 256-entry encoding.
        // Preserve the reference's explicit/native/standard fallback precedence.
        let encoding_name = font
            .materialization
            .encoding
            .as_ref()
            .and_then(|encoding| {
                let encoding = encoding.borrow();
                (!encoding.glyph_names.is_empty())
                    .then(|| encoding.glyph_names.get(code as usize).cloned())
            })
            .or_else(|| {
                font.materialization
                    .native_encoding
                    .as_ref()
                    .and_then(|encoding| {
                        (!encoding.is_empty()).then(|| encoding.get(code as usize).cloned())
                    })
            })
            .or_else(|| {
                (font.afm.is_some() || font.materialization.nonembedded).then(|| {
                    crate::fonts::formats::type1::STANDARD_ENCODING
                        .get(code as usize)
                        .map(|name| name.to_string())
                })
            });
        if let Some(name) = encoding_name {
            let source = if font.materialization.encoding.is_some() {
                "encoding"
            } else if font.materialization.resource.is_some() {
                "type1_encoding"
            } else {
                "standard_encoding"
            };
            return (name, source);
        }
        match code {
            33..=126 => (
                Some(char::from_u32(code).unwrap().to_string()),
                "fallback_label",
            ),
            32 => (Some("space".into()), "fallback_label"),
            _ => (None, "unresolved"),
        }
    }
    pub fn emitted_advance(&self, number: usize, code: u32) -> Result<Option<f64>> {
        let font = &self.items[number];
        if font.unicode_font.is_some() {
            return Ok(Some(sp_to_bp(font.get_char_width(code))));
        }
        if font.materialization.embedding_available
            && font
                .tfm_widths
                .as_ref()
                .is_some_and(|widths| widths.contains_key(&code))
        {
            if let Some(FontResource::Bitmap(resource)) = font.materialization.resource.as_deref() {
                let options = self
                    .pdf_options
                    .as_ref()
                    .ok_or_else(|| Error::value("PDF bitmap options not frozen"))?;
                return Ok(Some(
                    resource.char_width(
                        font.size,
                        code as u8,
                        font.get_char_width(code),
                        options,
                    )? * resource.font_scale(font.size, options)?
                        * sp_to_bp(font.size),
                ));
            }
            return Ok(Some(
                font.pdf_char_width(code)? * sp_to_bp(font.size) / 1000.0,
            ));
        }
        Ok(font
            .afm
            .as_ref()
            .and_then(|afm| afm.widths.get(&code))
            .map(|&width| width as f64 * sp_to_bp(font.size) / 1000.0))
    }
    pub fn dump_state<R>(
        &self,
        mut resource: impl FnMut(Option<&str>) -> Result<Option<R>>,
    ) -> Result<CatalogSnapshot<R>> {
        let mut fonts = Vec::new();
        for font in &self.items {
            fonts.push(FontSnapshot {
                name: font.name.clone(),
                unicode: resource(font.unicode_font.as_ref().map(|font| font.path.as_str()))?,
                pdf_name: font.materialization.pdf_name.clone(),
                size: font.size,
                tfm: resource(font.tfm_file.as_deref())?,
                pfb: resource(font.materialization.pfb_file.as_deref())?,
                enc: resource(font.materialization.enc_file.as_deref())?,
                parameters: font.parameter_overrides.clone(),
                parameter_count: font.parameter_count,
                hyphenchar: font.hyphenchar,
                skewchar: font.skewchar,
                null_font: font.null_font,
                dynamic: font.dynamic,
                ligatures_disabled: font.ligatures_disabled,
                micro_codes: font.micro_codes.clone(),
                width_overrides: font.width_overrides.clone(),
                letterspacing: font.letterspacing,
                italic_overrides: font.italic_overrides.clone(),
                expansion_policy: font.expansion_policy,
                expansion_base: font.expansion_base,
                expansion_ratio: font.expansion_ratio,
                expansion_variants: font.expansion_variants.clone(),
                expanded_kerns: if font.expansion_ratio != 0 {
                    font.tfm_kerns.clone()
                } else {
                    None
                },
                table_words: font.tfm_data.as_ref().map_or(7, |tfm| tfm.memory_words),
            });
        }
        Ok(CatalogSnapshot {
            fonts,
            loaded: self
                .loaded
                .iter()
                .map(|(key, &number)| (key.clone(), number))
                .collect(),
            current: self.current,
        })
    }
    pub fn restore_state<R>(
        &mut self,
        state: CatalogSnapshot<R>,
        mut resource: impl FnMut(Option<R>, &str) -> Result<Option<PathBuf>>,
    ) -> Result<()> {
        let mut fonts = Vec::new();
        for saved in state.fonts {
            let tfm = resource(saved.tfm, "tfm")?;
            let pfb = resource(saved.pfb, "type1 fonts")?;
            let enc = resource(saved.enc, "enc files")?;
            let mut font = if saved.null_font {
                Self::null_font()
            } else if let Some(unicode) = saved.unicode {
                let path = resource(Some(unicode), "truetype fonts")?
                    .ok_or_else(|| Error::value("Missing format Unicode resource"))?;
                FontInfo {
                    name: saved.name,
                    size: saved.size,
                    unicode_font: Some(Arc::new(UnicodeFont::read(&path, saved.size)?)),
                    materialization: FontMaterialization {
                        pdf_name: saved.pdf_name,
                        map_bound: true,
                        embedding_available: true,
                        ..FontMaterialization::default()
                    },
                    ..FontInfo::default()
                }
            } else {
                let mut font = (self.factory)(
                    &saved.name,
                    &saved.pdf_name,
                    saved.size as f64 / 65536.0,
                    tfm.as_deref(),
                    if saved.dynamic { None } else { pfb.as_deref() },
                    enc.as_deref(),
                    &self.locations,
                )?;
                font.materialization.pfb_file =
                    pfb.as_ref().map(|path| path.to_string_lossy().into_owned());
                if font.tfm_data.is_none() {
                    return Err(Error::value("Format font has no TFM metrics"));
                }
                font.materialization.native_encoding = font
                    .materialization
                    .resource
                    .as_ref()
                    .map(|resource| Arc::from(resource.encoding()));
                font
            };
            font.parameter_overrides = saved.parameters;
            if let Some(tfm) = &mut font.tfm_data {
                Arc::make_mut(tfm).memory_words = saved.table_words;
            }
            font.parameter_count = saved.parameter_count;
            font.hyphenchar = saved.hyphenchar;
            font.skewchar = saved.skewchar;
            font.dynamic = saved.dynamic;
            if saved.ligatures_disabled {
                Self::disable_ligatures(&mut font);
            }
            font.micro_codes = saved.micro_codes;
            let tags = font
                .micro_codes
                .borrow()
                .get(&FontCode::Tag)
                .cloned()
                .unwrap_or_default();
            for (character, value) in tags {
                set_code(&mut font, FontCode::Tag, character, value);
            }
            if let Some((base, amount)) = saved.letterspacing {
                install_letterspacing(&mut font, base, amount)?;
            }
            font.width_overrides = saved.width_overrides;
            font.italic_overrides = saved.italic_overrides;
            font.expansion_policy = saved.expansion_policy;
            font.expansion_base = saved.expansion_base;
            font.expansion_ratio = saved.expansion_ratio;
            font.expansion_variants = saved.expansion_variants;
            if saved.expanded_kerns.is_some() {
                font.tfm_kerns = saved.expanded_kerns;
            }
            font.materialization.embedding_available = font.unicode_font.is_some()
                || ((font.materialization.resource.is_some()
                    || font.materialization.pfb_file.is_some())
                    && font
                        .tfm_widths
                        .as_ref()
                        .is_some_and(|widths| !widths.is_empty()));
            fonts.push(font);
        }
        self.items = fonts;
        self.loaded = state.loaded.into_iter().collect();
        self.current = state.current;
        self.ini = true;
        Ok(())
    }
}
impl ExpansionCatalog for FontCatalog {
    fn items(&self) -> &[FontInfo] {
        &self.items
    }
    fn items_mut(&mut self) -> &mut Vec<FontInfo> {
        &mut self.items
    }
    fn load(&mut self, name: &str, directory: &Path, size: i64) -> Result<usize> {
        Self::load(self, name, directory, Some(size), 1000)
    }
}
impl VirtualCatalog for FontCatalog {
    fn load(&mut self, name: &str, directory: &Path, size: i64) -> Result<usize> {
        Self::load(self, name, directory, Some(size), 1000)
    }
    fn selection(&self, number: usize) -> VirtualSelection {
        let font = &self.items[number];
        VirtualSelection {
            name: font.name.clone(),
            size: font.size,
            design_size: font.tfm_design_size.unwrap_or(0),
            checksum: font.tfm_data.as_ref().map_or(0, |tfm| tfm.checksum),
            virtual_font: font.virtual_font.clone(),
        }
    }
    fn char_width(&self, number: usize, code: u32) -> i64 {
        self.items[number].get_char_width(code)
    }
    fn bind(&mut self, number: usize, warn: &mut dyn FnMut(&str)) -> Result<()> {
        Self::bind(self, number, false, warn)
    }
}
