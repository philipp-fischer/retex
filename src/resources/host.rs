//! Explicit job host for the existing parsers and engine.
//!
//! Engine entry points install a short-lived, thread-local dispatch guard for
//! static resource parsers. The engine owns the handle; no process environment
//! or working directory is changed. Guards must not span an async suspension.
//! A yielded engine owns its streams, and reinstalls its host on the next slice.
use chrono::{DateTime, FixedOffset, Local, Offset, Utc};
use std::{
    cell::RefCell,
    ffi::OsString,
    fs::{File, Metadata, OpenOptions},
    io::{self, Cursor, Read, Seek, Write},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}
pub type Reader = Box<dyn ReadSeek>;
/// An opaque identity and complete bounded input state for a transactional
/// writer. Hosts without this replay contract keep returning `None`.
#[derive(Clone)]
pub struct ReuseWriterState {
    pub(crate) stream: Option<u8>,
    pub(crate) token: Rc<()>,
    pub(crate) instance: Rc<()>,
    pub(crate) owner: std::sync::Arc<()>,
    pub(crate) path: PathBuf,
    pub(crate) bytes: std::sync::Arc<Vec<u8>>,
    pub(crate) position: usize,
    pub(crate) append: bool,
}
/// A writer can participate in rollback only if it can rebind without reopening
/// or truncating. `reuse_state` additionally promises that writes participate in
/// the owning host's bounded observation scope; other hosts return `None`.
pub trait CheckpointWriter: Write {
    fn reuse_state(&self) -> Option<ReuseWriterState> {
        None
    }
    fn fork(&self) -> io::Result<Option<Writer>> {
        Ok(None)
    }
}
impl CheckpointWriter for File {}
pub type Writer = Box<dyn CheckpointWriter>;
pub type HostHandle = Rc<dyn Host>;

pub enum FileMetadata {
    Native(Metadata),
    Virtual {
        len: u64,
        modified: DateTime<Utc>,
        directory: bool,
        identity: u64,
        observe_modified: Rc<dyn Fn(DateTime<Utc>)>,
        observe_len: Rc<dyn Fn(u64)>,
    },
}
impl FileMetadata {
    pub fn len(&self) -> u64 {
        match self {
            Self::Native(m) => m.len(),
            Self::Virtual {
                len, observe_len, ..
            } => {
                observe_len(*len);
                *len
            }
        }
    }
    pub fn is_file(&self) -> bool {
        match self {
            Self::Native(m) => m.is_file(),
            Self::Virtual { directory, .. } => !directory,
        }
    }
    pub fn is_dir(&self) -> bool {
        match self {
            Self::Native(m) => m.is_dir(),
            Self::Virtual { directory, .. } => *directory,
        }
    }
    pub fn modified(&self) -> io::Result<SystemTime> {
        if let Self::Native(m) = self {
            return m.modified();
        }
        let date = self.modified_timestamp()?;
        let seconds = date.timestamp();
        let nanos = date.timestamp_subsec_nanos();
        let result = if seconds >= 0 {
            UNIX_EPOCH.checked_add(Duration::new(seconds as u64, nanos))
        } else {
            let magnitude = seconds.unsigned_abs();
            let duration = if nanos == 0 {
                Duration::new(magnitude, 0)
            } else {
                Duration::new(magnitude - 1, 1_000_000_000 - nanos)
            };
            UNIX_EPOCH.checked_sub(duration)
        };
        result.ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported,
            "File date cannot be represented by this platform's SystemTime; use the signed timestamp API"))
    }
    /// Signed physical dates are independent of the platform SystemTime range.
    pub fn modified_timestamp(&self) -> io::Result<DateTime<Utc>> {
        match self {
            Self::Native(m) => {
                let time = m.modified()?;
                let (seconds, nanos) = match time.duration_since(UNIX_EPOCH) {
                    Ok(d) => (i64::try_from(d.as_secs()).ok(), d.subsec_nanos()),
                    Err(e) => {
                        let d = e.duration();
                        let secs = i64::try_from(d.as_secs()).ok();
                        if d.subsec_nanos() == 0 {
                            (secs.and_then(|n| n.checked_neg()), 0)
                        } else {
                            (
                                secs.and_then(|n| n.checked_neg()?.checked_sub(1)),
                                1_000_000_000 - d.subsec_nanos(),
                            )
                        }
                    }
                };
                seconds
                    .and_then(|s| DateTime::from_timestamp(s, nanos))
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::Unsupported,
                            "File modification date is outside the signed timestamp range",
                        )
                    })
            }
            Self::Virtual {
                modified,
                observe_modified,
                ..
            } => {
                observe_modified(*modified);
                Ok(*modified)
            }
        }
    }
    #[cfg(unix)]
    pub fn dev(&self) -> u64 {
        use std::os::unix::fs::MetadataExt;
        match self {
            Self::Native(m) => m.dev(),
            Self::Virtual { .. } => 0,
        }
    }
    #[cfg(unix)]
    pub fn ino(&self) -> u64 {
        use std::os::unix::fs::MetadataExt;
        match self {
            Self::Native(m) => m.ino(),
            Self::Virtual { identity, .. } => *identity,
        }
    }
}

/// Narrow resource/environment boundary. A browser host supplies immutable
/// project/bundle bytes and explicitly stages output instead of using std::fs.
pub trait Host {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn open_physical(
        &self,
        _path: &Path,
    ) -> io::Result<Option<Box<dyn crate::language::input_frames::PhysicalStream>>> {
        Ok(None)
    }
    /// Physical-source bookkeeping is distinct from explicit file-content queries.
    fn physical_source_bytes(&self, path: &Path) -> io::Result<std::sync::Arc<[u8]>> {
        self.read(path).map(Into::into)
    }
    fn input_bytes(&self, _path: &Path) -> io::Result<Option<std::sync::Arc<[u8]>>> {
        Ok(None)
    }
    fn open_read(&self, path: &Path) -> io::Result<Reader> {
        Ok(Box::new(Cursor::new(self.read(path)?)))
    }
    fn metadata(&self, path: &Path) -> io::Result<FileMetadata>;
    fn is_file(&self, path: &Path) -> bool {
        self.metadata(path).is_ok_and(|metadata| metadata.is_file())
    }
    fn is_dir(&self, path: &Path) -> bool {
        self.metadata(path).is_ok_and(|metadata| metadata.is_dir())
    }
    /// An authoritative host may prove absence throughout its file namespace.
    /// Native filesystems keep ordinary probing; callers still populate their
    /// directory caches and consult database matches in the original order.
    fn file_name_absent(&self, _name: &Path) -> bool {
        false
    }
    /// Complete normalized paths that may contain this basename, if the host
    /// owns an authoritative namespace. None retains ordinary filesystem probes.
    fn file_name_candidates(&self, _name: &Path) -> Option<std::sync::Arc<Vec<PathBuf>>> {
        None
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
    /// Recursive search consumes only non-hidden child directories. Hosts can
    /// observe that result separately from a complete directory listing.
    fn search_subdirectories(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        Ok(self
            .read_dir(path)?
            .into_iter()
            .filter(|child| {
                !child
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
                    && self.metadata(child).is_ok_and(|metadata| metadata.is_dir())
            })
            .collect())
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
    fn current_dir(&self) -> io::Result<PathBuf>;
    fn variable(&self, name: &str) -> Option<OsString>;
    fn open_output(&self, path: &Path, append: bool) -> io::Result<Writer>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    fn observe_time(&self) {}
    fn now(&self) -> SystemTime;
    fn monotonic_nanos(&self) -> u128;
    fn local_date(&self, time: SystemTime) -> DateTime<FixedOffset>;
    fn local_timestamp(&self, time: DateTime<Utc>) -> DateTime<FixedOffset> {
        time.with_timezone(&Local).fixed_offset()
    }
    fn native_capabilities(&self) -> bool {
        false
    }
    fn console(&self, error: bool, bytes: &[u8]) -> io::Result<()>;
}

pub struct NativeHost;
impl Host for NativeHost {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Reader> {
        Ok(Box::new(File::open(path)?))
    }
    fn metadata(&self, path: &Path) -> io::Result<FileMetadata> {
        Ok(FileMetadata::Native(path.metadata()?))
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        // Match the existing search's handling of unreadable individual entries.
        Ok(path
            .read_dir()?
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .collect())
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }
    fn current_dir(&self) -> io::Result<PathBuf> {
        std::env::current_dir()
    }
    fn variable(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }
    fn open_output(&self, path: &Path, append: bool) -> io::Result<Writer> {
        Ok(Box::new(
            OpenOptions::new()
                .write(true)
                .create(true)
                .append(append)
                .truncate(!append)
                .open(path)?,
        ))
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
    fn monotonic_nanos(&self) -> u128 {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_nanos()
    }
    fn local_date(&self, time: SystemTime) -> DateTime<FixedOffset> {
        let date: DateTime<Local> = time.into();
        date.fixed_offset()
    }
    fn native_capabilities(&self) -> bool {
        true
    }
    fn console(&self, error: bool, bytes: &[u8]) -> io::Result<()> {
        if error {
            std::io::stderr().write_all(bytes)
        } else {
            std::io::stdout().write_all(bytes)
        }
    }
}

thread_local! { static ACTIVE: RefCell<Vec<HostHandle>> = RefCell::new(Vec::new()); }
pub(crate) struct HostGuard(std::marker::PhantomData<Rc<()>>);
impl Drop for HostGuard {
    fn drop(&mut self) {
        ACTIVE.with(|active| {
            active.borrow_mut().pop();
        });
    }
}
pub(crate) fn enter(host: HostHandle) -> HostGuard {
    ACTIVE.with(|active| active.borrow_mut().push(host));
    HostGuard(std::marker::PhantomData)
}
pub fn current() -> HostHandle {
    ACTIVE
        .with(|active| active.borrow().last().cloned())
        .unwrap_or_else(|| Rc::new(NativeHost))
}
pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    #[cfg(feature = "latency-diagnostics")]
    let _diag = crate::latency_diagnostics::phase(
        "resource_read",
        &path.as_ref().display().to_string(),
        None,
    );
    let result = current().read(path.as_ref());
    #[cfg(feature = "latency-diagnostics")]
    if let Ok(bytes) = &result {
        crate::latency_diagnostics::count("read_returned_vec_bytes", bytes.len());
    }
    #[cfg(feature = "latency-diagnostics")]
    if crate::output_diagnostics::detailed() {
        crate::output_diagnostics::event(
            "resource_read",
            format!(
                "{}|{:?}",
                path.as_ref().display(),
                result
                    .as_ref()
                    .map(|b| (b.len(), crate::output_diagnostics::hash(b)))
            ),
        );
    }
    result
}
/// Observe the current resource normally while retaining immutable host bytes.
/// Native and custom hosts without immutable storage still perform a fresh read.
pub(crate) fn read_shared(path: &Path) -> io::Result<std::sync::Arc<[u8]>> {
    let host = current();
    match host.input_bytes(path)? {
        Some(bytes) => Ok(bytes),
        None => host.read(path).map(Into::into),
    }
}
pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    String::from_utf8(read(path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
pub fn open_read(path: impl AsRef<Path>) -> io::Result<Reader> {
    current().open_read(path.as_ref())
}
pub fn metadata(path: impl AsRef<Path>) -> io::Result<FileMetadata> {
    #[cfg(feature = "latency-diagnostics")]
    crate::latency_diagnostics::count("metadata_checks", 1);
    current().metadata(path.as_ref())
}
pub fn file_name_absent(name: &Path) -> bool {
    #[cfg(feature = "latency-diagnostics")]
    crate::latency_diagnostics::count("basename_absence_checks", 1);
    current().file_name_absent(name)
}
pub fn file_name_candidates(name: &Path) -> Option<std::sync::Arc<Vec<PathBuf>>> {
    #[cfg(feature = "latency-diagnostics")]
    crate::latency_diagnostics::count("basename_candidate_queries", 1);
    current().file_name_candidates(name)
}
pub fn is_file(path: impl AsRef<Path>) -> bool {
    #[cfg(feature = "latency-diagnostics")]
    let _diag =
        crate::latency_diagnostics::phase("file_check", &path.as_ref().display().to_string(), None);
    let result = current().is_file(path.as_ref());
    #[cfg(feature = "latency-diagnostics")]
    if crate::output_diagnostics::detailed() {
        crate::output_diagnostics::event(
            "file_check",
            format!("{}|exists={result}", path.as_ref().display()),
        );
    }
    result
}
pub fn is_dir(path: impl AsRef<Path>) -> bool {
    #[cfg(feature = "latency-diagnostics")]
    crate::latency_diagnostics::count("is_dir_checks", 1);
    current().is_dir(path.as_ref())
}
pub fn read_dir(path: impl AsRef<Path>) -> io::Result<Vec<PathBuf>> {
    current().read_dir(path.as_ref())
}
pub fn search_subdirectories(path: impl AsRef<Path>) -> io::Result<Vec<PathBuf>> {
    #[cfg(feature = "latency-diagnostics")]
    let _diag = crate::latency_diagnostics::enabled().then(|| {
        crate::latency_diagnostics::phase(
            "directory_search",
            &path.as_ref().display().to_string(),
            None,
        )
    });
    let result = current().search_subdirectories(path.as_ref());
    #[cfg(feature = "latency-diagnostics")]
    if let Ok(paths) = &result {
        crate::latency_diagnostics::count("child_directory_paths", paths.len());
    }
    result
}
pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    current().canonicalize(path.as_ref())
}
pub fn current_dir() -> io::Result<PathBuf> {
    current().current_dir()
}
pub fn var_os(name: impl AsRef<str>) -> Option<OsString> {
    current().variable(name.as_ref())
}
pub fn var(name: impl AsRef<str>) -> std::result::Result<String, std::env::VarError> {
    var_os(name)
        .ok_or(std::env::VarError::NotPresent)?
        .into_string()
        .map_err(std::env::VarError::NotUnicode)
}
pub fn open_output(path: impl AsRef<Path>, append: bool) -> io::Result<Writer> {
    current().open_output(path.as_ref(), append)
}
pub fn write(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    let mut output = open_output(path, false)?;
    output.write_all(bytes.as_ref())?;
    output.flush()
}
pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    current().rename(from.as_ref(), to.as_ref())
}
pub fn remove_file(path: impl AsRef<Path>) -> io::Result<()> {
    current().remove_file(path.as_ref())
}
pub fn now() -> SystemTime {
    current().now()
}
pub fn observe_time() {
    current().observe_time();
}
pub fn monotonic_nanos() -> u128 {
    current().monotonic_nanos()
}
pub fn local_date(time: SystemTime) -> DateTime<FixedOffset> {
    current().local_date(time)
}
pub fn local_timestamp(time: DateTime<Utc>) -> DateTime<FixedOffset> {
    current().local_timestamp(time)
}
pub fn native_capabilities() -> bool {
    current().native_capabilities()
}
pub fn is_absolute(path: &Path) -> bool {
    if native_capabilities() {
        path.is_absolute()
    } else {
        path.has_root()
    }
}
pub struct ConsoleWriter {
    pub error: bool,
}
impl Write for ConsoleWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        current().console(self.error, bytes)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub fn console_line(error: bool, text: impl AsRef<str>) {
    let _ = writeln!(ConsoleWriter { error }, "{}", text.as_ref());
}

/// Explicit clock values supplied by the host; no Wasm SystemTime::now call.
pub struct FixedClock {
    pub time: SystemTime,
    pub offset: FixedOffset,
}
impl FixedClock {
    pub fn new(seconds: u64, nanos: u32, offset_seconds: i32) -> io::Result<Self> {
        let offset = FixedOffset::east_opt(offset_seconds)
            .ok_or_else(|| io::Error::other("Invalid host UTC offset"))?;
        if nanos >= 1_000_000_000 {
            return Err(io::Error::other("Invalid host clock nanoseconds"));
        }
        let time = UNIX_EPOCH
            .checked_add(Duration::new(seconds, nanos))
            .ok_or_else(|| io::Error::other("Host clock overflow"))?;
        Ok(Self { time, offset })
    }
    pub fn local_date(&self, time: SystemTime) -> DateTime<FixedOffset> {
        let date: DateTime<Utc> = time.into();
        date.with_timezone(&self.offset.fix())
    }
}
