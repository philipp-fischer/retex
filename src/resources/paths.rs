//! Kpathsea configuration and literal filename policy (`resources/paths.py`).
use super::locations::ResourceLocations;
use crate::core::errors::{Error, Result};
use regex::Regex;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

pub fn brace_expand(value: &str) -> Vec<String> {
    // Use a work stack so finite deeply nested path expressions do not consume
    // the Rust call stack. Reverse pending alternatives to preserve source order.
    let mut pending = vec![value.to_string()];
    let mut result = Vec::new();
    while let Some(value) = pending.pop() {
        let Some(start) = value.find('{') else {
            result.push(value);
            continue;
        };
        let (mut depth, mut begin) = (1, start + 1);
        let mut parts = Vec::new();
        let mut matched = false;
        for (relative, character) in value[start + 1..].char_indices() {
            let end = start + 1 + relative;
            if character == '{' {
                depth += 1;
            } else if character == '}' {
                depth -= 1;
            }
            if character == ',' && depth == 1 {
                parts.push(value[begin..end].to_string());
                begin = end + 1;
            }
            if depth == 0 {
                parts.push(value[begin..end].to_string());
                for part in parts.into_iter().rev() {
                    pending.push(format!("{}{part}{}", &value[..start], &value[end + 1..]));
                }
                matched = true;
                break;
            }
        }
        if !matched {
            result.push(value);
        }
    }
    result
}
pub fn expand_user(value: &str) -> PathBuf {
    if value == "~" || value.starts_with("~/") {
        if let Some(home) = crate::resources::host::var_os("HOME") {
            return if value == "~" {
                home.into()
            } else {
                PathBuf::from(home).join(&value[2..])
            };
        }
    }
    // ~username expansion reads the public passwd database, as Python's Unix
    // expanduser does; this never changes process-wide environment variables.
    if let Some(tail) = value.strip_prefix('~') {
        let (name, suffix) = tail
            .split_once('/')
            .map_or((tail, ""), |(name, suffix)| (name, suffix));
        if !name.is_empty() {
            if let Ok(passwd) = crate::resources::host::read_to_string("/etc/passwd") {
                for line in passwd.lines() {
                    let fields: Vec<_> = line.split(':').collect();
                    if fields.len() >= 6 && fields[0] == name {
                        return PathBuf::from(fields[5]).join(suffix);
                    }
                }
            }
        }
    }
    value.into()
}
pub fn absolute(path: &Path) -> PathBuf {
    if crate::resources::host::is_absolute(&path) {
        path.into()
    } else {
        crate::resources::host::current_dir()
            .unwrap_or_else(|_| ".".into())
            .join(path)
    }
}
pub fn resolve_path(path: &Path) -> PathBuf {
    crate::resources::host::canonicalize(path).unwrap_or_else(|_| absolute(path))
}
pub fn read_latin1(path: &Path) -> Result<String> {
    Ok(crate::resources::host::read(path)?
        .iter()
        .map(|&b| char::from(b))
        .collect())
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SearchElement {
    pub pattern: String,
    pub database_only: bool,
}
impl SearchElement {
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            database_only: false,
        }
    }
    pub fn root(&self) -> PathBuf {
        let prefix = self.pattern.split("//").next().unwrap_or("");
        PathBuf::from(if prefix.is_empty() { "/" } else { prefix })
    }
    pub fn directories(&self) -> Vec<PathBuf> {
        let mut parts = self.pattern.split("//");
        let first = parts.next().unwrap_or("");
        let mut roots = vec![PathBuf::from(if first.is_empty() { "/" } else { first })];
        for suffix in parts {
            let mut following = Vec::new();
            for root in roots {
                if crate::resources::host::is_dir(&root) {
                    let mut pending = vec![root];
                    let mut seen = HashSet::new();
                    while let Some(current) = pending.pop() {
                        let Ok(_metadata) = crate::resources::host::metadata(&current) else {
                            continue;
                        };
                        #[cfg(unix)]
                        let identity = { (_metadata.dev(), _metadata.ino()) };
                        #[cfg(not(unix))]
                        let identity = resolve_path(&current);
                        if !seen.insert(identity) {
                            continue;
                        }
                        let candidate = current.join(suffix.trim_start_matches('/'));
                        if crate::resources::host::is_dir(&candidate) {
                            following.push(candidate);
                        }
                        let Ok(children) = crate::resources::host::search_subdirectories(&current)
                        else {
                            continue;
                        };
                        pending.extend(children.into_iter().rev());
                    }
                }
            }
            roots = following;
        }
        roots
    }
    pub fn contains(&self, directory: &Path) -> bool {
        let mut pattern = regex::escape(&absolute(&self.root()).to_string_lossy());
        for suffix in self.pattern.split("//").skip(1) {
            pattern.push_str(r"(?:/[^/]+)*");
            let suffix = suffix.trim_matches('/');
            if !suffix.is_empty() {
                pattern.push('/');
                pattern.push_str(&regex::escape(suffix));
            }
        }
        let expression = format!("^{}$", pattern.trim_end_matches('/'));
        Regex::new(&expression)
            .expect("escaped literal path pattern")
            .is_match(absolute(directory).to_string_lossy().trim_end_matches('/'))
    }
}
pub type Stamp = (i128, u64);
pub type DatabaseEntries = BTreeMap<String, Vec<PathBuf>>;
pub type AliasEntries = BTreeMap<String, Vec<String>>;
#[derive(Clone)]
pub struct PathPolicy {
    pub program: String,
    pub locations: ResourceLocations,
    pub configuration: BTreeMap<String, String>,
    pub configuration_files: Vec<PathBuf>,
    pub runtime_variables: BTreeMap<String, String>,
    pub output_directory: Option<PathBuf>,
    pub databases: BTreeMap<PathBuf, (Stamp, DatabaseEntries)>,
    pub aliases: BTreeMap<PathBuf, (Option<Stamp>, AliasEntries)>,
    pub builtins: BTreeMap<String, String>,
    pub database_order: Vec<PathBuf>,
}
impl PathPolicy {
    pub fn new(
        program: &str,
        output_directory: Option<PathBuf>,
        locations: ResourceLocations,
    ) -> Result<Self> {
        let repo = &locations.reference_root;
        let builtins = [
            ("RETEX_REFERENCE_ROOT", repo.to_string_lossy().into_owned()),
            ("progname", program.into()),
            ("engine", "pdftex".into()),
            (
                "SELFAUTOLOC",
                repo.join("bin").to_string_lossy().into_owned(),
            ),
            ("SELFAUTODIR", repo.to_string_lossy().into_owned()),
            (
                "SELFAUTOPARENT",
                repo.parent().unwrap_or(repo).to_string_lossy().into_owned(),
            ),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect();
        let output_directory = output_directory
            .filter(|path| !path.as_os_str().is_empty())
            .or_else(|| {
                crate::resources::host::var_os("TEXMF_OUTPUT_DIRECTORY").map(PathBuf::from)
            });
        let mut policy = Self {
            program: program.into(),
            locations,
            configuration: BTreeMap::new(),
            configuration_files: Vec::new(),
            runtime_variables: BTreeMap::new(),
            output_directory,
            databases: BTreeMap::new(),
            aliases: BTreeMap::new(),
            builtins,
            database_order: Vec::new(),
        };
        if let Some(output) = &policy.output_directory {
            policy.runtime_variables.insert(
                "TEXMF_OUTPUT_DIRECTORY".into(),
                output.to_string_lossy().into_owned(),
            );
        }
        let locations = if let Some(paths) = &policy.locations.configuration_paths {
            paths.clone()
        } else if let Some(search) = crate::resources::host::var("TEXMFCNF")
            .ok()
            .filter(|search| !search.is_empty())
        {
            policy
                .expand(&search)
                .replace(';', ":")
                .split(':')
                .map(PathBuf::from)
                .collect()
        } else {
            vec![
                policy.locations.reference_root.join("assets"),
                policy.locations.reference_root.join("assets/texmf"),
            ]
        };
        let continuation = Regex::new(r"\\\r?\n\s*").expect("configuration continuation");
        let assignment =
            Regex::new(r"^\s*([\w.-]+)\s*=\s*(.*?)\s*$").expect("configuration assignment");
        for location in locations {
            for expanded in brace_expand(&location.to_string_lossy()) {
                let path = expand_user(if expanded.is_empty() { "." } else { &expanded });
                let path = if path.file_name().is_some_and(|name| name == "texmf.cnf")
                    || (policy.locations.configuration_paths.is_some()
                        && crate::resources::host::is_file(&path))
                {
                    path
                } else {
                    path.join("texmf.cnf")
                };
                if !crate::resources::host::is_file(&path) {
                    continue;
                }
                policy.configuration_files.push(resolve_path(&path));
                let text = read_latin1(&path)?;
                let text = continuation.replace_all(&text, "");
                for line in text.lines() {
                    let comment = line
                        .char_indices()
                        .find(|&(at, c)| {
                            (c == '%' || c == '#') && (at == 0 || line.as_bytes()[at - 1] != b'\\')
                        })
                        .map_or(line.len(), |(at, _)| at);
                    if let Some(matched) = assignment.captures(&line[..comment]) {
                        policy
                            .configuration
                            .entry(matched[1].into())
                            .or_insert_with(|| matched[2].replace("\\%", "%"));
                    }
                }
            }
        }
        Ok(policy)
    }
    pub fn raw_value(&self, name: &str, environment: bool) -> String {
        if let Some(value) = self.runtime_variables.get(name) {
            return value.clone();
        }
        if environment {
            for key in [
                format!("{name}.{}", self.program),
                format!("{name}_{}", self.program),
                name.into(),
            ] {
                if let Ok(value) = crate::resources::host::var(key) {
                    if !value.is_empty() {
                        return value;
                    }
                }
            }
        }
        for key in [format!("{name}.{}", self.program), name.into()] {
            if let Some(value) = self.configuration.get(&key) {
                return value.clone();
            }
        }
        self.builtins.get(name).cloned().unwrap_or_default()
    }
    pub fn name_allowed(&self, name: &str, writing: bool) -> bool {
        let variable = if writing { "openout_any" } else { "openin_any" };
        let choice = self.raw_value(variable, true);
        let choice = choice
            .chars()
            .next()
            .unwrap_or(if writing { 'p' } else { 'a' });
        if "ay1".contains(choice) {
            return true;
        }
        for (at, character) in name.char_indices() {
            if character != '.' || (at != 0 && name.as_bytes()[at - 1] != b'/') {
                continue;
            }
            let remaining = &name[at..];
            if !(remaining.starts_with("./") || remaining.starts_with("../") || remaining == ".tex")
            {
                return false;
            }
        }
        if "rn0".contains(choice) {
            return true;
        }
        if name.starts_with('/') {
            let fallback = self.raw_value("TEXMFOUTPUT", true);
            if !self
                .output_directory
                .as_ref()
                .is_some_and(|root| name.starts_with(&format!("{}/", root.display())))
                && !(!fallback.is_empty() && name.starts_with(&(fallback + "/")))
            {
                return false;
            }
        }
        !(name.starts_with("../") || name.contains("/../"))
    }
    pub fn open_output(
        &self,
        filename: &str,
        append: bool,
        _binary: bool,
    ) -> Result<crate::resources::host::Writer> {
        if !self.name_allowed(filename, true) {
            return Err(Error::value(format!(
                "Output filename not allowed (openout_any): {filename}"
            )));
        }
        let name = Path::new(filename);
        let primary = if !crate::resources::host::is_absolute(&name) {
            self.output_directory
                .as_ref()
                .map_or_else(|| name.into(), |root| root.join(name))
        } else {
            name.into()
        };
        let open = |path: &Path| crate::resources::host::open_output(path, append);
        match open(&primary) {
            Ok(file) => Ok(file),
            Err(error) => {
                let fallback = self.raw_value("TEXMFOUTPUT", true);
                if fallback.is_empty() || crate::resources::host::is_absolute(&name) {
                    return Err(error.into());
                }
                Ok(open(&PathBuf::from(fallback).join(name))?)
            }
        }
    }
    pub fn expand(&self, text: &str) -> String {
        fn expand(policy: &PathPolicy, text: &str, active: &mut Vec<String>) -> String {
            static VARIABLES: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
            let variables = VARIABLES.get_or_init(|| {
                Regex::new(r"\$\{([^}]+)\}|\$([A-Za-z_][A-Za-z_0-9]*)")
                    .expect("path variable syntax")
            });
            variables
                .replace_all(text, |matched: &regex::Captures<'_>| {
                    let name = matched
                        .get(1)
                        .or_else(|| matched.get(2))
                        .expect("path variable")
                        .as_str();
                    if active.iter().any(|key| key == name) {
                        return String::new();
                    }
                    active.push(name.into());
                    let result = expand(policy, &policy.raw_value(name, true), active);
                    active.pop();
                    result
                })
                .into_owned()
        }
        expand(self, text, &mut Vec::new())
    }
    pub fn elements(&self, value: &str) -> Vec<SearchElement> {
        let mut result = Vec::new();
        for expanded in brace_expand(&self.expand(value)) {
            for part in expanded.replace(';', ":").split(':') {
                if part.is_empty() {
                    continue;
                }
                let only = part.starts_with("!!");
                let mut part = expand_user(if only { &part[2..] } else { part })
                    .to_string_lossy()
                    .into_owned();
                if let Ok(dot) = crate::resources::host::var("KPSE_DOT") {
                    if !dot.is_empty() && !crate::resources::host::is_absolute(Path::new(&part)) {
                        part = format!("{dot}/{}", part.strip_prefix("./").unwrap_or(&part));
                    }
                }
                result.push(SearchElement {
                    pattern: part,
                    database_only: only,
                });
            }
        }
        result
    }
    pub fn roots(&self, variable: &str, defaults: &[(PathBuf, bool)]) -> Vec<SearchElement> {
        let mut fallback = self.elements(&self.raw_value(variable, false));
        fallback.extend(defaults.iter().map(|(root, recursive)| {
            SearchElement::new(format!(
                "{}{}",
                root.display(),
                if *recursive { "//" } else { "" }
            ))
        }));
        let environment = [
            format!("{variable}.{}", self.program),
            format!("{variable}_{}", self.program),
            variable.into(),
        ]
        .into_iter()
        .find_map(|key| {
            crate::resources::host::var(key)
                .ok()
                .filter(|value| !value.is_empty())
        });
        let Some(environment) = environment else {
            return fallback;
        };
        let mut result = Vec::new();
        for part in self.expand(&environment).replace(';', ":").split(':') {
            if part.is_empty() {
                result.extend(fallback.clone());
            } else {
                result.extend(self.elements(part));
            }
        }
        result
    }
    pub fn load_databases(&mut self) -> Result<()> {
        let roots = self.elements(&self.raw_value("TEXMFDBS", true));
        let mut visited = HashSet::new();
        for root in roots
            .into_iter()
            .map(|element| element.root())
            .filter(|root| visited.insert(root.clone()))
        {
            let path = root.join("ls-R");
            if !crate::resources::host::is_file(&path) {
                continue;
            }
            let alias_path = root.join("aliases");
            let alias_stamp = if crate::resources::host::is_file(&alias_path) {
                Some(stamp(&alias_path)?)
            } else {
                None
            };
            if self
                .aliases
                .get(&root)
                .is_none_or(|(saved, _)| *saved != alias_stamp)
            {
                let mut mappings: AliasEntries = BTreeMap::new();
                if alias_stamp.is_some() {
                    for line in read_latin1(&alias_path)?.lines() {
                        let fields: Vec<_> = line.split_whitespace().collect();
                        if fields.len() == 2 && !fields[0].starts_with('%') {
                            mappings
                                .entry(fields[1].into())
                                .or_default()
                                .push(fields[0].into());
                        }
                    }
                }
                self.aliases.insert(root.clone(), (alias_stamp, mappings));
            }
            let current_stamp = stamp(&path)?;
            if self
                .databases
                .get(&root)
                .is_some_and(|(saved, _)| *saved == current_stamp)
            {
                continue;
            }
            let text = read_latin1(&path)?;
            let mut lines = text.lines();
            if !lines
                .next()
                .is_some_and(|line| line.starts_with("% ls-R -- "))
            {
                continue;
            }
            let mut entries: DatabaseEntries = BTreeMap::new();
            let mut directory = root.clone();
            for line in lines {
                if let Some(header) = line.strip_suffix(':') {
                    directory = root.join(header.strip_prefix("./").unwrap_or(header));
                } else if !line.is_empty() && !line.starts_with('%') {
                    entries
                        .entry(line.into())
                        .or_default()
                        .push(directory.join(line));
                }
            }
            if !self.databases.contains_key(&root) {
                self.database_order.push(root.clone());
            }
            self.databases.insert(root, (current_stamp, entries));
        }
        Ok(())
    }
    pub fn database_relevant(&mut self, element: &SearchElement) -> Result<bool> {
        self.load_databases()?;
        if !crate::resources::host::is_absolute(&element.root()) {
            return Ok(false);
        }
        let directory = absolute(&element.root());
        Ok(self.databases.keys().any(|root| {
            directory.starts_with(absolute(root))
                || (element.pattern.contains("//") && absolute(root).starts_with(&directory))
        }))
    }
    pub fn database_matches(
        &mut self,
        element: &SearchElement,
        name: &Path,
    ) -> Result<Vec<PathBuf>> {
        self.load_databases()?;
        let mut result = Vec::new();
        let basename = name.file_name().unwrap_or_default().to_string_lossy();
        for root in &self.database_order {
            let entries = &self.databases[root].1;
            let mut names = vec![basename.to_string()];
            names.extend(
                self.aliases[root]
                    .1
                    .get(basename.as_ref())
                    .cloned()
                    .unwrap_or_default(),
            );
            for basename in names {
                for path in entries.get(&basename).into_iter().flatten() {
                    let mut directory = path.parent().unwrap_or(Path::new(".")).to_path_buf();
                    if let Some(parent) = name.parent().filter(|parent| {
                        !parent.as_os_str().is_empty() && *parent != Path::new(".")
                    }) {
                        if !directory.ends_with(parent) {
                            continue;
                        }
                        for _ in parent.components() {
                            directory.pop();
                        }
                    }
                    if element.contains(&directory) && crate::resources::host::is_file(&path) {
                        result.push(path.clone());
                    }
                }
            }
        }
        Ok(result)
    }
}
fn stamp(path: &Path) -> Result<Stamp> {
    let metadata = crate::resources::host::metadata(&path)?;
    let modified = metadata.modified_timestamp()?;
    let time =
        modified.timestamp() as i128 * 1_000_000_000 + modified.timestamp_subsec_nanos() as i128;
    Ok((time, metadata.len()))
}

#[cfg(test)]
mod timestamp_tests {
    use super::*;
    use crate::resources::{
        catalog::{CatalogDelta, ResourceCatalog, ResourceChange, ResourceId},
        host::{FixedClock, enter},
        virtual_host::{VirtualHost, resource_path},
    };
    use std::rc::Rc;

    #[test]
    fn same_size_database_changes_before_epoch_reload_cached_names() {
        let host = Rc::new(
            VirtualHost::new(
                ResourceCatalog::new(Default::default()),
                PathBuf::from("/project"),
                BTreeMap::from([("TEXMFDBS".into(), "/project/db".into())]),
                FixedClock::new(1_600_000_000, 0, 0).unwrap(),
                1000,
            )
            .unwrap(),
        );
        let put = |name: &str, bytes: &[u8], version, time| {
            let path = PathBuf::from("/project/db").join(name);
            ResourceChange::put(
                ResourceId(path.display().to_string()),
                resource_path(&path),
                version,
                bytes.into(),
                time,
            )
            .unwrap()
        };
        host.apply(CatalogDelta {
            base_epoch: 0,
            new_epoch: 1,
            policy_version: 0,
            changes: vec![
                put("ls-R", b"% ls-R -- test\n./:\na.tex\n", 1, Some(-1000)),
                put("a.tex", b"A", 1, None),
                put("b.tex", b"B", 1, None),
            ],
        })
        .unwrap();
        let _guard = enter(host.clone());
        let mut policy = PathPolicy::new(
            "pdftex",
            None,
            ResourceLocations {
                configuration_paths: Some(vec![]),
                ..Default::default()
            },
        )
        .unwrap();
        let element = SearchElement::new("/project/db");
        assert_eq!(
            policy
                .database_matches(&element, Path::new("a.tex"))
                .unwrap(),
            vec![PathBuf::from("/project/db/a.tex")]
        );
        host.apply(CatalogDelta {
            base_epoch: 1,
            new_epoch: 2,
            policy_version: 0,
            changes: vec![put("ls-R", b"% ls-R -- test\n./:\nb.tex\n", 2, Some(-2000))],
        })
        .unwrap();
        assert!(
            policy
                .database_matches(&element, Path::new("a.tex"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            policy
                .database_matches(&element, Path::new("b.tex"))
                .unwrap(),
            vec![PathBuf::from("/project/db/b.tex")]
        );
    }
}
