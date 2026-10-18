//! Catalog-backed host with bounded, revision-tagged speculative file effects.
//! Reads see the current run's writes. Publication is explicit and atomic; drop
//! or cancel discards the overlay. There is no native filesystem fallback.
use super::{
    catalog::{CatalogDelta, ResourceCatalog, ResourceId, ResourcePath},
    effects::{DocumentRevision, EffectTransaction},
    host::{FileMetadata, FixedClock, Host, Writer},
};
use crate::core::errors::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    io::{self, BufRead, Read, Write},
    path::{Component, Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::SystemTime,
};

#[derive(Clone)]
struct PendingOutputs {
    tag: DocumentRevision,
    generation: u64,
    file_scope: u64,
    epoch: u64,
    budget: usize,
    total_bytes: usize,
    names: crate::core::persistence::SharedTable<PathBuf, u64>,
    inodes: crate::core::persistence::SharedTable<u64, FileData>,
    removed: crate::core::persistence::SharedTable<PathBuf, ()>,
    next_inode: u64,
}
#[derive(Clone)]
enum FileData {
    Input(Arc<[u8]>),
    Output(Arc<Vec<u8>>),
}
impl FileData {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Input(bytes) => bytes,
            Self::Output(bytes) => bytes,
        }
    }
}
impl PendingOutputs {
    fn file(&self, path: &Path) -> Option<&FileData> {
        self.names.get(path).and_then(|id| self.inodes.get(id))
    }
    fn output(&self, path: &Path) -> Option<&Arc<Vec<u8>>> {
        match self.file(path) {
            Some(FileData::Output(bytes)) => Some(bytes),
            _ => None,
        }
    }
    fn bind(&mut self, path: &Path, bytes: Arc<[u8]>) -> io::Result<u64> {
        if let Some(&id) = self.names.get(path) {
            return Ok(id);
        }
        self.check_path_budget(path)?;
        if self.inodes.len() >= 20_000 {
            return Err(io_error("Speculative file identity budget exceeded"));
        }
        let id = self.next_inode;
        self.next_inode = id
            .checked_add(1)
            .ok_or_else(|| io_error("Speculative file identity overflow"))?;
        self.names.insert(path.to_owned(), id);
        self.inodes.insert(id, FileData::Input(bytes));
        Ok(id)
    }
    fn check_path_budget(&self, path: &Path) -> io::Result<()> {
        self.check_paths_budget(&[path])
    }
    fn check_paths_budget(&self, paths: &[&Path]) -> io::Result<()> {
        let new = paths
            .iter()
            .copied()
            .filter(|path| !self.names.contains_key(*path) && !self.removed.contains_key(*path))
            .collect::<BTreeSet<_>>()
            .len();
        if self.names.len() + self.removed.len() + new > 20_000 {
            Err(io_error("Speculative effect path budget exceeded"))
        } else {
            Ok(())
        }
    }
    fn set_output(&mut self, id: u64, bytes: Arc<Vec<u8>>) -> io::Result<()> {
        let old = match self.inodes.get(&id) {
            Some(FileData::Output(data)) => data.len(),
            _ => 0,
        };
        let total = (self.total_bytes - old)
            .checked_add(bytes.len())
            .ok_or_else(|| io_error("Output byte overflow"))?;
        if total > self.budget {
            return Err(io_error("Speculative output byte budget exceeded"));
        }
        self.inodes.insert(id, FileData::Output(bytes));
        self.total_bytes = total;
        Ok(())
    }
}
#[derive(Clone)]
pub(crate) struct OutputCheckpoint {
    pending: PendingOutputs,
    console: Arc<Vec<(bool, Vec<u8>)>>,
    console_bytes: usize,
    console_overflow: usize,
}
impl OutputCheckpoint {
    pub(crate) fn main_input_matches(&self, main: &Path, bytes: &[u8]) -> bool {
        !self.pending.removed.contains_key(main)
            && self
                .pending
                .output(main)
                .is_none_or(|output| output.as_slice() == bytes)
    }
    /// A pass seed contains named outputs/removals from its preceding pass.
    /// Equality includes absence versus empty files and console/accounting;
    /// handle generations are deliberately rebound when restoring a prefix.
    pub(crate) fn same_pass_inputs(&self, other: &Self) -> bool {
        self.pending.budget == other.pending.budget
            && self.pending.removed == other.pending.removed
            && self.pending.names.len() == other.pending.names.len()
            && self.pending.names.keys().all(|path| {
                other.pending.names.contains_key(path)
                    && self.pending.file(path).map(FileData::bytes)
                        == other.pending.file(path).map(FileData::bytes)
            })
            && self.console == other.console
            && self.console_bytes == other.console_bytes
            && self.console_overflow == other.console_overflow
    }
    pub(crate) fn input_files_only(mut self) -> Self {
        self.console = Arc::new(Vec::new());
        self.console_bytes = 0;
        self.console_overflow = 0;
        self
    }
    /// Compare actual pass input files, independently of prior console history.
    /// Only used by the opt-in working-input policy, never by legacy restoration.
    pub(crate) fn same_input_files(&self, other: &Self) -> bool {
        self.pending.budget == other.pending.budget
            && self.pending.removed == other.pending.removed
            && self.pending.names.len() == other.pending.names.len()
            && self.pending.names.keys().all(|path| {
                other.pending.names.contains_key(path)
                    && self.pending.file(path).map(FileData::bytes)
                        == other.pending.file(path).map(FileData::bytes)
            })
    }
    pub(crate) fn can_rebase_inputs(&self, old: &Self, new: &Self) -> bool {
        old.same_input_files(new)
            && old.console_overflow == 0
            && new.console_overflow == 0
            && self.console_overflow == 0
            && self.console.starts_with(old.console.as_slice())
            && self.console_bytes >= old.console_bytes
            && self.console_overflow >= old.console_overflow
    }
    /// Keep exactly the prefix's writes and open-handle identities. Replace only
    /// the console inherited at pass entry, not messages emitted by this prefix.
    pub(crate) fn rebase_inputs(&self, old: &Self, new: &Self) -> Option<Self> {
        if !self.can_rebase_inputs(old, new) {
            return None;
        }
        let mut result = self.clone();
        let mut console = new.console.as_ref().clone();
        console.extend_from_slice(&self.console[old.console.len()..]);
        result.console = Arc::new(console);
        result.console_bytes = new.console_bytes + self.console_bytes - old.console_bytes;
        result.console_overflow =
            new.console_overflow + self.console_overflow - old.console_overflow;
        Some(result)
    }
    /// No writes, renames or removals, including orphaned output handles. Such
    /// a prefix may be placed over another pass's inputs after query validation.
    pub(crate) fn read_only(&self) -> bool {
        self.pending.removed.is_empty()
            && self
                .pending
                .inodes
                .values()
                .all(|file| matches!(file, FileData::Input(_)))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum QueryKind {
    Bytes,
    PhysicalBytes,
    Classification,
    Metadata,
    Modified,
    Directory,
    Children,
    SearchDirectories,
    FileNamePresent,
    FileNameMatches,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum QueryResult {
    Bytes(Option<(ResourceId, u64, u64)>),
    Metadata(Option<(u64, bool)>),
    Modified(Option<chrono::DateTime<chrono::Utc>>),
    Directory(bool),
    Children(Option<Arc<Vec<PathBuf>>>),
}
/// Execution dependencies have their own persistent root, independent of the
/// bounded lookup cache. Overflow disables reuse instead of forgetting misses.
#[derive(Clone)]
struct ValidationProof {
    host: Arc<()>,
    epoch: u64,
    main: Option<PathBuf>,
    clock: SystemTime,
}
#[derive(Clone)]
struct MissingQueries {
    children: Option<Arc<Vec<PathBuf>>>,
    /// Bits: missing bytes (1), missing metadata including directories (2).
    names: crate::core::persistence::SharedTable<OsString, u8>,
}
fn same_children(a: &Option<Arc<Vec<PathBuf>>>, b: &Option<Arc<Vec<PathBuf>>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}
#[derive(Clone)]
pub(crate) struct ResourceCheckpoint {
    policy: u64,
    queries: crate::core::persistence::SharedTable<(PathBuf, QueryKind), QueryResult>,
    modified: Option<chrono::DateTime<chrono::Utc>>,
    bytes: usize,
    overflow: bool,
    validated: RefCell<Option<ValidationProof>>,
    missing: crate::core::persistence::SharedTable<PathBuf, MissingQueries>,
}
/// Segment-local observations, including repeated lookups already present in
/// the checkpoint's dependency set. Dropping the scope ends recording.
pub(crate) struct ResourceReadScope {
    resources: RefCell<ResourceCheckpoint>,
    console: RefCell<Vec<(bool, Vec<u8>)>>,
    unsupported: Cell<bool>,
    rejection: Cell<Option<&'static str>>,
    sequence: Rc<Cell<u64>>,
    console_sequence: RefCell<Vec<u64>>,
    input_bindings: RefCell<Vec<PathBuf>>,
    allow_writes: Cell<bool>,
    writer_owner: Arc<()>,
    writer_keys: RefCell<Vec<(u8, Rc<()>, Rc<()>)>>,
    writes: RefCell<super::reuse_writes::Journal>,
}
impl ResourceReadScope {
    pub(crate) fn allow_existing_writes(&self) {
        self.allow_writes.set(true);
    }
    pub(crate) fn register_writer(
        &self,
        key: u8,
        writer: &dyn super::host::CheckpointWriter,
    ) -> bool {
        let Some(state) = writer.reuse_state() else {
            self.reject("unknown-writer");
            return false;
        };
        if !Arc::ptr_eq(&state.owner, &self.writer_owner) || self.writer_keys.borrow().len() >= 16 {
            self.reject("foreign-writer");
            return false;
        }
        self.writer_keys
            .borrow_mut()
            .push((key, state.token, state.instance));
        true
    }
    fn record_writer(&self, mut state: super::host::ReuseWriterState, bytes: &[u8]) {
        if !self.allow_writes.get() {
            self.reject("writer-write");
            return;
        }
        if !Arc::ptr_eq(&state.owner, &self.writer_owner) {
            self.reject("foreign-writer");
            return;
        }
        let matches = self
            .writer_keys
            .borrow()
            .iter()
            .filter(|(_, token, instance)| {
                Rc::ptr_eq(token, &state.token) && Rc::ptr_eq(instance, &state.instance)
            })
            .map(|(key, _, _)| *key)
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            self.reject("unknown-writer");
            return;
        }
        state.stream = Some(matches[0]);
        let order = self.sequence.get();
        if !self.writes.borrow_mut().record(state, bytes, order) {
            self.reject("writer-budget");
            return;
        }
        self.sequence.set(order + 1);
    }
    pub(crate) fn write_journal(&self, main: &Path) -> Option<super::reuse_writes::Journal> {
        let writes = self.writes.borrow();
        if self.unsupported.get()
            || writes.bindings.iter().any(|b| {
                b.path == main
                    || self
                        .resources
                        .borrow()
                        .queries
                        .keys()
                        .any(|(path, _)| *path == b.path)
                    || self.input_bindings.borrow().contains(&b.path)
            })
        {
            return None;
        }
        Some(writes.clone())
    }

    fn reject(&self, reason: &'static str) {
        self.unsupported.set(true);
        if self.rejection.get().is_none() {
            self.rejection.set(Some(reason));
        }
    }
    pub(crate) fn rejection(&self) -> Option<&'static str> {
        self.rejection.get()
    }
    pub(crate) fn input_bindings(&self) -> Vec<PathBuf> {
        self.input_bindings.borrow().clone()
    }
    pub(crate) fn sequence(&self) -> Rc<Cell<u64>> {
        self.sequence.clone()
    }
    pub(crate) fn console_sequence(&self) -> Vec<u64> {
        self.console_sequence.borrow().clone()
    }
    pub(crate) fn finish(&self) -> Option<(ResourceCheckpoint, Vec<(bool, Vec<u8>)>)> {
        if self.unsupported.get() || self.resources.borrow().overflow {
            return None;
        }
        Some((
            self.resources.borrow().clone(),
            self.console.borrow().clone(),
        ))
    }
}
impl ResourceCheckpoint {
    pub(crate) fn retained_bytes(&self) -> usize {
        self.bytes.saturating_add(512)
    }
}
pub struct VirtualHost {
    catalog: RefCell<ResourceCatalog>,
    dependencies: Rc<RefCell<ResourceCheckpoint>>,
    reuse_scope: Rc<RefCell<std::rc::Weak<ResourceReadScope>>>,
    observed_main: RefCell<Option<PathBuf>>,
    main_queried: RefCell<bool>,
    clock_observations: RefCell<u64>,
    wall_observations: RefCell<u64>,
    directories: RefCell<BTreeMap<PathBuf, Arc<Vec<PathBuf>>>>,
    file_names: RefCell<BTreeSet<OsString>>,
    file_members: RefCell<BTreeMap<OsString, Arc<Vec<PathBuf>>>>,
    search_directories: RefCell<BTreeMap<PathBuf, Arc<Vec<PathBuf>>>>,
    // Only existing-file content replacements and empty deltas enter this
    // bounded proof history. Topology/policy changes invalidate the shortcut.
    content_changes: RefCell<std::collections::VecDeque<(u64, u64, Option<PathBuf>)>>,
    validation_scope: Arc<()>,
    pending: Rc<RefCell<Option<PendingOutputs>>>,
    generation: RefCell<u64>,
    cwd: PathBuf,
    variables: BTreeMap<String, OsString>,
    clock: RefCell<FixedClock>,
    monotonic: RefCell<Rc<dyn Fn() -> u128>>,
    measurement_clock: RefCell<Rc<dyn Fn() -> u128>>,
    // Host output is bounded as well; diagnostic records remain engine-owned.
    console: RefCell<Arc<Vec<(bool, Vec<u8>)>>>,
    console_bytes: RefCell<usize>,
    console_limit: usize,
    console_overflow_bytes: RefCell<usize>,
}
fn io_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}
fn missing(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("Catalog resource not found: {}", path.display()),
    )
}
pub fn resource_path(path: &Path) -> ResourcePath {
    ResourcePath(super::super::language::input_frames::filesystem_bytes(
        path.as_os_str(),
    ))
}
pub fn physical_path(path: &ResourcePath) -> Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(OsString::from_vec(path.0.clone()).into())
    }
    #[cfg(not(unix))]
    {
        Ok(String::from_utf8(path.0.clone())
            .map_err(|_| {
                Error::value(
                    "This host cannot represent a non-UTF-8 path; byte path transport is required",
                )
            })?
            .into())
    }
}
fn identity(path: &Path) -> u64 {
    let digest = Sha256::digest(resource_path(path).0);
    u64::from_le_bytes(digest[..8].try_into().unwrap())
}

#[cfg(test)]
mod dependency_tests {
    use super::*;
    use crate::resources::catalog::ResourceChange;
    fn host() -> VirtualHost {
        VirtualHost::new(
            ResourceCatalog::new(Default::default()),
            PathBuf::from("/project"),
            BTreeMap::new(),
            FixedClock::new(1_600_000_000, 0, 0).unwrap(),
            1000,
        )
        .unwrap()
    }
    #[test]
    fn working_inputs_rebase_only_this_pass_console_and_reject_lost_messages() {
        let host = host();
        host.begin_effects(tag(), 64 * 1024 * 1024).unwrap();
        host.console(false, b"Prior pass").unwrap();
        host.next_effect_pass(&tag()).unwrap();
        let old = host.checkpoint_effects().unwrap();
        host.console(false, b"Current prefix").unwrap();
        let prefix = host.checkpoint_effects().unwrap();
        let fresh = host.auxiliary_input_seed(tag(), &BTreeMap::new()).unwrap();
        let rebased = prefix.rebase_inputs(&old, &fresh).unwrap();
        host.restore_effects(&rebased, tag()).unwrap();
        assert_eq!(
            host.take_console(),
            vec![(false, b"Current prefix".to_vec())]
        );
        assert!(host.console(false, &vec![b'x'; 2000]).is_err());
        let truncated = host.checkpoint_effects().unwrap();
        assert!(truncated.rebase_inputs(&fresh, &fresh).is_none());
    }
    #[test]
    fn writer_receipts_rebind_checkpoint_handles_and_preserve_positions() {
        let host = host();
        host.begin_effects(tag(), 1000).unwrap();
        let path = Path::new("/project/proof.txt");
        let mut writer = host.open_output(path, false).unwrap();
        writer.write_all(b"Base").unwrap();
        let saved = writer.fork().unwrap().unwrap();
        let checkpoint = host.checkpoint_effects().unwrap();
        let scope = host.begin_resource_reads().unwrap();
        scope.allow_existing_writes();
        assert!(scope.register_writer(7, writer.as_ref()));
        writer.write_all(b"A").unwrap();
        host.console(false, b"Between").unwrap();
        writer.write_all(b"B").unwrap();
        let journal = scope
            .write_journal(Path::new("/project/document.tex"))
            .unwrap();
        assert!(scope.finish().is_some());
        assert_eq!(
            journal.events.iter().map(|r| r.0).collect::<Vec<_>>(),
            vec![0, 2]
        );
        drop(scope);
        host.restore_effects(
            &checkpoint,
            DocumentRevision {
                revision: 2,
                ..tag()
            },
        )
        .unwrap();
        assert!(writer.write_all(b"Obsolete").is_err());
        let mut writers = BTreeMap::from([(7, saved.fork().unwrap().unwrap())]);
        assert_eq!(journal.bind(&writers), Some(vec![7]));
        assert!(host.can_replay_writes(&journal));
        for (_, index, bytes) in &journal.events {
            let keys = vec![7];
            writers
                .get_mut(&keys[*index])
                .unwrap()
                .write_all(bytes)
                .unwrap();
        }
        writers.get_mut(&7).unwrap().write_all(b"C").unwrap();
        assert_eq!(host.read(path).unwrap(), b"BaseABC");
        assert!(journal.bind(&writers).is_none());
    }
    #[test]
    fn writer_receipts_reject_aliases_main_writes_and_read_write_regions() {
        let host = host();
        host.begin_effects(tag(), 1000).unwrap();
        let path = Path::new("/project/proof.txt");
        let mut writer = host.open_output(path, false).unwrap();
        let mut fork = writer.fork().unwrap().unwrap();
        let scope = host.begin_resource_reads().unwrap();
        scope.allow_existing_writes();
        assert!(scope.register_writer(1, writer.as_ref()));
        assert!(scope.register_writer(2, fork.as_ref()));
        writer.write_all(b"A").unwrap();
        fork.write_all(b"B").unwrap();
        assert!(scope.finish().is_none());
        drop(scope);
        let scope = host.begin_resource_reads().unwrap();
        scope.allow_existing_writes();
        assert!(scope.register_writer(1, writer.as_ref()));
        writer.write_all(b"C").unwrap();
        assert!(scope.write_journal(path).is_none());
        drop(scope);
        let scope = host.begin_resource_reads().unwrap();
        scope.allow_existing_writes();
        assert!(scope.register_writer(1, writer.as_ref()));
        let _ = host.read(path).unwrap();
        writer.write_all(b"D").unwrap();
        assert!(
            scope
                .write_journal(Path::new("/project/main.tex"))
                .is_none()
        );
    }
    #[test]
    fn writer_receipts_require_unique_bindings_and_preflight_output_budget() {
        let host = host();
        host.begin_effects(tag(), 1000).unwrap();
        let writer = host
            .open_output(Path::new("/project/proof.txt"), false)
            .unwrap();
        let mut state = writer.reuse_state().unwrap();
        state.stream = Some(1);
        let fork = writer.fork().unwrap().unwrap();
        let mut journal = super::super::reuse_writes::Journal::default();
        assert!(journal.record(state, &vec![65; 1001], 0));
        let writers = BTreeMap::from([(2, writer), (3, fork)]);
        assert!(journal.bind(&writers).is_none());
        assert!(!host.can_replay_writes(&journal));
        let foreign = super::dependency_tests::host();
        foreign.begin_effects(tag(), 1000).unwrap();
        assert!(!foreign.owns_writers(&writers));
    }
    #[test]
    fn segment_resource_reads_repeat_prefix_queries_and_replay_negative_observations() {
        let host = host();
        replace(&host, "/project/figure.dat", b"original");
        host.begin_effects(tag(), 1000).unwrap();
        let path = Path::new("/project/figure.dat");
        host.read(path).unwrap();
        let scope = host.begin_resource_reads().unwrap();
        host.read(path).unwrap();
        host.metadata(path).unwrap().modified().unwrap();
        assert!(host.metadata(Path::new("/project/missing.dat")).is_err());
        host.console(false, b"ordered log").unwrap();
        let (reads, console) = scope.finish().unwrap();
        assert!(!reads.queries.is_empty());
        assert!(!reads.missing.is_empty());
        assert_eq!(console, vec![(false, b"ordered log".to_vec())]);
        assert!(host.begin_resource_reads().is_none());
        drop(scope);
        host.reset_resources();
        assert!(host.replay_resource_reads(&reads));
        assert!(!host.checkpoint_resources().queries.is_empty());
        host.discard_effects();
        replace(&host, "/project/unrelated.dat", b"unrelated");
        host.begin_effects(tag(), 1000).unwrap();
        assert!(host.replay_resource_reads(&reads));
        host.discard_effects();
        replace(&host, "/project/missing.dat", b"newly present");
        host.begin_effects(tag(), 1000).unwrap();
        assert!(!host.replay_resource_reads(&reads));
    }
    #[test]
    fn segment_hits_restore_main_observations_and_input_identities() {
        let host = host();
        let path = Path::new("/project/document.tex");
        replace(&host, "/project/document.tex", b"Main input");
        host.begin_effects(tag(), 1000).unwrap();
        host.observe_main(path);
        let scope = host.begin_resource_reads().unwrap();
        let _reader = host.open_physical(path).unwrap().unwrap();
        let (reads, _) = scope.finish().unwrap();
        let bindings = scope.input_bindings();
        drop(scope);
        host.discard_effects();
        host.begin_effects(tag(), 1000).unwrap();
        host.reset_resources();
        host.observe_main(path);
        assert!(!*host.main_queried.borrow());
        assert!(host.replay_resource_reads(&reads));
        assert!(*host.main_queried.borrow());
        assert!(host.can_replay_input_bindings(&bindings));
        host.replay_input_bindings(&bindings).unwrap();
        assert!(
            host.pending
                .borrow()
                .as_ref()
                .unwrap()
                .names
                .contains_key(path)
        );
        let next = host.pending.borrow().as_ref().unwrap().next_inode;
        host.replay_input_bindings(&bindings).unwrap();
        assert_eq!(host.pending.borrow().as_ref().unwrap().next_inode, next);
    }
    #[test]
    fn segment_resource_reads_reject_mutable_outputs_and_preexisting_writer_changes() {
        let host = host();
        host.begin_effects(tag(), 1000).unwrap();
        let path = Path::new("/project/live.aux");
        let mut writer = host.open_output(path, false).unwrap();
        let scope = host.begin_resource_reads().unwrap();
        writer.write_all(b"changed").unwrap();
        assert!(scope.finish().is_none());
        drop(scope);
        let scope = host.begin_resource_reads().unwrap();
        host.read(path).unwrap();
        assert!(scope.finish().is_none());
        drop(scope);
        let scope = host.begin_resource_reads().unwrap();
        host.read_dir(Path::new("/project")).unwrap();
        assert!(scope.finish().is_none());
        drop(scope);
        let scope = host.begin_resource_reads().unwrap();
        host.observe_time();
        assert!(scope.finish().is_none());
    }
    #[test]
    fn filename_members_preserve_full_topology_and_live_private_paths() {
        let host = host();
        let main = Path::new("/project/document.tex");
        replace(&host, "/project/font.tfm", b"original");
        replace(&host, "/project/nested/marker.tex", b"parent");
        assert_eq!(
            host.file_name_candidates(Path::new("font.tfm"))
                .unwrap()
                .as_ref(),
            &[PathBuf::from("/project/font.tfm")]
        );
        let snapshot = host.checkpoint_resources();
        replace(&host, "/project/font.tfm", b"changed content");
        assert!(host.resources_valid(&snapshot, Some(main)));
        replace(&host, "/outside/search/font.tfm", b"same basename");
        assert!(!host.resources_valid(&snapshot, Some(main)));
        host.reset_resources();
        let members = host.file_name_candidates(Path::new("font.tfm")).unwrap();
        assert_eq!(members.len(), 2);
        let snapshot = host.checkpoint_resources();
        host.begin_effects(tag(), 1000).unwrap();
        host.open_output(Path::new("/project/font.tfm"), false)
            .unwrap()
            .write_all(b"replacement")
            .unwrap();
        assert!(host.resources_match_effect_inputs(&snapshot, main));
        host.open_output(Path::new("/project/nested/font.tfm"), false)
            .unwrap()
            .write_all(b"private")
            .unwrap();
        assert!(
            host.file_name_candidates(Path::new("font.tfm"))
                .unwrap()
                .contains(&PathBuf::from("/project/nested/font.tfm"))
        );
        assert!(!host.resources_match_effect_inputs(&snapshot, main));
        host.discard_effects();
        assert!(
            !host
                .file_name_candidates(Path::new("font.tfm"))
                .unwrap()
                .contains(&PathBuf::from("/project/nested/font.tfm"))
        );
    }
    #[test]
    fn global_filename_absence_tracks_new_names_and_private_outputs_conservatively() {
        let host = host();
        let main = Path::new("/project/document.tex");
        assert!(host.file_name_absent(Path::new("missing.vf")));
        let absent = host.checkpoint_resources();
        assert!(host.resources_valid(&absent, Some(main)));
        replace(&host, "/project/unrelated.tfm", b"other");
        assert!(host.resources_valid(&absent, Some(main)));
        host.begin_effects(tag(), 1000).unwrap();
        host.open_output(Path::new("/project/missing.vf"), false)
            .unwrap()
            .write_all(b"private")
            .unwrap();
        assert!(!host.file_name_absent(Path::new("missing.vf")));
        assert!(!host.resources_match_effect_inputs(&absent, main));
        host.discard_effects();
        replace(&host, "/outside/original/search/missing.vf", b"supplied");
        assert!(!host.file_name_absent(Path::new("missing.vf")));
        assert!(!host.resources_valid(&absent, Some(main)));
        assert!(!host.file_name_absent(Path::new("unrelated.tfm")));
    }
    #[test]
    fn measurement_clock_does_not_consume_tex_clock_reads() {
        let host = host();
        let reads = Rc::new(std::cell::Cell::new(0u128));
        let counter = reads.clone();
        host.set_monotonic_clock(Rc::new(move || {
            counter.set(counter.get() + 1);
            counter.get()
        }));
        host.set_measurement_clock(Rc::new(|| 1234));
        assert_eq!(host.measurement_nanos(), 1234);
        assert_eq!(host.measurement_nanos(), 1234);
        assert_eq!(reads.get(), 0);
        assert_eq!(host.clock_observations(), 0);
        assert_eq!(host.monotonic_nanos(), 1);
        assert_eq!(host.clock_observations(), 1);
        assert_eq!(host.measurement_nanos(), 1234);
        assert_eq!(host.monotonic_nanos(), 2);
    }
    #[test]
    fn cached_validation_keeps_main_scope_topology_clock_and_private_input_guards() {
        let host = host();
        let main = Path::new("/project/document.tex");
        let asset = Path::new("/project/asset.tex");
        replace(&host, main.to_str().unwrap(), b"original");
        replace(&host, asset.to_str().unwrap(), b"asset");
        host.reset_resources();
        host.read(main).unwrap();
        host.read(asset).unwrap();
        host.read_dir(Path::new("/project")).unwrap();
        let before = host.checkpoint_resources();
        assert!(host.resources_valid(&before, Some(main)));
        for index in 0..100 {
            replace(
                &host,
                main.to_str().unwrap(),
                format!("edit {index}").as_bytes(),
            );
            assert!(host.resources_valid(&before, Some(main)));
            assert!(!host.resources_valid(&before, None));
        }
        // Base-catalog validity never establishes validity over private writes.
        host.begin_effects(tag(), 1000).unwrap();
        host.open_output(asset, false)
            .unwrap()
            .write_all(b"private changed asset")
            .unwrap();
        assert!(host.resources_valid(&before, Some(main)));
        assert!(!host.resources_match_effect_inputs(&before, main));
        host.discard_effects();
        replace(&host, "/project/new.tex", b"new");
        assert!(!host.resources_valid(&before, Some(main)));
        host.reset_resources();
        let metadata = host.metadata(asset).unwrap();
        metadata.modified().unwrap();
        let dated = host.checkpoint_resources();
        assert!(host.resources_valid(&dated, Some(main)));
        host.set_clock(FixedClock::new(1_600_000_001, 0, 0).unwrap());
        assert!(!host.resources_valid(&dated, Some(main)));
    }
    #[test]
    fn validation_proofs_are_local_to_their_host_and_query_set() {
        let first = host();
        let second = host();
        replace(&first, "/project/a", b"one");
        replace(&second, "/project/a", b"longer");
        assert_eq!(first.metadata(Path::new("/project/a")).unwrap().len(), 3);
        let snapshot = first.checkpoint_resources();
        assert!(first.resources_valid(&snapshot, None));
        assert!(!second.resources_valid(&snapshot, None));
        first.read(Path::new("/project/absent")).unwrap_err();
        let additional_query = first.checkpoint_resources();
        replace(&first, "/project/absent", b"present");
        assert!(first.resources_valid(&snapshot, None));
        assert!(!first.resources_valid(&additional_query, None));
    }
    #[test]
    fn compact_missing_names_preserve_every_negative_query_without_directory_overinvalidation() {
        let host = host();
        for directory in 0..1000 {
            for name in 0..30 {
                let path = PathBuf::from(format!("/project/fonts/{directory}/font{name}.tfm"));
                assert!(host.metadata(&path).is_err());
            }
        }
        let misses = host.checkpoint_resources();
        assert!(!misses.overflow);
        assert_eq!(misses.missing.len(), 1000);
        assert!(host.resources_valid(&misses, None));
        replace(&host, "/project/fonts/50/unrelated.tfm", b"other");
        assert!(host.resources_valid(&misses, None));
        host.begin_effects(tag(), 1000).unwrap();
        host.open_output(Path::new("/project/fonts/50/font29.tfm"), false)
            .unwrap()
            .write_all(b"private")
            .unwrap();
        assert!(!host.resources_match_effect_inputs(&misses, Path::new("/project/document.tex")));
        host.discard_effects();
        replace(&host, "/project/fonts/50/font29.tfm", b"supplied");
        assert!(!host.resources_valid(&misses, None));
        // Missing bytes can coexist with a directory at the same path; missing
        // metadata cannot. Creating a file there must invalidate the byte miss.
        host.reset_resources();
        assert!(host.read(Path::new("/project/fonts/50")).is_err());
        let directory_bytes = host.checkpoint_resources();
        assert!(host.resources_valid(&directory_bytes, None));
        replace(&host, "/project/fonts/50", b"now a file too");
        assert!(!host.resources_valid(&directory_bytes, None));
    }
    #[test]
    fn recursive_directory_search_matches_independent_python_tree() {
        #[derive(serde::Deserialize)]
        struct Case {
            pattern: String,
            directories: Vec<PathBuf>,
        }
        #[derive(serde::Deserialize)]
        struct Oracle {
            files: Vec<String>,
            cases: Vec<Case>,
        }
        let oracle: Oracle = serde_json::from_str(include_str!(
            "../../tests/fixtures/search-directories-python.json"
        ))
        .unwrap();
        let host = Rc::new(host());
        for name in oracle.files {
            replace(&host, &format!("/project/{name}"), b"fixture");
        }
        let _scope = crate::resources::host::enter(host);
        assert_eq!(oracle.cases.len(), 7);
        for case in oracle.cases {
            assert_eq!(
                crate::resources::paths::SearchElement::new(case.pattern).directories(),
                case.directories
            );
        }
    }
    #[test]
    fn search_directory_dependencies_preserve_topology_and_full_listing_guards() {
        let host = host();
        replace(&host, "/project/fonts/font.tfm", b"font");
        replace(&host, "/project/document.tex", b"source");
        host.reset_resources();
        host.observe_main(Path::new("/project/document.tex"));
        assert_eq!(
            host.search_subdirectories(Path::new("/project")).unwrap(),
            vec![PathBuf::from("/project/fonts")]
        );
        assert!(!host.main_queried());
        let directories = host.checkpoint_resources();
        host.read_dir(Path::new("/project")).unwrap();
        let all_children = host.checkpoint_resources();
        replace(&host, "/project/document.tex", b"longer edited source");
        replace(&host, "/project/new.aux", b"auxiliary");
        assert!(host.resources_valid(&directories, None));
        assert!(!host.resources_valid(&all_children, None));
        host.reset_resources();
        host.read_dir(Path::new("/project")).unwrap();
        let before_private_file = host.checkpoint_resources();
        host.begin_effects(tag(), 1000).unwrap();
        host.open_output(Path::new("/project/private.toc"), false)
            .unwrap()
            .write_all(b"contents")
            .unwrap();
        assert!(
            host.resources_match_effect_inputs(&directories, Path::new("/project/document.tex"))
        );
        assert!(host.resources_valid(&before_private_file, None));
        assert!(!host.resources_match_effect_inputs(
            &before_private_file,
            Path::new("/project/document.tex")
        ));
        assert!(
            !host.resources_match_effect_inputs(&all_children, Path::new("/project/document.tex"))
        );
        host.discard_effects();
        replace(&host, "/project/newdir/file", b"file");
        assert!(!host.resources_valid(&directories, None));
        host.reset_resources();
        assert!(
            host.search_subdirectories(Path::new("/project/absent"))
                .is_err()
        );
        let absent = host.checkpoint_resources();
        replace(&host, "/project/absent/child/file", b"file");
        assert!(!host.resources_valid(&absent, None));
        host.reset_resources();
        host.search_subdirectories(Path::new("/project")).unwrap();
        let before_removal = host.checkpoint_resources();
        host.apply(CatalogDelta {
            base_epoch: host.epoch(),
            new_epoch: host.epoch() + 1,
            policy_version: host.policy_version(),
            changes: vec![ResourceChange::Remove(resource_path(Path::new(
                "/project/newdir/file",
            )))],
        })
        .unwrap();
        assert!(!host.resources_valid(&before_removal, None));
    }
    #[test]
    fn read_only_pass_reuse_checks_bytes_absence_metadata_and_children() {
        let host = host();
        let path = Path::new("/project/part.tex");
        replace(&host, path.to_str().unwrap(), b"Before");
        host.begin_effects(tag(), 10_000).unwrap();
        host.read(path).unwrap();
        let bytes = host.checkpoint_resources();
        host.reset_resources();
        host.metadata(path).unwrap();
        let metadata = host.checkpoint_resources();
        host.reset_resources();
        assert!(host.read(Path::new("/project/missing.tex")).is_err());
        let absent = host.checkpoint_resources();
        host.reset_resources();
        host.read_dir(Path::new("/project")).unwrap();
        let listing = host.checkpoint_resources();
        let checkpoint = host.checkpoint_effects().unwrap();
        assert!(checkpoint.read_only());
        host.open_output(path, false)
            .unwrap()
            .write_all(b"Edited")
            .unwrap();
        host.open_output(Path::new("/project/missing.tex"), false)
            .unwrap();
        assert!(!host.checkpoint_effects().unwrap().read_only());
        host.next_effect_pass(&tag()).unwrap();
        let main = Path::new("/project/document.tex");
        assert!(!host.resources_match_effect_inputs(&bytes, main));
        assert!(host.resources_match_effect_inputs(&metadata, main));
        assert!(!host.resources_match_effect_inputs(&absent, main));
        assert!(!host.resources_match_effect_inputs(&listing, main));
        assert!(
            host.restore_read_only_for_pass(&checkpoint, &tag())
                .unwrap()
        );
        assert_eq!(host.read(path).unwrap(), b"Edited");
        assert_eq!(host.read(Path::new("/project/missing.tex")).unwrap(), b"");
        assert_eq!(host.staged_output_paths(&tag()).unwrap().len(), 2);
        assert_eq!(host.committed(path).unwrap().unwrap().as_ref(), b"Before");
    }
    #[test]
    fn equal_pass_bytes_allow_read_handles_but_removal_invalidates_them() {
        let host = host();
        let path = Path::new("/project/part.tex");
        replace(&host, path.to_str().unwrap(), b"Same\nTail\n");
        host.begin_effects(tag(), 10_000).unwrap();
        let mut reader = host.open_physical(path).unwrap().unwrap();
        let profile = crate::core::characters::LanguageProfile::Pdftex;
        assert_eq!(reader.readline(None, profile).unwrap().unwrap(), "Same\n");
        let resources = host.checkpoint_resources();
        let checkpoint = host.checkpoint_effects().unwrap();
        host.open_output(path, false)
            .unwrap()
            .write_all(b"Same\nTail\n")
            .unwrap();
        host.next_effect_pass(&tag()).unwrap();
        assert!(reader.fork().is_none());
        assert!(host.resources_match_effect_inputs(&resources, Path::new("/project/document.tex")));
        assert!(
            host.restore_read_only_for_pass(&checkpoint, &tag())
                .unwrap()
        );
        assert!(reader.readline(None, profile).is_err());
        let mut restored_reader = reader.fork().unwrap();
        assert_eq!(
            restored_reader.readline(None, profile).unwrap().unwrap(),
            "Tail\n"
        );
        assert_eq!(host.read(path).unwrap(), b"Same\nTail\n");
        host.remove_file(path).unwrap();
        host.next_effect_pass(&tag()).unwrap();
        assert!(
            !host.resources_match_effect_inputs(&resources, Path::new("/project/document.tex"))
        );
        assert!(host.read(path).is_err());
        assert_eq!(
            host.staged_removals(&tag()).unwrap(),
            vec![resource_path(path)]
        );
    }
    #[test]
    fn classification_observes_existence_and_type_separately_from_size() {
        let host = host();
        replace(&host, "/project/a", b"data");
        let check = |path: &str| {
            host.reset_resources();
            host.observe_main(Path::new(path));
            let metadata = host.metadata(Path::new(path));
            let expected = (
                metadata.as_ref().is_ok_and(|m| m.is_file()),
                metadata.as_ref().is_ok_and(|m| m.is_dir()),
            );
            let baseline = host.checkpoint_resources();
            let baseline_main = host.main_queried();
            for (file, value) in [(true, expected.0), (false, expected.1)] {
                host.reset_resources();
                host.observe_main(Path::new(path));
                assert_eq!(
                    if file {
                        host.is_file(Path::new(path))
                    } else {
                        host.is_dir(Path::new(path))
                    },
                    value
                );
                let actual = host.checkpoint_resources();
                let mut expected = baseline.queries.clone();
                expected.clear();
                for ((path, kind), value) in baseline.queries.iter() {
                    let (kind, value) = if *kind == QueryKind::Metadata {
                        let QueryResult::Metadata(value) = value else {
                            panic!("metadata query");
                        };
                        (
                            QueryKind::Classification,
                            QueryResult::Metadata(value.map(|(_, dir)| (0, dir))),
                        )
                    } else {
                        (kind.clone(), value.clone())
                    };
                    expected.insert((path.clone(), kind), value);
                }
                assert_eq!(actual.queries, expected);
                assert_eq!(actual.missing.len(), baseline.missing.len());
                for (parent, group) in &actual.missing {
                    let original = &baseline.missing[parent];
                    assert_eq!(group.names, original.names);
                    assert!(same_children(&group.children, &original.children));
                }
                assert_eq!(actual.modified, baseline.modified);
                assert_eq!(actual.overflow, baseline.overflow);
                assert_eq!(host.main_queried(), baseline_main);
            }
        };
        for path in [
            "/project/a",
            "/project",
            "/project/missing",
            "/project/../project/a",
        ] {
            check(path);
        }
        check(&format!("/project/{}", "a".repeat(4096)));
        host.begin_effects(tag(), 100).unwrap();
        host.open_output(Path::new("/project/private"), false)
            .unwrap()
            .write_all(b"new")
            .unwrap();
        host.open_output(Path::new("/project/a"), false)
            .unwrap()
            .write_all(b"replacement")
            .unwrap();
        check("/project/private");
        check("/project/a");
        host.remove_file(Path::new("/project/a")).unwrap();
        check("/project/a");
        host.discard_effects();
        check("/project/a");
    }
    fn replace(host: &VirtualHost, path: &str, bytes: &[u8]) {
        let path = Path::new(path);
        host.apply(CatalogDelta {
            base_epoch: host.epoch(),
            new_epoch: host.epoch() + 1,
            policy_version: host.policy_version(),
            changes: vec![ResourceChange::Put {
                id: ResourceId(path.display().to_string()),
                path: resource_path(path),
                version: host.resource_version(path).unwrap_or(0) + 1,
                bytes: bytes.into(),
            }],
        })
        .unwrap();
    }
    fn tag() -> DocumentRevision {
        DocumentRevision {
            document: "test".into(),
            revision: 1,
        }
    }
    #[test]
    fn reads_misses_generation_and_policy_are_dependencies() {
        let host = host();
        replace(&host, "/project/a", b"a");
        host.reset_resources();
        host.input_bytes(Path::new("/project/a")).unwrap();
        assert!(host.read(Path::new("/project/missing")).is_err());
        let checkpoint = host.checkpoint_resources();
        replace(&host, "/project/unrelated", b"b");
        assert!(host.resources_valid(&checkpoint, None));
        replace(&host, "/project/missing", b"now present");
        assert!(!host.resources_valid(&checkpoint, None));
        host.reset_resources();
        host.read(Path::new("/project/a")).unwrap();
        let checkpoint = host.checkpoint_resources();
        host.apply(CatalogDelta {
            base_epoch: host.epoch(),
            new_epoch: host.epoch() + 1,
            policy_version: host.policy_version(),
            changes: vec![ResourceChange::Remove(resource_path(Path::new(
                "/project/a",
            )))],
        })
        .unwrap();
        replace(&host, "/project/a", b"a"); // Same ID, bytes and version; a new generation.
        assert!(!host.resources_valid(&checkpoint, None));
        host.reset_resources();
        let checkpoint = host.checkpoint_resources();
        host.apply(CatalogDelta {
            base_epoch: host.epoch(),
            new_epoch: host.epoch() + 1,
            policy_version: host.policy_version() + 1,
            changes: vec![],
        })
        .unwrap();
        assert!(!host.resources_valid(&checkpoint, None));
    }
    #[test]
    fn directory_membership_metadata_and_output_parents_are_dependencies() {
        let host = host();
        replace(&host, "/project/a", b"a");
        host.reset_resources();
        assert_eq!(host.metadata(Path::new("/project/a")).unwrap().len(), 1);
        let metadata = host.checkpoint_resources();
        host.read_dir(Path::new("/project")).unwrap();
        let listing = host.checkpoint_resources();
        assert!(host.metadata(Path::new("/project/newdir")).is_err());
        let missing_directory = host.checkpoint_resources();
        replace(&host, "/project/newdir/file", b"b");
        assert!(host.resources_valid(&metadata, None));
        assert!(!host.resources_valid(&listing, None));
        assert!(!host.resources_valid(&missing_directory, None));
        replace(&host, "/project/a", b"longer");
        assert!(!host.resources_valid(&metadata, None));
        host.reset_resources();
        host.begin_effects(tag(), 100).unwrap();
        assert!(
            host.open_output(Path::new("/project/absent/output"), false)
                .is_err()
        );
        let output_parent = host.checkpoint_resources();
        host.discard_effects();
        replace(&host, "/project/absent/other", b"x");
        assert!(!host.resources_valid(&output_parent, None));
    }
    #[test]
    fn rollback_restores_dependency_prefix_and_overflow_disables_reuse() {
        let host = host();
        replace(&host, "/project/a", b"a");
        replace(&host, "/project/b", b"b");
        host.reset_resources();
        host.read(Path::new("/project/a")).unwrap();
        let prefix = host.checkpoint_resources();
        host.read(Path::new("/project/b")).unwrap();
        let suffix = host.checkpoint_resources();
        replace(&host, "/project/b", b"changed");
        assert!(host.resources_valid(&prefix, None));
        assert!(!host.resources_valid(&suffix, None));
        host.restore_resources(&prefix);
        assert!(host.resources_valid(&host.checkpoint_resources(), None));
        host.dependencies.borrow_mut().bytes = 8 * 1024 * 1024;
        assert!(host.read(Path::new("/project/never")).is_err());
        assert!(!host.resources_valid(&host.checkpoint_resources(), None));
        assert!(host.resources_valid(&prefix, None));
    }
    #[test]
    fn append_reads_committed_bytes_but_overwritten_outputs_are_private() {
        let host = host();
        replace(&host, "/project/a", b"old");
        host.reset_resources();
        host.begin_effects(tag(), 100).unwrap();
        host.open_output(Path::new("/project/a"), true)
            .unwrap()
            .write_all(b" appended")
            .unwrap();
        let append = host.checkpoint_resources();
        host.discard_effects();
        host.reset_resources();
        host.begin_effects(tag(), 100).unwrap();
        host.open_output(Path::new("/project/a"), false)
            .unwrap()
            .write_all(b"new")
            .unwrap();
        assert_eq!(host.read(Path::new("/project/a")).unwrap(), b"new");
        host.metadata(Path::new("/project/a"))
            .unwrap()
            .modified()
            .unwrap();
        let overwritten = host.checkpoint_resources();
        host.discard_effects();
        replace(&host, "/project/a", b"different");
        assert!(!host.resources_valid(&append, None));
        assert!(host.resources_valid(&overwritten, None));
        host.set_clock(FixedClock::new(1_600_000_001, 0, 0).unwrap());
        assert!(!host.resources_valid(&overwritten, None));
    }

    #[test]
    fn renamed_replaced_and_unlinked_handles_match_independent_python() {
        use crate::core::characters::LanguageProfile;
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/file-effects-python.json"
        ))
        .unwrap();
        let host = host();
        let a = Path::new("/project/a");
        let b = Path::new("/project/b");
        let initial = [b"A\n".as_slice(), &vec![b'x'; 9200], b"\nTail A\n"].concat();
        replace(&host, "/project/a", &initial);
        replace(&host, "/project/b", b"B\nTail B\n");
        let epoch = host.epoch();
        host.begin_effects(tag(), 100_000).unwrap();
        let mut reader_a = host.open_physical(a).unwrap().unwrap();
        let mut reader_b = host.open_physical(b).unwrap().unwrap();
        let profile = LanguageProfile::Pdftex;
        assert_eq!(
            reader_a.readline(None, profile).unwrap().unwrap(),
            expected["first_a"].as_str().unwrap()
        );
        assert_eq!(
            reader_b.readline(None, profile).unwrap().unwrap(),
            expected["first_b"].as_str().unwrap()
        );
        let mut writer = host.open_output(a, true).unwrap();
        host.rename(a, b).unwrap();
        writer.write_all(b"Appended\n").unwrap();
        assert!(host.read(a).is_err());
        assert_eq!(
            format!("{:x}", Sha256::digest(host.read(b).unwrap())),
            expected["renamed_bytes_sha256"].as_str().unwrap()
        );
        assert_eq!(host.committed(a).unwrap().unwrap().as_ref(), initial);
        host.remove_file(b).unwrap();
        writer.write_all(b"Unlinked\n").unwrap();
        host.open_output(b, false)
            .unwrap()
            .write_all(b"Replacement\n")
            .unwrap();
        let mut remaining = String::new();
        while let Some(line) = reader_a.readline(None, profile).unwrap() {
            remaining.push_str(&line);
        }
        assert_eq!(
            format!("{:x}", Sha256::digest(remaining.as_bytes())),
            expected["reader_a_remaining_sha256"].as_str().unwrap()
        );
        assert_eq!(
            reader_b.readline(None, profile).unwrap().unwrap(),
            expected["reader_b_remaining"].as_str().unwrap()
        );
        assert!(reader_b.readline(None, profile).unwrap().is_none());
        assert_eq!(
            host.read_dir(Path::new("/project")).unwrap(),
            vec![b.to_owned()]
        );
        assert_eq!(
            host.staged_removals(&tag()).unwrap(),
            vec![resource_path(a)]
        );
        host.commit_effects(&tag()).unwrap();
        assert_eq!(host.epoch(), epoch + 1);
        assert!(host.committed(a).unwrap().is_none());
        assert_eq!(
            host.committed(b).unwrap().unwrap().as_ref(),
            expected["final_files"]["b"].as_str().unwrap().as_bytes()
        );
        assert!(writer.write_all(b"obsolete").is_err());
    }

    #[test]
    fn effect_checkpoint_restores_names_bytes_handles_and_deletions() {
        use crate::core::characters::LanguageProfile;
        let host = host();
        let a = Path::new("/project/a");
        let b = Path::new("/project/b");
        replace(&host, "/project/a", b"Initial\n");
        host.begin_effects(tag(), 1000).unwrap();
        let mut writer = host.open_output(a, true).unwrap();
        writer.write_all(b"Prefix\n").unwrap();
        let mut reader = host.open_physical(a).unwrap().unwrap();
        assert_eq!(
            reader
                .readline(None, LanguageProfile::Pdftex)
                .unwrap()
                .as_deref(),
            Some("Initial\n")
        );
        let reader_checkpoint = reader.fork().unwrap();
        let checkpoint = host.checkpoint_effects().unwrap();
        host.rename(a, b).unwrap();
        writer.write_all(b"Discarded\n").unwrap();
        host.remove_file(b).unwrap();
        let next = DocumentRevision {
            revision: 2,
            ..tag()
        };
        host.restore_effects(&checkpoint, next.clone()).unwrap();
        assert!(writer.write_all(b"obsolete").is_err());
        assert!(reader.readline(None, LanguageProfile::Pdftex).is_err());
        let mut resumed = writer.fork().unwrap().unwrap();
        resumed.write_all(b"Accepted\n").unwrap();
        let mut reader = reader_checkpoint.fork().unwrap();
        assert_eq!(
            reader
                .readline(None, LanguageProfile::Pdftex)
                .unwrap()
                .as_deref(),
            Some("Prefix\n")
        );
        assert_eq!(
            reader
                .readline(None, LanguageProfile::Pdftex)
                .unwrap()
                .as_deref(),
            Some("Accepted\n")
        );
        assert!(host.staged_removals(&next).unwrap().is_empty());
        assert!(host.read(b).is_err());
        host.commit_effects(&next).unwrap();
        assert_eq!(host.read(a).unwrap(), b"Initial\nPrefix\nAccepted\n");
    }

    #[test]
    fn committed_removal_is_private_until_acceptance_and_cancel_restores_it() {
        let host = host();
        let path = Path::new("/project/a");
        replace(&host, "/project/a", b"Old");
        let epoch = host.epoch();
        host.begin_effects(tag(), 0).unwrap();
        host.remove_file(path).unwrap();
        assert!(host.read(path).is_err());
        assert!(host.metadata(path).is_err());
        assert!(host.read_dir(Path::new("/project")).unwrap().is_empty());
        assert!(host.remove_file(path).is_err());
        assert!(host.rename(path, path).is_err());
        host.discard_effects();
        assert_eq!(host.read(path).unwrap(), b"Old");
        assert_eq!(host.epoch(), epoch);
        host.begin_effects(tag(), 0).unwrap();
        host.remove_file(path).unwrap();
        let wrong = DocumentRevision {
            revision: 99,
            ..tag()
        };
        assert!(host.commit_effects(&wrong).is_err());
        assert_eq!(host.epoch(), epoch);
        host.commit_effects(&tag()).unwrap();
        assert!(host.committed(path).unwrap().is_none());
    }

    #[test]
    fn old_handles_cannot_rebind_to_an_unrelated_file_transaction() {
        let host = host();
        let path = Path::new("/project/a");
        replace(&host, "/project/a", b"Initial\n");
        host.begin_effects(tag(), 100).unwrap();
        let old_writer = host.open_output(path, false).unwrap();
        let old_reader = host.open_physical(path).unwrap().unwrap();
        host.discard_effects();
        host.begin_effects(
            DocumentRevision {
                revision: 2,
                ..tag()
            },
            100,
        )
        .unwrap();
        host.open_output(path, false)
            .unwrap()
            .write_all(b"New\n")
            .unwrap();
        assert!(old_writer.fork().is_err());
        assert!(old_reader.fork().is_none());
        assert_eq!(host.read(path).unwrap(), b"New\n");
    }

    #[test]
    fn unlinking_the_last_child_preserves_its_directory() {
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/file-effects-python.json"
        ))
        .unwrap();
        assert_eq!(expected["directory_survives_file_removal"], true);
        let host = host();
        let directory = Path::new("/project/subdir");
        let child = directory.join("child");
        replace(&host, "/project/subdir/child", b"old");
        host.begin_effects(tag(), 0).unwrap();
        host.remove_file(&child).unwrap();
        host.commit_effects(&tag()).unwrap();
        assert!(host.metadata(directory).unwrap().is_dir());
        assert!(host.read_dir(directory).unwrap().is_empty());
        assert!(
            host.read_dir(Path::new("/project"))
                .unwrap()
                .contains(&directory.to_owned())
        );
        assert!(host.read(&child).is_err());
    }

    #[test]
    fn rename_preflights_both_paths_and_cannot_hide_a_source_on_budget_failure() {
        let host = host();
        let a = Path::new("/project/a");
        let b = Path::new("/project/b");
        replace(&host, "/project/a", b"Initial");
        host.begin_effects(tag(), 100).unwrap();
        for n in 0..19_999 {
            host.open_output(&PathBuf::from(format!("/project/output-{n}")), false)
                .unwrap();
        }
        assert!(host.rename(a, b).is_err());
        assert_eq!(host.read(a).unwrap(), b"Initial");
        assert!(host.read(b).is_err());
        assert!(host.staged_removals(&tag()).unwrap().is_empty());
        assert_eq!(host.staged_outputs(&tag()).unwrap().len(), 19_999);
        assert_eq!(host.pending_bytes(), 0);
    }
    #[test]
    fn stamped_mutation_capabilities_reject_without_losing_file_dates_or_bytes() {
        let host = host();
        stamped(&host, "/project/a", 1, -1_000);
        host.begin_effects(tag(), 100).unwrap();
        assert_eq!(
            host.rename(Path::new("/project/a"), Path::new("/project/b"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            host.open_output(Path::new("/project/a"), true)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(host.read(Path::new("/project/a")).unwrap(), b"same");
        assert!(host.read(Path::new("/project/b")).is_err());
        assert_eq!(
            host.metadata(Path::new("/project/a"))
                .unwrap()
                .modified()
                .unwrap(),
            SystemTime::UNIX_EPOCH - std::time::Duration::from_secs(1)
        );
        assert_eq!(host.pending_bytes(), 0);
        assert!(host.staged_removals(&tag()).unwrap().is_empty());
        assert!(
            host.rename(Path::new("/project/a"), Path::new("/project/a"))
                .is_ok()
        );
        host.open_output(Path::new("/project/a"), false)
            .unwrap()
            .write_all(b"new")
            .unwrap();
        assert_eq!(
            host.metadata(Path::new("/project/a"))
                .unwrap()
                .modified()
                .unwrap(),
            host.clock.borrow().time
        );
        assert!(
            host.rename(Path::new("/project/a"), Path::new("/project/b"))
                .is_ok()
        );
        assert_eq!(host.read(Path::new("/project/b")).unwrap(), b"new");
    }
    fn stamped(host: &VirtualHost, path: &str, version: u64, milliseconds: i64) {
        host.apply(CatalogDelta {
            base_epoch: host.epoch(),
            new_epoch: host.epoch() + 1,
            policy_version: host.policy_version(),
            changes: vec![
                super::super::catalog::ResourceChange::put(
                    ResourceId(path.into()),
                    resource_path(Path::new(path)),
                    version,
                    Arc::from(b"same".as_slice()),
                    Some(milliseconds),
                )
                .unwrap(),
            ],
        })
        .unwrap();
    }
    #[test]
    fn supplied_modification_queries_track_consumption_and_individual_paths() {
        let host = host();
        stamped(&host, "/project/a", 1, 1_600_000_000_123);
        stamped(&host, "/project/b", 1, -1_000);
        host.reset_resources();
        assert_eq!(host.metadata(Path::new("/project/a")).unwrap().len(), 4);
        let size_only = host.checkpoint_resources();
        assert_eq!(
            host.metadata(Path::new("/project/a"))
                .unwrap()
                .modified()
                .unwrap(),
            SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1_600_000_000_123)
        );
        let a_only = host.checkpoint_resources();
        assert_eq!(
            host.metadata(Path::new("/project/b"))
                .unwrap()
                .modified()
                .unwrap(),
            SystemTime::UNIX_EPOCH - std::time::Duration::from_secs(1)
        );
        let both = host.checkpoint_resources();
        assert!(
            host.resources_valid(&both, None),
            "Different file timestamps must coexist"
        );
        host.set_clock(FixedClock::new(1_600_000_010, 0, 0).unwrap());
        assert!(
            host.resources_valid(&both, None),
            "Supplied dates are independent of the fallback clock"
        );
        stamped(&host, "/project/b", 2, 1_600_000_001_456);
        assert!(host.resources_valid(&a_only, None));
        assert!(!host.resources_valid(&both, None));
        stamped(&host, "/project/a", 2, 1_600_000_002_789);
        assert!(
            host.resources_valid(&size_only, None),
            "Equal size observations do not consume timestamps"
        );
        assert!(
            !host.resources_valid(&a_only, None),
            "Equal bytes with a changed consumed date must invalidate"
        );
    }
    #[test]
    fn signed_dates_preserve_fractional_pre_epoch_metadata_without_platform_time_clamping() {
        let host = host();
        stamped(&host, "/project/a", 1, -123);
        let metadata = host.metadata(Path::new("/project/a")).unwrap();
        assert_eq!(
            metadata.modified_timestamp().unwrap().timestamp_millis(),
            -123
        );
        assert_eq!(
            metadata
                .modified()
                .unwrap()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_err()
                .duration(),
            std::time::Duration::from_millis(123)
        );
    }
    #[test]
    fn modification_queries_invalidate_transitions_between_supplied_and_clock_dates() {
        let host = host();
        replace(&host, "/project/a", b"same");
        host.reset_resources();
        host.metadata(Path::new("/project/a"))
            .unwrap()
            .modified_timestamp()
            .unwrap();
        let fallback = host.checkpoint_resources();
        host.set_clock(FixedClock::new(1_600_000_001, 0, 0).unwrap());
        assert!(!host.resources_valid(&fallback, None));
        host.set_clock(FixedClock::new(1_600_000_000, 0, 0).unwrap());
        stamped(&host, "/project/a", 2, 1_600_000_002_123);
        assert!(!host.resources_valid(&fallback, None));
        host.reset_resources();
        host.metadata(Path::new("/project/a"))
            .unwrap()
            .modified_timestamp()
            .unwrap();
        let supplied = host.checkpoint_resources();
        replace(&host, "/project/a", b"same");
        assert!(!host.resources_valid(&supplied, None));
    }
    #[test]
    fn supplied_dates_invalidate_read_only_auxiliary_prefixes_and_rejected_deltas_are_atomic() {
        let host = host();
        stamped(&host, "/project/a", 1, 1_600_000_001_123);
        host.reset_resources();
        host.metadata(Path::new("/project/a"))
            .unwrap()
            .modified()
            .unwrap();
        let prefix = host.checkpoint_resources();
        host.begin_effects(tag(), 100).unwrap();
        host.open_output(Path::new("/project/a"), false)
            .unwrap()
            .write_all(b"same")
            .unwrap();
        assert!(
            !host.resources_match_effect_inputs(&prefix, Path::new("/project/document.tex")),
            "Equal private bytes do not establish modification-date equivalence"
        );
        host.discard_effects();
        let epoch = host.epoch();
        let invalid = CatalogDelta {
            base_epoch: epoch,
            new_epoch: epoch + 1,
            policy_version: host.policy_version(),
            changes: vec![super::super::catalog::ResourceChange::PutWithMetadata {
                id: ResourceId("/project/a".into()),
                path: resource_path(Path::new("/project/a")),
                version: 2,
                bytes: Arc::from(b"bad".as_slice()),
                modified: chrono::DateTime::<chrono::Utc>::from_timestamp(253_402_300_800, 0)
                    .unwrap(),
            }],
        };
        assert!(host.apply(invalid).is_err());
        assert_eq!(host.epoch(), epoch);
        assert_eq!(host.read(Path::new("/project/a")).unwrap(), b"same");
        assert!(host.resources_valid(&prefix, None));
        assert!(
            super::super::catalog::ResourceChange::put(
                ResourceId("x".into()),
                resource_path(Path::new("/project/a")),
                2,
                Arc::from(b"bad".as_slice()),
                Some(i64::MAX)
            )
            .is_err()
        );
    }
}
impl VirtualHost {
    pub fn new(
        catalog: ResourceCatalog,
        cwd: PathBuf,
        variables: BTreeMap<String, OsString>,
        clock: FixedClock,
        console_limit: usize,
    ) -> Result<Self> {
        if !cwd.has_root() {
            return Err(Error::value(
                "Virtual host working directory must be absolute",
            ));
        }
        let policy = catalog.policy_version();
        let host = Self {
            catalog: RefCell::new(catalog),
            dependencies: Rc::new(RefCell::new(ResourceCheckpoint {
                policy,
                queries: Default::default(),
                modified: None,
                bytes: 0,
                overflow: false,
                validated: RefCell::new(None),
                missing: Default::default(),
            })),
            reuse_scope: Rc::new(RefCell::new(std::rc::Weak::new())),
            observed_main: RefCell::new(None),
            main_queried: RefCell::new(false),
            clock_observations: RefCell::new(0),
            wall_observations: RefCell::new(0),
            directories: RefCell::new(BTreeMap::new()),
            file_names: RefCell::new(BTreeSet::new()),
            file_members: RefCell::new(BTreeMap::new()),
            search_directories: RefCell::new(BTreeMap::new()),
            content_changes: RefCell::new(std::collections::VecDeque::new()),
            validation_scope: Arc::new(()),
            pending: Rc::new(RefCell::new(None)),
            generation: RefCell::new(0),
            cwd,
            variables,
            clock: RefCell::new(clock),
            monotonic: RefCell::new(Rc::new(|| 0)),
            measurement_clock: RefCell::new(Rc::new(|| {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    super::host::NativeHost.monotonic_nanos()
                }
                #[cfg(target_arch = "wasm32")]
                {
                    0
                }
            })),
            console: RefCell::new(Arc::new(Vec::new())),
            console_bytes: RefCell::new(0),
            console_limit,
            console_overflow_bytes: RefCell::new(0),
        };
        host.rebuild_directories()?;
        Ok(host)
    }
    pub(crate) fn normalize_resource_path(&self, path: &ResourcePath) -> Result<ResourcePath> {
        Ok(resource_path(
            &self.normalize(&physical_path(path)?).map_err(Error::from)?,
        ))
    }
    // Stamped catalog inputs have physical metadata independent of the fixed
    // job clock. Until output inodes own timestamps, these mutations must not
    // silently replace that metadata. Truncation correctly creates new data.
    fn require_unstamped_mutation(&self, path: &Path, operation: &str) -> io::Result<()> {
        let pending = self.pending.borrow();
        if pending
            .as_ref()
            .is_some_and(|p| p.output(path).is_none() && !p.removed.contains_key(path))
            && self
                .catalog
                .borrow()
                .get(&resource_path(path))
                .is_some_and(|r| r.modified.is_some())
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "{operation} of a resource with supplied modification metadata requires transactional inode timestamp support"
                ),
            ));
        }
        Ok(())
    }
    /// Preserve the ordinary metadata dependency and namespace checks while
    /// avoiding unused timestamps, inode hashes and modification callbacks.
    fn classification(&self, path: &Path) -> io::Result<bool> {
        let path = self.normalize(path)?;
        self.observe_query(&path);
        let pending = self.pending.borrow();
        if pending
            .as_ref()
            .is_some_and(|p| p.removed.contains_key(&path))
        {
            return Err(missing(&path));
        }
        let output = pending.as_ref().is_some_and(|p| p.output(&path).is_some());
        self.observe_resource(
            &path,
            if output {
                QueryKind::Directory
            } else {
                QueryKind::Classification
            },
        );
        let resource = self
            .catalog
            .borrow()
            .metadata_resource(&resource_path(&path))
            .map_err(io_error)?;
        let directory = self.directories.borrow().contains_key(&path);
        if !output && resource.is_none() && !directory {
            return Err(missing(&path));
        }
        Ok(directory)
    }
    fn normalize(&self, path: &Path) -> io::Result<PathBuf> {
        let full = if path.has_root() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        };
        let mut result = PathBuf::new();
        for component in full.components() {
            match component {
                Component::ParentDir => {
                    result.pop();
                }
                Component::CurDir => {}
                other => result.push(other.as_os_str()),
            }
        }
        if super::super::language::input_frames::filesystem_bytes(result.as_os_str()).contains(&0) {
            return Err(io_error("NUL in host resource path"));
        }
        Ok(result)
    }
    fn rebuild_directories(&self) -> Result<()> {
        self.rebuild_directories_preserving(&[])
    }
    fn rebuild_directories_preserving(&self, existing: &[PathBuf]) -> Result<()> {
        let mut entries: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
        // Empty configured working/output directories exist even before files.
        for base in std::iter::once(self.cwd.clone())
            .chain(
                self.variables
                    .get("TEXMF_OUTPUT_DIRECTORY")
                    .map(PathBuf::from),
            )
            .chain(existing.iter().cloned())
        {
            entries.entry(base.clone()).or_default();
            let mut path = base;
            while let Some(parent) = path.parent() {
                entries
                    .entry(parent.into())
                    .or_default()
                    .insert(path.clone());
                path = parent.into();
            }
        }
        let mut file_names = BTreeSet::new();
        let mut file_members: BTreeMap<OsString, Vec<PathBuf>> = BTreeMap::new();
        for resource in self.catalog.borrow().paths() {
            let mut path = physical_path(resource)?;
            if let Some(name) = path.file_name() {
                file_names.insert(name.to_os_string());
                file_members
                    .entry(name.to_os_string())
                    .or_default()
                    .push(path.clone());
            }
            if !path.has_root() || self.normalize(&path).map_err(io_error)? != path {
                return Err(Error::value(
                    "Virtual catalog paths must be absolute and normalized",
                ));
            }
            while let Some(parent) = path.parent() {
                entries
                    .entry(parent.into())
                    .or_default()
                    .insert(path.clone());
                path = parent.into();
            }
        }
        let directories: BTreeMap<PathBuf, Arc<Vec<PathBuf>>> = entries
            .into_iter()
            .map(|(p, children)| (p, Arc::new(children.into_iter().collect())))
            .collect();
        *self.search_directories.borrow_mut() = directories
            .iter()
            .map(|(path, children)| {
                (
                    path.clone(),
                    Arc::new(
                        children
                            .iter()
                            .filter(|child| {
                                directories.contains_key(*child)
                                    && !child
                                        .file_name()
                                        .is_some_and(|name| name.to_string_lossy().starts_with('.'))
                            })
                            .cloned()
                            .collect(),
                    ),
                )
            })
            .collect();
        *self.directories.borrow_mut() = directories;
        *self.file_names.borrow_mut() = file_names;
        *self.file_members.borrow_mut() = file_members
            .into_iter()
            .map(|(name, paths)| (name, Arc::new(paths)))
            .collect();
        Ok(())
    }
    pub(crate) fn observe_main(&self, path: &Path) {
        *self.observed_main.borrow_mut() = Some(path.into());
        *self.main_queried.borrow_mut() = false;
    }
    pub(crate) fn main_queried(&self) -> bool {
        *self.main_queried.borrow()
    }
    pub(crate) fn wall_observations(&self) -> u64 {
        *self.wall_observations.borrow()
    }
    pub(crate) fn wall_identity(&self) -> (SystemTime, i32) {
        let clock = self.clock.borrow();
        (clock.time, clock.offset.local_minus_utc())
    }
    pub(crate) fn clock_observations(&self) -> u64 {
        *self.clock_observations.borrow()
    }
    fn observe_query(&self, path: &Path) {
        if self
            .pending
            .borrow()
            .as_ref()
            .is_some_and(|p| p.output(path).is_some() || p.removed.contains_key(path))
        {
            self.reject_resource_reuse("private-query");
        }
        if self.observed_main.borrow().as_deref() == Some(path) {
            *self.main_queried.borrow_mut() = true;
        }
    }
    fn query_result(&self, path: &Path, kind: &QueryKind) -> QueryResult {
        match kind {
            QueryKind::Bytes | QueryKind::PhysicalBytes => QueryResult::Bytes(
                self.catalog
                    .borrow()
                    .get(&resource_path(path))
                    .map(|r| (r.id.clone(), r.version, r.generation)),
            ),
            QueryKind::Metadata => {
                let resource = self.catalog.borrow().get(&resource_path(path));
                let directory = self.directories.borrow().contains_key(path);
                QueryResult::Metadata((resource.is_some() || directory).then(|| {
                    (
                        resource.as_ref().map_or(0, |r| r.bytes.len()) as u64,
                        directory,
                    )
                }))
            }
            QueryKind::Modified => {
                QueryResult::Modified(self.catalog.borrow().get(&resource_path(path)).map(
                    |resource| {
                        resource
                            .modified
                            .unwrap_or_else(|| self.clock.borrow().time.into())
                    },
                ))
            }
            QueryKind::Classification => {
                let resource = self.catalog.borrow().get(&resource_path(path));
                let directory = self.directories.borrow().contains_key(path);
                QueryResult::Metadata((resource.is_some() || directory).then_some((0, directory)))
            }
            QueryKind::Directory => {
                QueryResult::Directory(self.directories.borrow().contains_key(path))
            }
            QueryKind::Children => {
                QueryResult::Children(self.directories.borrow().get(path).cloned())
            }
            QueryKind::SearchDirectories => {
                QueryResult::Children(self.search_directories.borrow().get(path).cloned())
            }
            QueryKind::FileNamePresent => {
                QueryResult::Directory(self.file_names.borrow().contains(path.as_os_str()))
            }
            QueryKind::FileNameMatches => {
                QueryResult::Children(self.file_members.borrow().get(path.as_os_str()).cloned())
            }
        }
    }
    fn observe_resource(&self, path: &Path, kind: QueryKind) {
        let value = self.query_result(path, &kind);
        self.record_resource(&self.dependencies, path, kind.clone(), value.clone());
        if let Some(scope) = self.reuse_scope.borrow().upgrade() {
            self.record_resource(&scope.resources, path, kind, value);
        }
    }
    fn record_resource(
        &self,
        target: &RefCell<ResourceCheckpoint>,
        path: &Path,
        kind: QueryKind,
        value: QueryResult,
    ) {
        let mut observations = target.borrow_mut();
        if observations.overflow {
            return;
        }
        let absent = match (&kind, &value) {
            (QueryKind::Bytes, QueryResult::Bytes(None)) => 1,
            (QueryKind::Metadata | QueryKind::Classification, QueryResult::Metadata(None)) => 2,
            _ => 0,
        };
        if absent != 0 {
            if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
                let present = observations
                    .missing
                    .get(parent)
                    .and_then(|group| group.names.get(name))
                    .copied()
                    .unwrap_or(0);
                if present & absent != 0 {
                    return;
                }
                let new_parent = !observations.missing.contains_key(parent);
                let cost = if new_parent {
                    resource_path(parent).0.len() + 64
                } else {
                    0
                } + if present == 0 {
                    crate::language::input_frames::filesystem_bytes(name).len() + 64
                } else {
                    8
                };
                if observations.queries.len() + observations.missing.len() >= 20_000
                    || observations.bytes.saturating_add(cost) > 8 * 1024 * 1024
                {
                    observations.overflow = true;
                    return;
                }
                let children = self.directories.borrow().get(parent).cloned();
                if let Some(old) = observations.missing.get(parent) {
                    if !same_children(&old.children, &children)
                        && !self.missing_valid(parent, old, None)
                    {
                        observations.overflow = true;
                        return;
                    }
                }
                observations.bytes += cost;
                *observations.validated.borrow_mut() = None;
                let group = observations
                    .missing
                    .entry(parent.into())
                    .or_insert_with(|| MissingQueries {
                        children: children.clone(),
                        names: Default::default(),
                    });
                group.children = children;
                group.names.insert(name.into(), present | absent);
                return;
            }
        }
        let key = (path.to_path_buf(), kind);
        if observations.queries.get(&key) == Some(&value) {
            return;
        }
        // Accounting is deliberately conservative on replacements. No cached
        // resource bytes are retained by observations.
        let cost = resource_path(path).0.len()
            + match &value {
                QueryResult::Children(Some(children)) => {
                    children.iter().map(|p| resource_path(p).0.len() + 64).sum()
                }
                QueryResult::Bytes(Some((id, _, _))) => id.0.len() + 64,
                _ => 64,
            };
        if observations.queries.len() + observations.missing.len() >= 20_000
            || observations.bytes.saturating_add(cost) > 8 * 1024 * 1024
        {
            observations.overflow = true;
            return;
        }
        observations.bytes += cost;
        *observations.validated.borrow_mut() = None;
        observations.queries.insert(key, value);
    }
    pub(crate) fn owns_writers(&self, writers: &BTreeMap<u8, super::host::Writer>) -> bool {
        writers.values().all(|w| {
            w.reuse_state()
                .is_some_and(|s| Arc::ptr_eq(&s.owner, &self.validation_scope))
        })
    }
    pub(crate) fn can_replay_writes(&self, writes: &super::reuse_writes::Journal) -> bool {
        if writes
            .bindings
            .iter()
            .any(|b| !Arc::ptr_eq(&b.owner, &self.validation_scope))
        {
            return false;
        }
        if writes.events.is_empty() {
            return true;
        }
        let pending = self.pending.borrow();
        pending.as_ref().is_some_and(|p| {
            writes
                .growth()
                .and_then(|n| p.total_bytes.checked_add(n))
                .is_some_and(|n| n <= p.budget)
        })
    }
    pub(crate) fn begin_resource_reads(&self) -> Option<Rc<ResourceReadScope>> {
        if self.reuse_scope.borrow().upgrade().is_some() {
            return None;
        }
        let scope = Rc::new(ResourceReadScope {
            resources: RefCell::new(ResourceCheckpoint {
                policy: self.policy_version(),
                queries: Default::default(),
                modified: None,
                bytes: 0,
                overflow: false,
                validated: RefCell::new(None),
                missing: Default::default(),
            }),
            console: Default::default(),
            unsupported: Cell::new(false),
            rejection: Cell::new(None),
            sequence: Rc::new(Cell::new(0)),
            console_sequence: Default::default(),
            input_bindings: Default::default(),
            allow_writes: Cell::new(false),
            writer_owner: self.validation_scope.clone(),
            writer_keys: Default::default(),
            writes: Default::default(),
        });
        *self.reuse_scope.borrow_mut() = Rc::downgrade(&scope);
        Some(scope)
    }
    fn reject_resource_reuse(&self, reason: &'static str) {
        if let Some(scope) = self.reuse_scope.borrow().upgrade() {
            scope.reject(reason);
        }
    }
    pub(crate) fn can_replay_console(&self, records: &[(bool, Vec<u8>)]) -> bool {
        self.console
            .borrow()
            .len()
            .checked_add(records.len())
            .is_some_and(|n| n <= 4096)
            && self
                .console_bytes
                .borrow()
                .checked_add(records.iter().map(|r| r.1.len()).sum::<usize>())
                .is_some_and(|n| n <= self.console_limit)
    }
    pub(crate) fn can_replay_input_bindings(&self, paths: &[PathBuf]) -> bool {
        let pending = self.pending.borrow();
        let Some(pending) = pending.as_ref() else {
            return paths.is_empty();
        };
        let new = paths
            .iter()
            .filter(|p| !pending.names.contains_key(*p))
            .collect::<BTreeSet<_>>();
        pending
            .check_paths_budget(&paths.iter().map(PathBuf::as_path).collect::<Vec<_>>())
            .is_ok()
            && pending.inodes.len() + new.len() <= 20_000
            && pending.next_inode.checked_add(new.len() as u64).is_some()
            && paths.iter().all(|p| {
                pending.names.contains_key(p)
                    || self.catalog.borrow().get(&resource_path(p)).is_some()
            })
    }
    pub(crate) fn replay_input_bindings(&self, paths: &[PathBuf]) -> io::Result<()> {
        let mut pending = self.pending.borrow_mut();
        if let Some(pending) = pending.as_mut() {
            for path in paths {
                if !pending.names.contains_key(path) {
                    let bytes = self
                        .catalog
                        .borrow()
                        .get(&resource_path(path))
                        .ok_or_else(|| missing(path))?
                        .bytes
                        .clone();
                    pending.bind(path, bytes)?;
                }
            }
        }
        Ok(())
    }
    pub(crate) fn replay_resource_reads(&self, snapshot: &ResourceCheckpoint) -> bool {
        if !self.resources_match_private(snapshot, None) {
            return false;
        }
        for ((path, kind), _) in snapshot.queries.iter() {
            self.observe_query(path);
            self.observe_resource(path, kind.clone());
        }
        for (parent, group) in snapshot.missing.iter() {
            for (name, flags) in group.names.iter() {
                let path = parent.join(name);
                self.observe_query(&path);
                if flags & 1 != 0 {
                    self.observe_resource(&path, QueryKind::Bytes);
                }
                if flags & 2 != 0 {
                    self.observe_resource(&path, QueryKind::Metadata);
                }
            }
        }
        if let Some(time) = snapshot.modified {
            self.dependencies.borrow_mut().modified = Some(time);
        }
        true
    }
    pub(crate) fn reset_resources(&self) {
        *self.dependencies.borrow_mut() = ResourceCheckpoint {
            policy: self.policy_version(),
            queries: Default::default(),
            modified: None,
            bytes: 0,
            overflow: false,
            validated: RefCell::new(None),
            missing: Default::default(),
        };
        *self.observed_main.borrow_mut() = None;
    }
    pub(crate) fn checkpoint_resources(&self) -> ResourceCheckpoint {
        self.dependencies.borrow().clone()
    }
    pub(crate) fn checkpoint_auxiliary_resources(
        &self,
        inputs: &BTreeMap<PathBuf, Option<Arc<[u8]>>>,
    ) -> ResourceCheckpoint {
        let result = RefCell::new(self.checkpoint_resources());
        // Private reads need not consult the catalog. The working-input policy
        // nevertheless observes explicit replacements/removals of declared files.
        for path in inputs.keys() {
            self.record_resource(
                &result,
                path,
                QueryKind::Bytes,
                self.query_result(path, &QueryKind::Bytes),
            );
        }
        result.into_inner()
    }
    pub(crate) fn restore_resources(&self, snapshot: &ResourceCheckpoint) {
        *self.dependencies.borrow_mut() = snapshot.clone();
    }
    /// Called only after whole-checkpoint validation proved these open physical
    /// readers' immutable prefixes and safe rebinding. Their continuations now
    /// read the new catalog versions. Keep that physical provenance current so
    /// later checkpoints (after a reader closes) validate the execution actually
    /// performed. Explicit bytes, size, date and all other queries stay intact.
    pub(crate) fn restore_source_resources(
        &self,
        snapshot: &ResourceCheckpoint,
        sources: &[PathBuf],
    ) {
        let mut rebound = snapshot.clone();
        for path in sources {
            let key = (path.clone(), QueryKind::PhysicalBytes);
            if rebound.queries.contains_key(&key) {
                rebound
                    .queries
                    .insert(key, self.query_result(path, &QueryKind::PhysicalBytes));
            }
        }
        *rebound.validated.borrow_mut() = None;
        *self.dependencies.borrow_mut() = rebound;
    }
    /// Stronger than the prefix query guard: every catalog input available to
    /// the skipped continuation must be unchanged, including negative queries.
    pub(crate) fn only_main_changes_since(&self, mut epoch: u64, policy: u64, main: &Path) -> bool {
        if policy != self.policy_version() {
            return false;
        }
        for (before, after, path) in self.content_changes.borrow().iter() {
            if *after <= epoch {
                continue;
            }
            if *before != epoch || path.as_deref().is_some_and(|p| p != main) {
                return false;
            }
            epoch = *after;
        }
        epoch == self.epoch()
    }
    pub(crate) fn resources_valid(
        &self,
        snapshot: &ResourceCheckpoint,
        main: Option<&Path>,
    ) -> bool {
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase("checkpoint_validation", "catalog", None);
        let eligible = !snapshot.overflow
            && snapshot.policy == self.policy_version()
            && snapshot.modified.is_none_or(|time| {
                time == chrono::DateTime::<chrono::Utc>::from(self.clock.borrow().time)
            });
        if !eligible {
            return false;
        }
        let epoch = self.epoch();
        let clock = self.clock.borrow().time;
        let proven = snapshot.validated.borrow().as_ref().is_some_and(|proof| {
            if Arc::ptr_eq(&proof.host, &self.validation_scope)
                && proof.main.as_deref() == main
                && proof.clock == clock
            {
                let mut proven = proof.epoch;
                for (before, after, changed) in self.content_changes.borrow().iter() {
                    if *after <= proven {
                        continue;
                    }
                    if *before != proven
                        || changed.as_deref().is_some_and(|path| Some(path) != main)
                    {
                        break;
                    }
                    proven = *after;
                }
                return proven == epoch;
            }
            false
        });
        if proven {
            snapshot.validated.borrow_mut().as_mut().unwrap().epoch = epoch;
            return true;
        }
        let valid = snapshot
            .missing
            .iter()
            .all(|(parent, group)| self.missing_valid(parent, group, main))
            && snapshot.queries.iter().all(|((path, kind), value)| {
                // Physical editor input is validated by the checkpoint's exact
                // buffered prefix. Other queries of it prohibit capture.
                main == Some(path.as_path()) || self.query_result(path, kind) == *value
            });
        if valid {
            *snapshot.validated.borrow_mut() = Some(ValidationProof {
                host: self.validation_scope.clone(),
                epoch,
                main: main.map(Path::to_path_buf),
                clock,
            });
        }
        valid
    }
    fn missing_valid(&self, parent: &Path, group: &MissingQueries, main: Option<&Path>) -> bool {
        let current = self.directories.borrow().get(parent).cloned();
        if same_children(&group.children, &current) {
            return true;
        }
        group.names.iter().all(|(name, kinds)| {
            let path = parent.join(name);
            main == Some(path.as_path())
                || (self.catalog.borrow().get(&resource_path(&path)).is_none()
                    && (*kinds & 2 == 0 || !self.directories.borrow().contains_key(&path)))
        })
    }
    /// Validate a catalog-only prefix against the next pass's private inputs.
    /// Bytes, absence, metadata and directory enumeration are independently
    /// observable; a prefix that already read changed auxiliary data must replay.
    /// Prefix exemptions apply only to physical opens proven by the caller.
    /// Full reads, file metadata, misses, directories and search policy remain exact.
    pub(crate) fn resources_valid_sources(
        &self,
        snapshot: &ResourceCheckpoint,
        sources: &[PathBuf],
    ) -> bool {
        if snapshot.overflow
            || snapshot.policy != self.policy_version()
            || snapshot.modified.is_some_and(|t| {
                t != chrono::DateTime::<chrono::Utc>::from(self.clock.borrow().time)
            })
        {
            return false;
        }
        snapshot
            .missing
            .iter()
            .all(|(parent, group)| self.missing_valid(parent, group, None))
            && snapshot.queries.iter().all(|((path, kind), value)| {
                (*kind == QueryKind::PhysicalBytes && sources.contains(path))
                    || self.query_result(path, kind) == *value
            })
    }
    pub(crate) fn auxiliary_resources_valid(
        &self,
        snapshot: &ResourceCheckpoint,
        main: &Path,
    ) -> bool {
        let sources: Vec<_> = snapshot
            .queries
            .keys()
            .filter(|(_, kind)| *kind == QueryKind::PhysicalBytes)
            .map(|(path, _)| path.clone())
            .collect();
        // Root explicit-query behavior is unchanged: source metadata prohibits
        // checkpoint capture, and auxiliary provenance still validates it here.
        let _ = main;
        self.resources_valid_sources(snapshot, &sources)
    }
    pub(crate) fn source_resource_rejections(
        &self,
        snapshot: &ResourceCheckpoint,
        sources: &[PathBuf],
    ) -> Vec<String> {
        snapshot
            .queries
            .iter()
            .filter(|((path, kind), value)| {
                !(*kind == QueryKind::PhysicalBytes && sources.contains(path))
                    && self.query_result(path, kind) != **value
            })
            .take(8)
            .map(|((path, kind), _)| format!("{}:{kind:?}", path.display()))
            .collect()
    }
    pub(crate) fn source_modified(&self, path: &Path) -> Option<chrono::DateTime<chrono::Utc>> {
        self.catalog
            .borrow()
            .get(&resource_path(path))
            .and_then(|r| r.modified)
    }
    pub(crate) fn source_bytes(&self, path: &Path) -> Option<Arc<[u8]>> {
        self.catalog
            .borrow()
            .get(&resource_path(path))
            .map(|r| r.bytes.clone())
    }
    pub(crate) fn resources_match_effect_inputs(
        &self,
        snapshot: &ResourceCheckpoint,
        main: &Path,
    ) -> bool {
        self.resources_match_private(snapshot, Some(main))
    }
    fn resources_match_private(&self, snapshot: &ResourceCheckpoint, main: Option<&Path>) -> bool {
        if !self.resources_valid(snapshot, main) {
            return false;
        }
        let pending = self.pending.borrow();
        let Some(pending) = pending.as_ref() else {
            return false;
        };
        if snapshot.missing.iter().any(|(parent, group)| {
            group.names.keys().any(|name| {
                let path = parent.join(name);
                Some(path.as_path()) != main
                    && !pending.removed.contains_key(&path)
                    && pending.output(&path).is_some()
            })
        }) {
            return false;
        }
        snapshot.queries.iter().all(|((path, kind), expected)| {
            if Some(path.as_path()) == main {
                return true;
            }
            match kind {
                QueryKind::Bytes | QueryKind::PhysicalBytes => {
                    let original = self.catalog.borrow().get(&resource_path(path));
                    if pending.removed.contains_key(path) {
                        original.is_none()
                    } else if let Some(bytes) = pending.output(path) {
                        original.is_some_and(|resource| resource.bytes.as_ref() == bytes.as_slice())
                    } else {
                        true
                    }
                }
                QueryKind::Metadata => {
                    if pending.removed.contains_key(path) {
                        expected == &QueryResult::Metadata(None)
                    } else if let Some(bytes) = pending.output(path) {
                        expected == &QueryResult::Metadata(Some((bytes.len() as u64, false)))
                    } else {
                        true
                    }
                }
                QueryKind::Modified => {
                    if pending.removed.contains_key(path) {
                        expected == &QueryResult::Modified(None)
                    } else if pending.output(path).is_some() {
                        expected == &QueryResult::Modified(Some(self.clock.borrow().time.into()))
                    } else {
                        true
                    }
                }
                QueryKind::Classification => {
                    if pending.removed.contains_key(path) {
                        expected == &QueryResult::Metadata(None)
                    } else if pending.output(path).is_some() {
                        expected == &QueryResult::Metadata(Some((0, false)))
                    } else {
                        true
                    }
                }
                QueryKind::Directory => true,
                QueryKind::FileNameMatches => {
                    let QueryResult::Children(members) = expected else {
                        return false;
                    };
                    !pending.names.keys().any(|file| {
                        file.file_name() == Some(path.as_os_str())
                            && !pending.removed.contains_key(file)
                            && pending.file(file).is_some()
                            && !members
                                .as_ref()
                                .is_some_and(|members| members.contains(file))
                    })
                }
                QueryKind::FileNamePresent => !pending.names.keys().any(|file| {
                    file.file_name() == Some(path.as_os_str())
                        && !pending.removed.contains_key(file)
                        && pending.output(file).is_some()
                }),
                // Private effects create/remove/rename files only. Their
                // parents must already exist; directory topology cannot change.
                QueryKind::SearchDirectories => true,
                QueryKind::Children => {
                    let QueryResult::Children(original) = expected else {
                        return false;
                    };
                    let Some(original) = original else {
                        return true;
                    };
                    let mut children = original.as_ref().clone();
                    children.retain(|child| !pending.removed.contains_key(child));
                    for file in pending.names.keys().filter(|file| {
                        file.parent() == Some(path.as_path()) && pending.output(file).is_some()
                    }) {
                        if !children.contains(file) {
                            children.push(file.clone());
                        }
                    }
                    children.sort();
                    children == **original
                }
            }
        })
    }
    pub fn policy_version(&self) -> u64 {
        self.catalog.borrow().policy_version()
    }
    pub fn epoch(&self) -> u64 {
        self.catalog.borrow().epoch()
    }
    /// Project replacement must retain the catalog identity and advance the
    /// content version, including files originally supplied by a bundle.
    pub fn resource_identity(&self, path: &ResourcePath) -> Result<Option<(ResourceId, u64)>> {
        let path = self.normalize_resource_path(path)?;
        Ok(self
            .catalog
            .borrow()
            .get(&path)
            .map(|r| (r.id.clone(), r.version)))
    }
    /// Fixed test hosts freeze elapsed time by default. Browser/native hosts
    /// must supply their real monotonic clock before constructing an engine.
    pub fn set_clock(&self, clock: FixedClock) {
        *self.clock.borrow_mut() = clock;
    }
    pub(crate) fn clock_bucket(&self) -> (u64, i32) {
        let clock = self.clock.borrow();
        (
            clock
                .time
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                / 60,
            clock.offset.local_minus_utc(),
        )
    }
    pub fn set_monotonic_clock(&self, clock: Rc<dyn Fn() -> u128>) {
        *self.monotonic.borrow_mut() = clock;
    }
    #[cfg(any(test, target_arch = "wasm32"))]
    pub(crate) fn set_measurement_clock(&self, clock: Rc<dyn Fn() -> u128>) {
        *self.measurement_clock.borrow_mut() = clock;
    }
    /// Instrumentation must not create a TeX clock dependency.
    pub(crate) fn measurement_nanos(&self) -> u128 {
        (self.measurement_clock.borrow())()
    }
    pub(crate) fn validate_document_path(&self, path: &Path) -> Result<()> {
        if !path.has_root() || self.normalize(path).map_err(io_error)? != path {
            return Err(Error::value(
                "Document path must be absolute and normalized",
            ));
        }
        Ok(())
    }
    pub(crate) fn resource_id(&self, path: &Path) -> Option<super::catalog::ResourceId> {
        self.catalog
            .borrow()
            .get(&resource_path(path))
            .map(|resource| resource.id.clone())
    }
    pub(crate) fn resource_version(&self, path: &Path) -> Option<u64> {
        self.catalog
            .borrow()
            .get(&resource_path(path))
            .map(|resource| resource.version)
    }
    pub(crate) fn validate_delta(&self, delta: &CatalogDelta) -> Result<()> {
        for change in &delta.changes {
            let path = match change {
                super::catalog::ResourceChange::Put { path, .. }
                | super::catalog::ResourceChange::PutWithMetadata { path, .. }
                | super::catalog::ResourceChange::Remove(path) => path,
            };
            self.validate_document_path(&physical_path(path)?)?;
        }
        self.catalog.borrow().validate(delta).map(|_| ())
    }
    pub fn apply(&self, delta: CatalogDelta) -> Result<()> {
        if self.pending.borrow().is_some() {
            return Err(Error::value(
                "Cancel speculative execution before changing its resources",
            ));
        }
        // Validate path transport/topology before the catalog mutation.
        let mut topology = false;
        for change in &delta.changes {
            let path = match change {
                super::catalog::ResourceChange::Put { path, .. }
                | super::catalog::ResourceChange::PutWithMetadata { path, .. } => {
                    topology |= self.catalog.borrow().get(path).is_none();
                    path
                }
                super::catalog::ResourceChange::Remove(path) => {
                    topology = true;
                    path
                }
            };
            let physical = physical_path(path)?;
            if !physical.has_root() || self.normalize(&physical).map_err(io_error)? != physical {
                return Err(Error::value(
                    "Virtual catalog paths must be absolute and normalized",
                ));
            }
        }
        let before = self.epoch();
        let after = delta.new_epoch;
        let simple_content =
            !topology && delta.policy_version == self.policy_version() && delta.changes.len() <= 1;
        let changed = delta
            .changes
            .first()
            .map(|change| match change {
                super::catalog::ResourceChange::Put { path, .. }
                | super::catalog::ResourceChange::PutWithMetadata { path, .. }
                | super::catalog::ResourceChange::Remove(path) => physical_path(path),
            })
            .transpose()?;
        self.catalog.borrow_mut().apply(delta)?;
        if topology {
            self.rebuild_directories()?;
        }
        let mut history = self.content_changes.borrow_mut();
        if simple_content {
            if history.len() == 64 {
                history.pop_front();
            }
            history.push_back((before, after, changed));
        } else {
            history.clear();
        }
        Ok(())
    }
    pub fn begin_effects(&self, tag: DocumentRevision, byte_budget: usize) -> Result<()> {
        if self.pending.borrow().is_some() {
            return Err(Error::value(
                "Speculative output transaction already active",
            ));
        }
        let next = self
            .generation
            .borrow()
            .checked_add(1)
            .ok_or_else(|| Error::value("Output generation overflow"))?;
        *self.generation.borrow_mut() = next;
        *self.pending.borrow_mut() = Some(PendingOutputs {
            tag,
            generation: next,
            file_scope: next,
            epoch: self.epoch(),
            budget: byte_budget,
            total_bytes: 0,
            names: Default::default(),
            inodes: Default::default(),
            removed: Default::default(),
            next_inode: 1,
        });
        Ok(())
    }
    /// An explicit read generation for the next revision. No catalog publication
    /// and no unrelated output, reader, writer or console state is inherited.
    pub(crate) fn auxiliary_input_seed(
        &self,
        tag: DocumentRevision,
        inputs: &BTreeMap<PathBuf, Option<Arc<[u8]>>>,
    ) -> Result<OutputCheckpoint> {
        let mut pending = PendingOutputs {
            tag,
            generation: 0,
            file_scope: *self.generation.borrow(),
            epoch: self.epoch(),
            budget: 64 * 1024 * 1024,
            total_bytes: 0,
            names: Default::default(),
            inodes: Default::default(),
            removed: Default::default(),
            next_inode: 1,
        };
        for (path, bytes) in inputs {
            self.validate_document_path(path)?;
            if let Some(bytes) = bytes {
                let id = pending.bind(path, Arc::from([])).map_err(Error::from)?;
                pending
                    .set_output(id, Arc::new(bytes.to_vec()))
                    .map_err(Error::from)?;
            } else if self.committed(path)?.is_some() {
                pending.removed.insert(path.clone(), ());
            }
        }
        Ok(OutputCheckpoint {
            pending,
            console: Arc::new(Vec::new()),
            console_bytes: 0,
            console_overflow: 0,
        })
    }
    pub(crate) fn checkpoint_effects(&self) -> Result<OutputCheckpoint> {
        Ok(OutputCheckpoint {
            pending: self
                .pending
                .borrow()
                .as_ref()
                .ok_or_else(|| Error::value("No output transaction"))?
                .clone(),
            console: self.console.borrow().clone(),
            console_bytes: *self.console_bytes.borrow(),
            console_overflow: *self.console_overflow_bytes.borrow(),
        })
    }
    pub(crate) fn restore_effects(
        &self,
        snapshot: &OutputCheckpoint,
        tag: DocumentRevision,
    ) -> Result<()> {
        self.discard_effects();
        self.begin_effects(tag, snapshot.pending.budget)?;
        let mut pending = self.pending.borrow_mut();
        let pending = pending.as_mut().unwrap();
        pending.names = snapshot.pending.names.clone();
        pending.file_scope = snapshot.pending.file_scope;
        pending.inodes = snapshot.pending.inodes.clone();
        pending.removed = snapshot.pending.removed.clone();
        pending.next_inode = snapshot.pending.next_inode;
        pending.total_bytes = snapshot.pending.total_bytes;
        *self.console.borrow_mut() = snapshot.console.clone();
        *self.console_bytes.borrow_mut() = snapshot.console_bytes;
        *self.console_overflow_bytes.borrow_mut() = snapshot.console_overflow;
        Ok(())
    }
    /// Conservative, non-mutating budget preflight for a read-only prefix.
    pub(crate) fn read_only_fits_current(&self, snapshot: &OutputCheckpoint) -> bool {
        snapshot.read_only()
            && self.pending.borrow().as_ref().is_some_and(|current| {
                snapshot.pending.names.len() + current.names.len() + current.removed.len() <= 20_000
                    && snapshot.pending.inodes.len() + current.names.len() <= 20_000
            })
    }
    /// Rebind a prefix containing only catalog read handles over this pass's
    /// private journal. Prefix reader identities and buffered bytes survive;
    /// unopened auxiliary files acquire the new pass's bytes. Never copy old
    /// speculative outputs from the checkpoint into the next pass.
    pub(crate) fn restore_read_only_for_pass(
        &self,
        snapshot: &OutputCheckpoint,
        tag: &DocumentRevision,
    ) -> Result<bool> {
        if !snapshot.read_only() {
            return Ok(false);
        }
        let current_borrow = self.pending.borrow();
        let current = current_borrow
            .as_ref()
            .ok_or_else(|| Error::value("No output transaction"))?;
        if &current.tag != tag || current.epoch != self.epoch() {
            return Err(Error::value("Obsolete auxiliary pass"));
        }
        // Conservative preflight: on resource limits, initialized replay remains
        // available instead of altering the document's semantic budget.
        if snapshot.pending.names.len() + current.names.len() + current.removed.len() > 20_000
            || snapshot.pending.inodes.len() + current.names.len() > 20_000
        {
            return Ok(false);
        }
        let next = self
            .generation
            .borrow()
            .checked_add(1)
            .ok_or_else(|| Error::value("Output generation overflow"))?;
        let mut restored = snapshot.pending.clone();
        restored.tag = tag.clone();
        restored.epoch = current.epoch;
        restored.generation = next;
        restored.budget = current.budget;
        for path in current.names.keys() {
            if let Some(bytes) = current.output(path) {
                let id = restored.bind(path, Arc::from([])).map_err(Error::from)?;
                restored
                    .set_output(id, bytes.clone())
                    .map_err(Error::from)?;
            }
        }
        restored.removed = current.removed.clone();
        for path in restored.removed.keys() {
            restored.names.remove(path);
        }
        drop(current_borrow);
        *self.generation.borrow_mut() = next;
        *self.pending.borrow_mut() = Some(restored);
        *self.console.borrow_mut() = snapshot.console.clone();
        *self.console_bytes.borrow_mut() = snapshot.console_bytes;
        *self.console_overflow_bytes.borrow_mut() = snapshot.console_overflow;
        Ok(true)
    }
    pub fn discard_effects(&self) {
        self.pending.borrow_mut().take();
    }
    /// Start another execution pass without publishing the revision's journal.
    /// Only named effects survive; read bindings and unlinked handles belong to
    /// the completed pass. Advancing the handle generation closes those handles.
    pub(crate) fn next_effect_pass(&self, tag: &DocumentRevision) -> Result<()> {
        let outputs = self.staged_outputs(tag)?;
        let removals = self.staged_removals(tag)?;
        let budget = self.pending.borrow().as_ref().unwrap().budget;
        let next = self
            .generation
            .borrow()
            .checked_add(1)
            .ok_or_else(|| Error::value("Output generation overflow"))?;
        let mut pending = PendingOutputs {
            tag: tag.clone(),
            generation: next,
            file_scope: next,
            epoch: self.epoch(),
            budget,
            total_bytes: 0,
            names: Default::default(),
            inodes: Default::default(),
            removed: Default::default(),
            next_inode: 1,
        };
        for (path, bytes) in outputs {
            let path = physical_path(&path)?;
            let id = pending.bind(&path, Arc::from([])).map_err(Error::from)?;
            pending
                .set_output(id, Arc::new(bytes.to_vec()))
                .map_err(Error::from)?;
        }
        for path in removals {
            pending.removed.insert(physical_path(&path)?, ());
        }
        *self.generation.borrow_mut() = next;
        *self.pending.borrow_mut() = Some(pending);
        Ok(())
    }
    pub fn pending_bytes(&self) -> usize {
        self.pending.borrow().as_ref().map_or(0, |p| p.total_bytes)
    }
    pub fn staged_outputs(
        &self,
        tag: &DocumentRevision,
    ) -> Result<BTreeMap<ResourcePath, Arc<[u8]>>> {
        let pending = self.pending.borrow();
        let pending = pending
            .as_ref()
            .ok_or_else(|| Error::value("No speculative outputs"))?;
        if &pending.tag != tag || pending.epoch != self.epoch() {
            return Err(Error::value("Obsolete speculative outputs"));
        }
        Ok(pending
            .names
            .iter()
            .filter_map(|(path, _)| {
                pending
                    .output(path)
                    .map(|bytes| (resource_path(path), Arc::from(bytes.as_slice())))
            })
            .collect())
    }
    pub fn staged_output_paths(&self, tag: &DocumentRevision) -> Result<Vec<ResourcePath>> {
        let pending = self.pending.borrow();
        let pending = pending
            .as_ref()
            .ok_or_else(|| Error::value("No speculative outputs"))?;
        if &pending.tag != tag || pending.epoch != self.epoch() {
            return Err(Error::value("Obsolete speculative outputs"));
        }
        Ok(pending
            .names
            .keys()
            .filter(|path| pending.output(path).is_some())
            .map(|path| resource_path(path))
            .collect())
    }
    pub fn commit_effects(&self, accepted: &DocumentRevision) -> Result<()> {
        let borrow = self.pending.borrow();
        let pending = borrow
            .as_ref()
            .ok_or_else(|| Error::value("No speculative outputs"))?;
        if &pending.tag != accepted || pending.epoch != self.epoch() {
            return Err(Error::value(
                "Obsolete speculative outputs cannot be published",
            ));
        }
        let mut catalog = self.catalog.borrow_mut();
        let mut transaction = EffectTransaction::new(pending.tag.clone(), &catalog, pending.budget);
        for (path, _) in &pending.names {
            if let Some(bytes) = pending.output(path) {
                transaction.write(resource_path(path), bytes.as_slice().into())?;
            }
        }
        for (path, _) in &pending.removed {
            // Removing a temporary created in this revision has no catalog
            // source; it still hides any old committed spelling during replay.
            if catalog.get(&resource_path(path)).is_some() {
                transaction.remove(&catalog, resource_path(path))?;
            }
        }
        transaction.commit(&mut catalog, accepted)?;
        drop(catalog);
        // Resource limits/version overflow can reject publication too. Keep the
        // complete overlay and handles until the atomic catalog commit succeeds.
        drop(borrow);
        self.pending.borrow_mut().take();
        // File rename/unlink does not remove the containing directories.
        // Resource-catalog updates retain their separate discovery contract.
        let existing = self
            .directories
            .borrow()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        self.rebuild_directories_preserving(&existing)
    }
    pub fn staged_removals(&self, tag: &DocumentRevision) -> Result<Vec<ResourcePath>> {
        let pending = self.pending.borrow();
        let pending = pending
            .as_ref()
            .ok_or_else(|| Error::value("No speculative outputs"))?;
        if &pending.tag != tag || pending.epoch != self.epoch() {
            return Err(Error::value("Obsolete speculative outputs"));
        }
        Ok(pending
            .removed
            .keys()
            .map(|path| resource_path(path))
            .collect())
    }
    pub fn committed(&self, path: &Path) -> io::Result<Option<Arc<[u8]>>> {
        let path = self.normalize(path)?;
        Ok(self
            .catalog
            .borrow()
            .get(&resource_path(&path))
            .map(|r| r.bytes.clone()))
    }
    pub fn take_console(&self) -> Vec<(bool, Vec<u8>)> {
        *self.console_bytes.borrow_mut() = 0;
        Arc::unwrap_or_clone(std::mem::take(&mut *self.console.borrow_mut()))
    }
    pub fn console_overflow_bytes(&self) -> usize {
        *self.console_overflow_bytes.borrow()
    }
}
impl Host for VirtualHost {
    fn open_physical(
        &self,
        path: &Path,
    ) -> io::Result<Option<Box<dyn crate::language::input_frames::PhysicalStream>>> {
        let path = self.normalize(path)?;
        let initial = self.physical_source_bytes(&path)?;
        let (generation, file_scope, file_id) =
            if let Some(pending) = self.pending.borrow_mut().as_mut() {
                (
                    Some(pending.generation),
                    Some(pending.file_scope),
                    Some(pending.bind(&path, initial.clone())?),
                )
            } else {
                (None, None, None)
            };
        if let Some(scope) = self.reuse_scope.borrow().upgrade() {
            let mut bindings = scope.input_bindings.borrow_mut();
            if bindings.len() >= 512 || path.as_os_str().len() > 4096 {
                scope.reject("input-binding-budget");
            } else {
                bindings.push(path.clone());
            }
        }
        Ok(Some(Box::new(VirtualStream(
            crate::language::input_frames::SourceStream::new(VirtualBufferedReader {
                pending: self.pending.clone(),
                generation,
                file_scope,
                file_id,
                initial,
                path: path.clone(),
                read_position: 0,
                buffer: Arc::new(Vec::new()),
                location: 0,
            }),
        ))))
    }
    fn physical_source_bytes(&self, path: &Path) -> io::Result<Arc<[u8]>> {
        let path = self.normalize(path)?;
        if let Some(pending) = self.pending.borrow().as_ref() {
            if pending.removed.contains_key(&path) {
                return Err(missing(&path));
            }
            if let Some(bytes) = pending.output(&path) {
                self.reject_resource_reuse("private-input");
                return Ok(bytes.as_slice().into());
            }
        }
        // Material reuse still requires the full immutable file. Only live
        // checkpoint selection may substitute a validated physical prefix.
        let value = self.query_result(&path, &QueryKind::PhysicalBytes);
        self.record_resource(&self.dependencies, &path, QueryKind::PhysicalBytes, value);
        if let Some(scope) = self.reuse_scope.borrow().upgrade() {
            let value = self.query_result(&path, &QueryKind::Bytes);
            self.record_resource(&scope.resources, &path, QueryKind::Bytes, value);
        }
        self.catalog
            .borrow()
            .get(&resource_path(&path))
            .map(|r| r.bytes.clone())
            .ok_or_else(|| missing(&path))
    }
    fn observe_time(&self) {
        self.reject_resource_reuse("time");
        *self.wall_observations.borrow_mut() += 1;
    }
    fn input_bytes(&self, path: &Path) -> io::Result<Option<Arc<[u8]>>> {
        let path = self.normalize(path)?;
        self.observe_query(&path);
        if let Some(pending) = self.pending.borrow().as_ref() {
            if pending.removed.contains_key(&path) {
                return Err(missing(&path));
            }
            if let Some(bytes) = pending.output(&path) {
                self.reject_resource_reuse("private-input");
                return Ok(Some(bytes.as_slice().into()));
            }
        }
        self.observe_resource(&path, QueryKind::Bytes);
        self.catalog
            .borrow()
            .get(&resource_path(&path))
            .map(|r| Some(r.bytes.clone()))
            .ok_or_else(|| missing(&path))
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let path = self.normalize(path)?;
        self.observe_query(&path);
        if let Some(pending) = self.pending.borrow().as_ref() {
            if pending.removed.contains_key(&path) {
                return Err(missing(&path));
            }
            if let Some(bytes) = pending.output(&path) {
                self.reject_resource_reuse("private-read");
                return Ok(bytes.as_ref().clone());
            }
        }
        self.observe_resource(&path, QueryKind::Bytes);
        let result = self
            .catalog
            .borrow_mut()
            .lookup(&[resource_path(&path)])
            .map_err(io_error)?;
        result
            .resource
            .map(|r| r.bytes.to_vec())
            .ok_or_else(|| missing(&path))
    }
    fn file_name_absent(&self, name: &Path) -> bool {
        let Some(name) = name.file_name() else {
            return false;
        };
        if self.file_names.borrow().contains(name)
            || self.pending.borrow().as_ref().is_some_and(|p| {
                p.names.keys().any(|file| {
                    file.file_name() == Some(name)
                        && !p.removed.contains_key(file)
                        && p.output(file).is_some()
                })
            })
        {
            return false;
        }
        // This stronger negative observation also invalidates when a same-named
        // file is supplied outside the original search roots. It never drops a
        // queried miss or treats a private output as absent.
        self.observe_resource(Path::new(name), QueryKind::FileNamePresent);
        true
    }
    fn file_name_candidates(&self, name: &Path) -> Option<Arc<Vec<PathBuf>>> {
        let name = name.file_name()?;
        if self.pending.borrow().as_ref().is_some_and(|p| {
            p.names
                .keys()
                .any(|f| f.file_name() == Some(name) && p.output(f).is_some())
                || p.removed.keys().any(|f| f.file_name() == Some(name))
        }) {
            self.reject_resource_reuse("private-file-name");
        }
        self.observe_resource(Path::new(name), QueryKind::FileNameMatches);
        let members = self
            .file_members
            .borrow()
            .get(name)
            .cloned()
            .unwrap_or_default();
        let pending = self.pending.borrow();
        let additional: Vec<_> = pending
            .as_ref()
            .into_iter()
            .flat_map(|p| {
                p.names.keys().filter(|file| {
                    file.file_name() == Some(name)
                        && !p.removed.contains_key(*file)
                        && p.file(file).is_some()
                        && !members.contains(*file)
                })
            })
            .cloned()
            .collect();
        if additional.is_empty() {
            Some(members)
        } else {
            let mut combined = members.as_ref().clone();
            combined.extend(additional);
            Some(Arc::new(combined))
        }
    }
    fn is_file(&self, path: &Path) -> bool {
        self.classification(path).is_ok_and(|directory| !directory)
    }
    fn is_dir(&self, path: &Path) -> bool {
        self.classification(path).is_ok_and(|directory| directory)
    }
    fn metadata(&self, path: &Path) -> io::Result<FileMetadata> {
        let path = self.normalize(path)?;
        self.observe_query(&path);
        let output = self
            .pending
            .borrow()
            .as_ref()
            .and_then(|p| p.output(&path).map(|bytes| bytes.len()));
        if self
            .pending
            .borrow()
            .as_ref()
            .is_some_and(|p| p.removed.contains_key(&path))
        {
            return Err(missing(&path));
        }
        self.observe_resource(
            &path,
            if output.is_some() {
                QueryKind::Directory
            } else {
                QueryKind::Classification
            },
        );
        let resource = self
            .catalog
            .borrow_mut()
            .lookup(&[resource_path(&path)])
            .map_err(io_error)?
            .resource;
        let directory = self.directories.borrow().contains_key(&path);
        if output.is_none() && resource.is_none() && !directory {
            return Err(missing(&path));
        }
        let observations = self.dependencies.clone();
        let reuse_observations = self.reuse_scope.borrow().clone();
        let catalog_input = output.is_none() && resource.is_some();
        let supplied = if output.is_none() {
            resource.as_ref().and_then(|resource| resource.modified)
        } else {
            None
        };
        let len_observations = self.dependencies.clone();
        let len_reuse = self.reuse_scope.borrow().clone();
        let len_path = path.clone();
        Ok(FileMetadata::Virtual {
            observe_len: Rc::new(move |len| {
                if !catalog_input {
                    return;
                }
                let value = QueryResult::Metadata(Some((len, false)));
                let mut targets = vec![len_observations.as_ref()];
                let reuse = len_reuse.upgrade();
                if let Some(scope) = &reuse {
                    targets.push(&scope.resources);
                }
                for target in targets {
                    let mut dependencies = target.borrow_mut();
                    if dependencies.overflow {
                        continue;
                    }
                    let key = (len_path.clone(), QueryKind::Metadata);
                    if let Some(previous) = dependencies.queries.get(&key) {
                        if previous != &value {
                            dependencies.overflow = true;
                        }
                        continue;
                    }
                    let cost = resource_path(&len_path).0.len() + 64;
                    if dependencies.queries.len() >= 20000
                        || dependencies.bytes.saturating_add(cost) > 8 * 1024 * 1024
                    {
                        dependencies.overflow = true;
                        continue;
                    }
                    dependencies.bytes += cost;
                    *dependencies.validated.borrow_mut() = None;
                    dependencies.queries.insert(key, value.clone());
                }
            }),
            len: output.unwrap_or_else(|| resource.as_ref().map_or(0, |r| r.bytes.len())) as u64,
            modified: supplied.unwrap_or_else(|| self.clock.borrow().time.into()),
            directory,
            identity: identity(&path),
            observe_modified: Rc::new(move |time| {
                let mut targets = vec![observations.as_ref()];
                let reuse_observations = reuse_observations.upgrade();
                if let Some(scope) = &reuse_observations {
                    targets.push(&scope.resources);
                }
                for observations in targets {
                    let mut dependencies = observations.borrow_mut();
                    if dependencies.overflow {
                        continue;
                    }
                    if catalog_input {
                        let key = (path.clone(), QueryKind::Modified);
                        let value = QueryResult::Modified(Some(time));
                        match dependencies.queries.get(&key) {
                            Some(previous) if previous == &value => continue,
                            Some(_) => {
                                dependencies.overflow = true;
                                continue;
                            }
                            None => {}
                        }
                        let cost = resource_path(&path).0.len() + 64;
                        if dependencies.queries.len() >= 20_000
                            || dependencies.bytes.saturating_add(cost) > 8 * 1024 * 1024
                        {
                            dependencies.overflow = true;
                            continue;
                        }
                        dependencies.bytes += cost;
                        *dependencies.validated.borrow_mut() = None;
                        dependencies.queries.insert(key, value);
                        continue;
                    }
                    if dependencies
                        .modified
                        .is_some_and(|previous| previous != time)
                    {
                        dependencies.overflow = true;
                    }
                    dependencies.modified = Some(time);
                }
            }),
        })
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let path = self.normalize(path)?;
        if self.pending.borrow().as_ref().is_some_and(|p| {
            p.names
                .keys()
                .any(|f| f.parent() == Some(path.as_path()) && p.output(f).is_some())
                || p.removed.keys().any(|f| f.parent() == Some(path.as_path()))
        }) {
            self.reject_resource_reuse("private-directory");
        }
        self.observe_query(&path);
        self.observe_resource(&path, QueryKind::Children);
        let mut children = self
            .directories
            .borrow()
            .get(&path)
            .map(|children| children.as_ref().clone())
            .ok_or_else(|| missing(&path))?;
        if let Some(pending) = self.pending.borrow().as_ref() {
            children.retain(|child| !pending.removed.contains_key(child));
            for file in pending.names.keys().filter(|file| {
                file.parent() == Some(path.as_path()) && pending.output(file).is_some()
            }) {
                if !children.contains(file) {
                    children.push(file.clone());
                }
            }
        }
        children.sort();
        Ok(children)
    }
    fn search_subdirectories(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let path = self.normalize(path)?;
        self.observe_query(&path);
        self.observe_resource(&path, QueryKind::SearchDirectories);
        let QueryResult::Children(children) =
            self.query_result(&path, &QueryKind::SearchDirectories)
        else {
            unreachable!()
        };
        children
            .map(|children| children.as_ref().clone())
            .ok_or_else(|| missing(&path))
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        let path = self.normalize(path)?;
        self.classification(&path)?;
        Ok(path)
    }
    fn current_dir(&self) -> io::Result<PathBuf> {
        Ok(self.cwd.clone())
    }
    fn variable(&self, name: &str) -> Option<OsString> {
        self.variables.get(name).cloned()
    }
    fn open_output(&self, path: &Path, append: bool) -> io::Result<Writer> {
        self.reject_resource_reuse("open-output");
        let path = self.normalize(path)?;
        if self.directories.borrow().contains_key(&path) {
            return Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                "Output path is a directory",
            ));
        }
        // A program can overwrite its own physical input. Such a prefix cannot
        // be reused using the editor's byte identity alone.
        self.observe_query(&path);
        let parent = path
            .parent()
            .ok_or_else(|| io_error("Output has no parent directory"))?;
        self.observe_resource(parent, QueryKind::Directory);
        if !self.directories.borrow().contains_key(parent) {
            return Err(missing(parent));
        }
        let existing = if append {
            self.require_unstamped_mutation(&path, "Append")?;
            Some(self.read(&path).or_else(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    Ok(Vec::new())
                } else {
                    Err(e)
                }
            })?)
        } else {
            None
        };
        let mut pending = self.pending.borrow_mut();
        let pending = pending.as_mut().ok_or_else(|| {
            io_error("Output publication requires an active revision transaction")
        })?;
        let bytes = existing.unwrap_or_default();
        let previous = pending.output(&path).map_or(0, |bytes| bytes.len());
        let total = (pending.total_bytes - previous)
            .checked_add(bytes.len())
            .ok_or_else(|| io_error("Output byte overflow"))?;
        if total > pending.budget {
            return Err(io_error("Speculative output byte budget exceeded"));
        }
        let position = bytes.len();
        let file_id = pending.bind(&path, Arc::from([]))?;
        pending.set_output(file_id, Arc::new(bytes))?;
        pending.removed.remove(&path);
        Ok(Box::new(VirtualWriter {
            reuse_token: Rc::new(()),
            reuse_instance: Rc::new(()),
            reuse_owner: self.validation_scope.clone(),
            reuse_scope: self.reuse_scope.clone(),
            pending: self.pending.clone(),
            generation: pending.generation,
            file_scope: pending.file_scope,
            file_id,
            position,
            append,
        }))
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.reject_resource_reuse("rename");
        let from = self.normalize(from)?;
        let to = self.normalize(to)?;
        if self.directories.borrow().contains_key(&to) {
            return Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                "Rename target is a directory",
            ));
        }
        self.observe_query(&from);
        self.observe_query(&to);
        let bytes = self.read(&from)?;
        if from == to {
            return Ok(());
        }
        self.require_unstamped_mutation(&from, "Rename")?;
        let mut pending = self.pending.borrow_mut();
        let pending = pending
            .as_mut()
            .ok_or_else(|| io_error("Rename requires an active output transaction"))?;
        if let Some(parent) = to.parent() {
            self.observe_resource(parent, QueryKind::Directory);
        }
        if !to
            .parent()
            .is_some_and(|p| self.directories.borrow().contains_key(p))
        {
            return Err(missing(&to));
        }
        pending.check_paths_budget(&[&from, &to])?;
        let previous = pending.output(&from).map_or(0, |data| data.len());
        let total = (pending.total_bytes - previous)
            .checked_add(bytes.len())
            .ok_or_else(|| io_error("Output byte overflow"))?;
        if total > pending.budget {
            return Err(io_error("Speculative output byte budget exceeded"));
        }
        let file_id = pending.bind(&from, bytes.as_slice().into())?;
        // Materialize a catalog input only when it becomes a renamed output.
        if pending.output(&from).is_none() {
            pending.set_output(file_id, Arc::new(bytes))?;
        }
        pending.names.remove(&from);
        pending.names.insert(to.clone(), file_id);
        pending.removed.insert(from, ());
        pending.removed.remove(&to);
        Ok(())
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.reject_resource_reuse("remove");
        let path = self.normalize(path)?;
        self.observe_query(&path);
        self.read(&path)?;
        let mut pending = self.pending.borrow_mut();
        let pending = pending
            .as_mut()
            .ok_or_else(|| io_error("Remove requires an active output transaction"))?;
        pending.check_path_budget(&path)?;
        pending.names.remove(&path);
        pending.removed.insert(path, ());
        Ok(())
    }
    fn now(&self) -> SystemTime {
        self.clock.borrow().time
    }
    fn monotonic_nanos(&self) -> u128 {
        *self.clock_observations.borrow_mut() += 1;
        (self.monotonic.borrow())()
    }
    fn local_date(&self, time: SystemTime) -> chrono::DateTime<chrono::FixedOffset> {
        self.clock.borrow().local_date(time)
    }
    fn local_timestamp(
        &self,
        time: chrono::DateTime<chrono::Utc>,
    ) -> chrono::DateTime<chrono::FixedOffset> {
        time.with_timezone(&self.clock.borrow().offset)
    }
    fn console(&self, error: bool, bytes: &[u8]) -> io::Result<()> {
        if let Some(scope) = self.reuse_scope.borrow().upgrade() {
            let mut records = scope.console.borrow_mut();
            if records.len() >= 1024
                || records
                    .iter()
                    .map(|r| r.1.len())
                    .sum::<usize>()
                    .saturating_add(bytes.len())
                    > 64 * 1024
            {
                scope.reject("console-budget");
            } else {
                let order = scope.sequence.get();
                scope.sequence.set(order + 1);
                scope.console_sequence.borrow_mut().push(order);
                records.push((error, bytes.to_vec()));
            }
        }
        let total = self
            .console_bytes
            .borrow()
            .checked_add(bytes.len())
            .ok_or_else(|| io_error("Console byte count overflow"))?;
        if total > self.console_limit || self.console.borrow().len() >= 4096 {
            let mut overflow = self.console_overflow_bytes.borrow_mut();
            *overflow = overflow.saturating_add(bytes.len());
            return Err(io_error("Host console byte budget exceeded"));
        }
        *self.console_bytes.borrow_mut() = total;
        Arc::make_mut(&mut self.console.borrow_mut()).push((error, bytes.to_vec()));
        Ok(())
    }
}
// Match the 8 KiB read-ahead used by native BufReader and the reference's
// TextIOWrapper. Reopening/truncating a file changes subsequent refills, while
// already buffered bytes belong to the opened reader. Checkpoints retain both
// the buffer and the underlying file offset.
const INPUT_BUFFER_SIZE: usize = 8192;
#[derive(Clone)]
struct VirtualBufferedReader {
    pending: Rc<RefCell<Option<PendingOutputs>>>,
    generation: Option<u64>,
    file_scope: Option<u64>,
    file_id: Option<u64>,
    initial: Arc<[u8]>,
    path: PathBuf,
    read_position: usize,
    buffer: Arc<Vec<u8>>,
    location: usize,
}
impl VirtualBufferedReader {
    fn byte_position(&self) -> usize {
        self.read_position - self.buffer.len() + self.location
    }
    fn replace_bytes(&mut self, bytes: Arc<[u8]>) -> io::Result<()> {
        let position = self.byte_position();
        if position > bytes.len() {
            return Err(io_error("Checkpoint past new input"));
        }
        // The physical line has already been decoded, but its read-ahead may
        // include the edited body. Rebuild that window without moving the cursor.
        let start = self.read_position - self.buffer.len();
        let end = bytes.len().min(start.saturating_add(INPUT_BUFFER_SIZE));
        self.buffer = Arc::new(bytes[start..end].to_vec());
        self.location = position - start;
        self.read_position = end;
        if let Some(file_id) = self.file_id {
            let mut pending = self.pending.borrow_mut();
            let pending = pending
                .as_mut()
                .filter(|p| Some(p.generation) == self.generation)
                .ok_or_else(|| io_error("Cancelled/obsolete input reader"))?;
            pending
                .inodes
                .insert(file_id, FileData::Input(bytes.clone()));
        }
        self.initial = bytes;
        Ok(())
    }
}
impl BufRead for VirtualBufferedReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        let pending = self.pending.borrow();
        let active = match self.generation {
            Some(generation) => Some(
                pending
                    .as_ref()
                    .filter(|p| p.generation == generation)
                    .ok_or_else(|| io_error("Cancelled/obsolete input reader"))?,
            ),
            None => None,
        };
        if self.location == self.buffer.len() {
            let bytes = match (active, self.file_id) {
                (Some(pending), Some(id)) => pending
                    .inodes
                    .get(&id)
                    .ok_or_else(|| io_error("Missing live input identity"))?
                    .bytes(),
                _ => self.initial.as_ref(),
            };
            let start = self.read_position.min(bytes.len());
            let end = bytes
                .len()
                .min(self.read_position.saturating_add(INPUT_BUFFER_SIZE));
            self.buffer = Arc::new(bytes[start..end].to_vec());
            self.location = 0;
            // Reading at EOF doesn't move the file offset, even after truncate.
            self.read_position += end - start;
        }
        Ok(&self.buffer[self.location..])
    }
    fn consume(&mut self, amount: usize) {
        self.location = (self.location + amount).min(self.buffer.len());
    }
}
impl Read for VirtualBufferedReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let input = self.fill_buf()?;
        let length = output.len().min(input.len());
        output[..length].copy_from_slice(&input[..length]);
        self.consume(length);
        Ok(length)
    }
}
struct VirtualStream(crate::language::input_frames::SourceStream<VirtualBufferedReader>);
impl crate::language::input_frames::PhysicalStream for VirtualStream {
    fn readline(
        &mut self,
        limit: Option<usize>,
        profile: crate::core::characters::LanguageProfile,
    ) -> Result<Option<String>> {
        self.0.readline(limit, profile)
    }
    fn close(&mut self) {
        self.0.close();
    }
    fn fork(&self) -> Option<Box<dyn crate::language::input_frames::PhysicalStream>> {
        let mut stream = self.0.stream.clone();
        if let Some(reader) = &mut stream {
            if reader.generation.is_some() {
                let pending = reader.pending.borrow();
                let pending = pending.as_ref()?;
                if Some(pending.file_scope) != reader.file_scope {
                    return None;
                }
                reader.generation = Some(pending.generation);
            }
        }
        Some(Box::new(Self(
            crate::language::input_frames::SourceStream { stream },
        )))
    }
    fn byte_position(&self) -> Option<usize> {
        self.0
            .stream
            .as_ref()
            .map(VirtualBufferedReader::byte_position)
    }
    fn physical_path(&self) -> Option<&Path> {
        self.0.stream.as_ref().map(|r| r.path.as_path())
    }
    fn observe_explicit_read(&self) -> Result<()> {
        if let Some(path) = self.physical_path() {
            crate::resources::host::current().input_bytes(path)?;
        }
        Ok(())
    }
    fn buffered_end(&self) -> Option<usize> {
        self.0.stream.as_ref().map(|r| r.read_position)
    }
    fn catalog_bytes(&self) -> Option<Arc<[u8]>> {
        let reader = self.0.stream.as_ref()?;
        if let Some(file_id) = reader.file_id {
            let pending = reader.pending.borrow();
            let pending = pending.as_ref()?;
            if Some(pending.generation) != reader.generation
                || Some(pending.file_scope) != reader.file_scope
            {
                return None;
            }
            let FileData::Input(bytes) = pending.inodes.get(&file_id)? else {
                return None;
            };
            if !Arc::ptr_eq(bytes, &reader.initial) {
                return None;
            }
        }
        Some(reader.initial.clone())
    }
    fn seek_catalog(&mut self, position: usize) -> Result<()> {
        use std::io::BufRead;
        let bytes = self
            .catalog_bytes()
            .ok_or_else(|| Error::value("Mutable or obsolete source reader"))?;
        let reader = self
            .0
            .stream
            .as_mut()
            .ok_or_else(|| Error::value("Closed input stream"))?;
        if position < reader.byte_position() || position > bytes.len() {
            return Err(Error::value("Invalid source advance"));
        }
        // Consume the same buffered windows ordinary reading would consume.
        // Resetting read-ahead here would change later writes to an open file.
        while reader.byte_position() < position {
            let remaining = position - reader.byte_position();
            let n = remaining.min(reader.fill_buf()?.len());
            if n == 0 {
                return Err(Error::value("Unexpected end of source advance"));
            }
            reader.consume(n);
        }
        Ok(())
    }
    fn replace_bytes(&mut self, bytes: Arc<[u8]>) -> Result<()> {
        self.0
            .stream
            .as_mut()
            .ok_or_else(|| Error::value("Closed input stream"))?
            .replace_bytes(bytes)?;
        Ok(())
    }
}
fn writer_path(pending: &PendingOutputs, id: u64) -> Option<PathBuf> {
    let mut names = pending
        .names
        .iter()
        .filter_map(|(path, &number)| (number == id).then_some(path));
    let path = names.next()?.clone();
    if names.next().is_some() {
        return None;
    }
    Some(path)
}
struct VirtualWriter {
    reuse_token: Rc<()>,
    reuse_instance: Rc<()>,
    reuse_owner: Arc<()>,
    reuse_scope: Rc<RefCell<std::rc::Weak<ResourceReadScope>>>,
    pending: Rc<RefCell<Option<PendingOutputs>>>,
    generation: u64,
    file_scope: u64,
    file_id: u64,
    position: usize,
    append: bool,
}
impl super::host::CheckpointWriter for VirtualWriter {
    fn reuse_state(&self) -> Option<super::host::ReuseWriterState> {
        let pending = self.pending.borrow();
        let pending = pending.as_ref()?;
        if pending.generation != self.generation || pending.file_scope != self.file_scope {
            return None;
        }
        let FileData::Output(data) = pending.inodes.get(&self.file_id)? else {
            return None;
        };
        Some(super::host::ReuseWriterState {
            stream: None,
            token: self.reuse_token.clone(),
            instance: self.reuse_instance.clone(),
            owner: self.reuse_owner.clone(),
            path: writer_path(pending, self.file_id)?,
            bytes: data.clone(),
            position: if self.append {
                data.len()
            } else {
                self.position
            },
            append: self.append,
        })
    }

    fn fork(&self) -> io::Result<Option<Writer>> {
        let pending = self.pending.borrow();
        let pending = pending
            .as_ref()
            .ok_or_else(|| io_error("No output transaction to resume"))?;
        if pending.file_scope != self.file_scope || !pending.inodes.contains_key(&self.file_id) {
            return Err(io_error("Missing live output identity"));
        }
        Ok(Some(Box::new(Self {
            reuse_token: self.reuse_token.clone(),
            reuse_instance: Rc::new(()),
            reuse_owner: self.reuse_owner.clone(),
            reuse_scope: self.reuse_scope.clone(),
            pending: self.pending.clone(),
            generation: pending.generation,
            file_scope: self.file_scope,
            file_id: self.file_id,
            position: self.position,
            append: self.append,
        })))
    }
}
impl Write for VirtualWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let scope = self.reuse_scope.borrow().upgrade();
        let mut pending = self.pending.borrow_mut();
        let pending = pending
            .as_mut()
            .filter(|p| p.generation == self.generation)
            .ok_or_else(|| io_error("Cancelled/obsolete output writer"))?;
        let path = scope
            .as_ref()
            .and_then(|_| writer_path(pending, self.file_id));
        let Some(FileData::Output(data)) = pending.inodes.get_mut(&self.file_id) else {
            return Err(io_error("Missing live output identity"));
        };
        if self.append {
            self.position = data.len();
        }
        let end = self
            .position
            .checked_add(bytes.len())
            .ok_or_else(|| io_error("Output length overflow"))?;
        let growth = end.saturating_sub(data.len());
        let total = pending
            .total_bytes
            .checked_add(growth)
            .ok_or_else(|| io_error("Output byte count overflow"))?;
        if total > pending.budget {
            return Err(io_error("Speculative output byte budget exceeded"));
        }
        if let Some(scope) = scope {
            if let Some(path) = path {
                scope.record_writer(
                    super::host::ReuseWriterState {
                        stream: None,
                        token: self.reuse_token.clone(),
                        instance: self.reuse_instance.clone(),
                        owner: self.reuse_owner.clone(),
                        path,
                        bytes: data.clone(),
                        position: self.position,
                        append: self.append,
                    },
                    bytes,
                );
            } else {
                scope.reject("unnamed-writer");
            }
        }
        let data = Arc::make_mut(data);
        if end > data.len() {
            data.resize(end, 0);
        }
        data[self.position..end].copy_from_slice(bytes);
        self.position = end;
        pending.total_bytes = total;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if self
            .pending
            .borrow()
            .as_ref()
            .is_some_and(|p| p.generation == self.generation)
        {
            Ok(())
        } else {
            Err(io_error("Cancelled/obsolete output writer"))
        }
    }
}
