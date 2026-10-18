//! Ordered input/font lookup and job-local alias caches (`resources/search.py`).
use super::generation::ResourceGeneration;
use super::locations::ResourceLocations;
use super::paths::{PathPolicy, SearchElement, expand_user, read_latin1, resolve_path};
use crate::core::errors::{Error, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
pub const FONT_PATHS: &[(&str, &str)] = &[
    ("tfm", "TFMFONTS"),
    ("type1 fonts", "T1FONTS"),
    ("enc files", "ENCFONTS"),
    ("map", "TEXFONTMAPS"),
    ("vf", "VFFONTS"),
    ("truetype fonts", "TTFONTS"),
    ("opentype fonts", "OPENTYPEFONTS"),
    ("pk", "PKFONTS"),
    ("sfd", "SFDFONTS"),
    ("misc fonts", "TEXFONTS"),
];
#[derive(Clone)]
pub struct ResourceSearch {
    pub directories: HashMap<SearchElement, Vec<PathBuf>>,
    pub policy: PathPolicy,
    pub generation: ResourceGeneration,
    pub font_aliases: Option<BTreeMap<String, Vec<String>>>,
}
impl ResourceSearch {
    pub fn new(
        program: &str,
        output: Option<PathBuf>,
        exact_repro: bool,
        locations: ResourceLocations,
    ) -> Result<Self> {
        Ok(Self {
            directories: HashMap::new(),
            policy: PathPolicy::new(program, output, locations)?,
            generation: ResourceGeneration { exact_repro },
            font_aliases: None,
        })
    }
    pub fn roots(&self, variable: &str, defaults: &[(PathBuf, bool)]) -> Vec<SearchElement> {
        self.policy.roots(variable, defaults)
    }
    pub fn aliases(&mut self, name: &str) -> Result<Vec<PathBuf>> {
        if self.font_aliases.is_none() {
            self.font_aliases = Some(BTreeMap::new());
            let roots = self.roots("TEXFONTMAPS", &[]);
            let mut seen = HashSet::new();
            for root in &roots {
                for path in self.matches(
                    &[PathBuf::from("texfonts.map")],
                    std::slice::from_ref(root),
                    false,
                    None,
                )? {
                    if seen.insert(path.clone()) {
                        self.read_alias_file(&path, &roots, &mut Vec::new())?;
                    }
                }
            }
        }
        let requested = Path::new(name);
        let suffix = requested
            .extension()
            .map(|suffix| format!(".{}", suffix.to_string_lossy()))
            .unwrap_or_default();
        let mappings = self.font_aliases.as_ref().expect("alias cache");
        let found = mappings.get(name).or_else(|| {
            if !suffix.is_empty() {
                mappings.get(requested.with_extension("").to_string_lossy().as_ref())
            } else {
                None
            }
        });
        Ok(found
            .into_iter()
            .flatten()
            .map(|mapping| {
                if Path::new(mapping).extension().is_some() || suffix.is_empty() {
                    mapping.into()
                } else {
                    format!("{mapping}{suffix}").into()
                }
            })
            .collect())
    }
    fn read_alias_file(
        &mut self,
        path: &Path,
        roots: &[SearchElement],
        active: &mut Vec<PathBuf>,
    ) -> Result<()> {
        if active.iter().any(|known| known == path) {
            return Err(Error::value("Recursive texfonts.map include"));
        }
        active.push(path.into());
        for line in read_latin1(path)?.lines() {
            let line = if let Some((before, _)) = line.rsplit_once('%') {
                before
            } else {
                line.split_once("@c").map_or(line, |(before, _)| before)
            };
            let words: Vec<_> = line.split_whitespace().collect();
            if words.len() < 2 {
                continue;
            }
            if words[0] == "include" {
                if let Some(included) =
                    self.resolve(&[PathBuf::from(words[1])], roots, false, None)?
                {
                    self.read_alias_file(&included, roots, active)?;
                }
            } else {
                self.font_aliases
                    .as_mut()
                    .expect("alias cache")
                    .entry(words[1].into())
                    .or_default()
                    .push(words[0].into());
            }
        }
        active.pop();
        Ok(())
    }
    pub fn resolve(
        &mut self,
        names: &[PathBuf],
        roots: &[SearchElement],
        must_exist: bool,
        retry_names: Option<&[PathBuf]>,
    ) -> Result<Option<PathBuf>> {
        // Short-circuiting matters: Python's next(matches(...)) does not populate
        // the remaining directory caches after finding its first candidate.
        Ok(self
            .collect_matches(names, roots, must_exist, retry_names, true)?
            .into_iter()
            .next())
    }
    pub fn matches(
        &mut self,
        names: &[PathBuf],
        roots: &[SearchElement],
        must_exist: bool,
        retry_names: Option<&[PathBuf]>,
    ) -> Result<Vec<PathBuf>> {
        self.collect_matches(names, roots, must_exist, retry_names, false)
    }
    fn collect_matches(
        &mut self,
        names: &[PathBuf],
        roots: &[SearchElement],
        must_exist: bool,
        retry_names: Option<&[PathBuf]>,
        first_only: bool,
    ) -> Result<Vec<PathBuf>> {
        let mut result = Vec::new();
        for disk_retry in [false, true] {
            if disk_retry && !must_exist {
                break;
            }
            let candidates = if disk_retry {
                retry_names.unwrap_or(names)
            } else {
                names
            };
            for entry in roots {
                if !disk_retry {
                    for name in candidates {
                        for path in self.policy.database_matches(entry, name)? {
                            result.push(resolve_path(&path));
                            if first_only {
                                return Ok(result);
                            }
                        }
                    }
                }
                if entry.database_only || (!disk_retry && self.policy.database_relevant(entry)?) {
                    continue;
                }
                let directories = self
                    .directories
                    .entry(entry.clone())
                    .or_insert_with(|| entry.directories());
                let absent: Vec<_> = candidates
                    .iter()
                    .map(|name| crate::resources::host::file_name_absent(name))
                    .collect();
                if absent.iter().all(|absent| *absent) {
                    continue;
                }
                let members: Vec<_> = candidates
                    .iter()
                    .zip(&absent)
                    .map(|(name, absent)| {
                        if *absent {
                            None
                        } else {
                            crate::resources::host::file_name_candidates(name)
                        }
                    })
                    .collect();
                for directory in directories {
                    for ((name, absent), members) in candidates.iter().zip(&absent).zip(&members) {
                        if *absent {
                            continue;
                        }
                        let path = directory.join(name);
                        // Virtual namespace paths are normalized. Parent-relative
                        // or relative paths retain the original metadata probe.
                        if path.is_absolute()
                            && !path.components().any(|component| {
                                matches!(component, std::path::Component::ParentDir)
                            })
                            && members
                                .as_ref()
                                .is_some_and(|members| !members.contains(&path))
                        {
                            continue;
                        }
                        if crate::resources::host::is_file(&path) {
                            result.push(resolve_path(&path));
                            if first_only {
                                return Ok(result);
                            }
                        }
                    }
                }
            }
        }
        Ok(result)
    }
    pub fn make(&mut self, kind: &str, name: &str) -> Result<Option<PathBuf>> {
        let path = self.generation.make(&mut self.policy, kind, name)?;
        if let Some(path) = &path {
            for (root, (_, entries)) in &mut self.policy.databases {
                if !entries.is_empty() && path.starts_with(resolve_path(root)) {
                    entries
                        .entry(
                            path.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned(),
                        )
                        .or_default()
                        .push(path.clone());
                }
            }
        }
        Ok(path)
    }
}
pub fn filesystem_name(tokens: &str) -> Result<std::ffi::OsString> {
    let bytes: Vec<_> = tokens
        .chars()
        .map(|c| {
            u8::try_from(c as u32).map_err(|_| Error::value("Filename is not an octet string"))
        })
        .collect::<Result<_>>()?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(std::ffi::OsString::from_vec(bytes))
    }
    #[cfg(not(unix))]
    {
        Ok(std::ffi::OsString::from(String::from_utf8(bytes).map_err(
            |_| Error::value("This host cannot represent non-UTF-8 physical filenames"),
        )?))
    }
}
#[derive(Clone)]
pub struct InputResources {
    pub defaults: Vec<(PathBuf, bool)>,
    pub native: bool,
    pub search: ResourceSearch,
    pub last_error: Option<String>,
}
impl InputResources {
    pub fn new(
        native: bool,
        program: &str,
        output: Option<PathBuf>,
        locations: ResourceLocations,
    ) -> Result<Self> {
        Ok(Self {
            defaults: locations.input_roots.clone(),
            native,
            search: ResourceSearch::new(program, output, false, locations)?,
            last_error: None,
        })
    }
    pub fn resolve_literal(
        &mut self,
        name: &str,
        unicode: bool,
        directory: Option<&Path>,
        extension: &str,
        must_exist: bool,
        query: bool,
    ) -> Result<Option<PathBuf>> {
        let path = if unicode {
            PathBuf::from(name)
        } else {
            PathBuf::from(filesystem_name(name)?)
        };
        self.resolve_path(&path, directory, extension, must_exist, query)
    }
    pub fn resolve_path(
        &mut self,
        path: &Path,
        directory: Option<&Path>,
        extension: &str,
        must_exist: bool,
        query: bool,
    ) -> Result<Option<PathBuf>> {
        if let Some(name) = path.to_str() {
            return self.resolve(name, directory, extension, must_exist, query);
        }
        self.last_error = None;
        if !self
            .search
            .policy
            .name_allowed(&path.to_string_lossy(), false)
        {
            self.last_error = Some("Input filename not allowed (openin_any)".into());
            return Ok(None);
        }
        let mut suffixed = path.as_os_str().to_os_string();
        suffixed.push(extension);
        let names = if extension.is_empty() {
            vec![path.to_path_buf()]
        } else {
            vec![PathBuf::from(suffixed), path.to_path_buf()]
        };
        if crate::resources::host::is_absolute(&path) {
            return Ok(names
                .iter()
                .find(|p| crate::resources::host::is_file(&p))
                .map(|p| resolve_path(p)));
        }
        if let Some(output) = &self.search.policy.output_directory {
            if let Some(found) = names
                .iter()
                .map(|n| output.join(n))
                .find(|p| crate::resources::host::is_file(&p))
            {
                return Ok(Some(resolve_path(&found)));
            }
        }
        let mut roots = Vec::new();
        if !self.native {
            if let Some(dir) = directory {
                roots.push(SearchElement::new(dir.to_string_lossy().into_owned()))
            }
            roots.push(SearchElement::new(
                crate::resources::host::current_dir()?
                    .to_string_lossy()
                    .into_owned(),
            ))
        }
        roots.extend(self.search.roots("TEXINPUTS", &self.defaults));
        self.search
            .resolve(&names, &roots, must_exist, Some(&[path.to_path_buf()]))
    }
    pub fn resolve(
        &mut self,
        filename: &str,
        directory: Option<&Path>,
        extension: &str,
        must_exist: bool,
        query: bool,
    ) -> Result<Option<PathBuf>> {
        self.last_error = None;
        let requested = Path::new(filename);
        let candidate = self
            .search
            .policy
            .output_directory
            .as_ref()
            .filter(|_| !crate::resources::host::is_absolute(&requested))
            .map(|output| output.join(requested));
        if query {
            if let Some(candidate) = candidate
                .as_ref()
                .filter(|path| crate::resources::host::is_file(&path))
            {
                return Ok(Some(resolve_path(candidate)));
            }
        }
        if !self.search.policy.name_allowed(filename, false) {
            self.last_error = Some(format!(
                "Input filename not allowed (openin_any): {filename}"
            ));
            return Ok(None);
        }
        if let Some(candidate) = candidate.filter(|path| crate::resources::host::is_file(&path)) {
            return Ok(Some(resolve_path(&candidate)));
        }
        let filename = expand_user(&self.search.policy.expand(filename));
        let requested = filename.as_path();
        let names = if extension.is_empty() || requested.to_string_lossy().ends_with(extension) {
            vec![requested.into()]
        } else {
            let suffixed = PathBuf::from(format!("{}{extension}", requested.display()));
            let first = ["1", "t", "true", "y", "yes"].contains(
                &self
                    .search
                    .policy
                    .raw_value("try_std_extension_first", true)
                    .to_ascii_lowercase()
                    .as_str(),
            );
            if first || requested.extension().is_none() {
                vec![suffixed, requested.into()]
            } else {
                vec![requested.into(), suffixed]
            }
        };
        if crate::resources::host::is_absolute(&requested) {
            if let Some(found) = names
                .iter()
                .find(|path| crate::resources::host::is_file(&path))
            {
                return Ok(Some(resolve_path(found)));
            }
            return if must_exist {
                self.search.make("tex", &filename.to_string_lossy())
            } else {
                Ok(None)
            };
        }
        let mut roots = Vec::new();
        if !self.native {
            if let Some(directory) = directory {
                roots.push(SearchElement::new(directory.to_string_lossy().into_owned()));
            }
            roots.push(SearchElement::new(
                crate::resources::host::current_dir()?
                    .to_string_lossy()
                    .into_owned(),
            ));
        }
        roots.extend(self.search.roots("TEXINPUTS", &self.defaults));
        let path = self
            .search
            .resolve(&names, &roots, must_exist, Some(&[requested.into()]))?;
        if path.is_some() || !must_exist {
            Ok(path)
        } else {
            self.search.make("tex", &filename.to_string_lossy())
        }
    }
}
