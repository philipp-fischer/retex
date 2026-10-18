//! Pure map preparation outside rollback. Resource reads, lookup and PDF use
//! remain job-local. Subfont declarations retain the ordinary resolver path.
use super::maps::{FontMapEntry, MapFileParser};
use crate::core::persistence::SharedTable;
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Weak},
};
type Fonts = SharedTable<String, FontMapEntry>;
const BUDGET: usize = 16 * 1024 * 1024;
struct Entry {
    before: Weak<BTreeMap<String, FontMapEntry>>,
    source: Arc<[u8]>,
    path: PathBuf,
    cwd: PathBuf,
    suppress: bool,
    fonts: Fonts,
    warnings: Vec<String>,
    bytes: usize,
}
#[derive(Default)]
struct Cache {
    entries: VecDeque<Entry>,
    bytes: usize,
    hits: u64,
    misses: u64,
}
thread_local! { static CACHE: RefCell<Cache> = RefCell::new(Cache::default()); }

pub(super) fn prepare(
    parser: &MapFileParser,
    path: &Path,
    source: &Arc<[u8]>,
    suppress: bool,
) -> Option<(Fonts, Vec<String>)> {
    #[cfg(feature = "latency-diagnostics")]
    let _diag =
        crate::latency_diagnostics::phase("font_map_cache", &path.display().to_string(), None);
    // insert consults cwd even with an explicit parent. Preserve the current
    // host dependency and error; a different host/cwd cannot reuse its result.
    let cwd = crate::resources::host::current_dir().ok()?;
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let hit = cache.entries.iter().position(|entry| {
            entry.before.as_ptr() as usize == parser.fonts.storage_address()
                && Arc::ptr_eq(&entry.source, source)
                && entry.path == path
                && entry.cwd == cwd
                && entry.suppress == suppress
        });
        if let Some(index) = hit {
            cache.hits += 1;
            #[cfg(feature = "latency-diagnostics")]
            crate::latency_diagnostics::count("hits", 1);
            let entry = cache.entries.remove(index).unwrap();
            let result = (entry.fonts.clone(), entry.warnings.clone());
            cache.entries.push_back(entry);
            return Some(result);
        }
        cache.misses += 1;
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::count("misses", 1);
        let text: String = source.iter().copied().map(char::from).collect();
        let mut declarations = Vec::new();
        for line in text.lines() {
            let parsed = MapFileParser::parse_map_line(line);
            if parsed
                .as_ref()
                .ok()
                .and_then(|p| p.as_ref())
                .is_some_and(|entry| super::maps::subfont_pattern().is_match(&entry.tfm_name))
            {
                return None;
            }
            declarations.push(parsed);
        }
        let mut prepared = parser.clone();
        prepared.suppress_duplicates = Rc::new(move || suppress);
        let mut warnings = Vec::new();
        let unused = HashSet::new();
        for declaration in declarations {
            match declaration {
                Ok(Some(entry)) => prepared
                    .insert(
                        entry,
                        "+",
                        &mut |w| warnings.push(w.to_owned()),
                        &unused,
                        path.parent(),
                    )
                    .ok()?,
                Ok(None) => {}
                Err(error) => warnings.push(error.to_string()),
            }
        }
        // Count complete immutable output and source storage conservatively.
        let bytes = source.len()
            + prepared
                .fonts
                .iter()
                .map(|(key, entry)| {
                    key.capacity()
                        + std::mem::size_of::<(String, FontMapEntry)>()
                        + 8 * std::mem::size_of::<usize>()
                        + entry.tfm_name.capacity()
                        + [
                            &entry.ps_name,
                            &entry.encoding,
                            &entry.fontfile,
                            &entry.options,
                            &entry.directory,
                        ]
                        .into_iter()
                        .map(|v| v.as_ref().map_or(0, |s| s.capacity()))
                        .sum::<usize>()
                        + entry
                            .subfont
                            .as_ref()
                            .map_or(0, |v| v.capacity() * std::mem::size_of::<i32>())
                })
                .sum::<usize>()
            + warnings
                .iter()
                .map(|s| s.capacity() + std::mem::size_of::<String>())
                .sum::<usize>()
            + path.as_os_str().len()
            + cwd.as_os_str().len()
            + std::mem::size_of::<Entry>();
        let result = (prepared.fonts.clone(), warnings.clone());
        if bytes <= BUDGET {
            while cache.bytes + bytes > BUDGET || cache.entries.len() >= 16 {
                let old = cache.entries.pop_front().unwrap();
                cache.bytes -= old.bytes;
            }
            cache.bytes += bytes;
            cache.entries.push_back(Entry {
                before: parser.fonts.weak_storage(),
                source: source.clone(),
                path: path.into(),
                cwd,
                suppress,
                fonts: prepared.fonts,
                warnings,
                bytes,
            });
        }
        Some(result)
    })
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn diagnostics() -> serde_json::Value {
    CACHE.with(|c| { let c = c.borrow(); serde_json::json!({"hits":c.hits,"misses":c.misses,"entries":c.entries.len(),"bytes":c.bytes,"byte_budget":BUDGET}) })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepared_maps_preserve_warnings_mutation_rollback_and_resource_identity() {
        CACHE.with(|c| *c.borrow_mut() = Cache::default());
        let source: Arc<[u8]> = b"face First <a.pfb\nface Second <b.pfb\nbad \"\n"
            .as_slice()
            .into();
        let parser = MapFileParser::default();
        let path = Path::new("/fixture/font.map");
        let (mut first, warnings) = prepare(&parser, path, &source, false).unwrap();
        assert_eq!(warnings.len(), 2);
        let (again, replay) = prepare(&parser, path, &source, false).unwrap();
        assert!(first.shares_storage_with(&again));
        assert_eq!(warnings, replay);
        first.remove("face");
        assert!(again.contains_key("face"));
        assert_eq!(prepare(&parser, path, &source, true).unwrap().1.len(), 1);
        let changed: Arc<[u8]> = b"face Replacement <c.pfb\n".as_slice().into();
        assert_eq!(
            prepare(&parser, path, &changed, false).unwrap().0["face"]
                .ps_name
                .as_deref(),
            Some("Replacement")
        );
        let mut changed_parser = parser.clone();
        changed_parser.fonts = again;
        assert_eq!(
            prepare(&changed_parser, path, &changed, false).unwrap().0["face"]
                .ps_name
                .as_deref(),
            Some("First")
        );
        assert_eq!(
            prepare(&parser, path, &source, false).unwrap().0["face"]
                .ps_name
                .as_deref(),
            Some("First")
        );
        let subfont: Arc<[u8]> = b"prefix@definition@ <font.ttf\n".as_slice().into();
        assert!(prepare(&parser, path, &subfont, false).is_none());
        for i in 0..40 {
            let bytes: Arc<[u8]> = format!("face{i} Other <font.pfb\n").into_bytes().into();
            prepare(&parser, path, &bytes, false).unwrap();
        }
        CACHE.with(|c| {
            let c = c.borrow();
            assert!(c.hits >= 2 && c.entries.len() <= 16 && c.bytes <= BUDGET);
        });
    }
}
