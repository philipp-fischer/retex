//! Private replay resources and outputs. Never publishes to the project host.
use crate::resources::host::{CheckpointWriter, FileMetadata, Host, HostHandle, Writer};
use chrono::{DateTime, FixedOffset, Utc};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::OsString,
    io::{self, Write},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::SystemTime,
};
const OUTPUT_BUDGET: usize = 16 * 1024 * 1024;
pub(super) struct ReplayHost {
    pub base: HostHandle,
    pub inputs: BTreeMap<PathBuf, Vec<u8>>,
    outputs: Rc<RefCell<BTreeMap<PathBuf, Vec<u8>>>>,
}
impl ReplayHost {
    pub fn new(base: HostHandle, inputs: BTreeMap<PathBuf, Vec<u8>>) -> Self {
        Self {
            base,
            inputs,
            outputs: Default::default(),
        }
    }
    fn key(&self, p: &Path) -> PathBuf {
        self.base.canonicalize(p).unwrap_or_else(|_| {
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                self.base.current_dir().unwrap_or_default().join(p)
            }
        })
    }
    fn bytes(&self, p: &Path) -> Option<Vec<u8>> {
        let p = self.key(p);
        self.inputs
            .get(&p)
            .cloned()
            .or_else(|| self.outputs.borrow().get(&p).cloned())
    }
}
struct Output {
    files: Rc<RefCell<BTreeMap<PathBuf, Vec<u8>>>>,
    path: PathBuf,
    position: usize,
}
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self
            .position
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("Recovery output length overflow"))?;
        let mut files = self.files.borrow_mut();
        let total: usize = files.values().map(Vec::len).sum();
        let old = files.get(&self.path).map_or(0, Vec::len);
        if total.saturating_sub(old).saturating_add(end.max(old)) > OUTPUT_BUDGET {
            return Err(io::Error::other("Recovery output byte budget exceeded"));
        }
        let out = files.entry(self.path.clone()).or_default();
        out.resize(out.len().max(end), 0);
        out[self.position..end].copy_from_slice(bytes);
        self.position = end;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl CheckpointWriter for Output {}
impl Host for ReplayHost {
    fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        self.bytes(p).map(Ok).unwrap_or_else(|| self.base.read(p))
    }
    fn input_bytes(&self, p: &Path) -> io::Result<Option<Arc<[u8]>>> {
        if let Some(b) = self.bytes(p) {
            Ok(Some(b.into()))
        } else {
            self.base.input_bytes(p)
        }
    }
    fn physical_source_bytes(&self, p: &Path) -> io::Result<Arc<[u8]>> {
        if let Some(b) = self.bytes(p) {
            Ok(b.into())
        } else {
            self.base.physical_source_bytes(p)
        }
    }
    fn open_physical(
        &self,
        p: &Path,
    ) -> io::Result<Option<Box<dyn crate::language::input_frames::PhysicalStream>>> {
        if self.bytes(p).is_some() {
            Ok(None)
        } else {
            self.base.open_physical(p)
        }
    }
    fn metadata(&self, p: &Path) -> io::Result<FileMetadata> {
        if let Some(b) = self.bytes(p) {
            return Ok(FileMetadata::Virtual {
                len: b.len() as u64,
                modified: self.base.now().into(),
                directory: false,
                identity: 0,
                observe_modified: Rc::new(|_| {}),
                observe_len: Rc::new(|_| {}),
            });
        }
        self.base.metadata(p)
    }
    fn read_dir(&self, p: &Path) -> io::Result<Vec<PathBuf>> {
        self.base.read_dir(p)
    }
    fn canonicalize(&self, p: &Path) -> io::Result<PathBuf> {
        self.base.canonicalize(p)
    }
    fn current_dir(&self) -> io::Result<PathBuf> {
        self.base.current_dir()
    }
    fn variable(&self, n: &str) -> Option<OsString> {
        self.base.variable(n)
    }
    fn open_output(&self, p: &Path, append: bool) -> io::Result<Writer> {
        let path = self.key(p);
        let mut files = self.outputs.borrow_mut();
        if !append {
            files.insert(path.clone(), Vec::new());
        }
        let position = files.get(&path).map_or(0, Vec::len);
        drop(files);
        Ok(Box::new(Output {
            files: self.outputs.clone(),
            path,
            position,
        }))
    }
    fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
        let bytes = self.read(a)?;
        if bytes.len() > OUTPUT_BUDGET {
            return Err(io::Error::other("Recovery output budget exceeded"));
        }
        let mut files = self.outputs.borrow_mut();
        files.insert(self.key(b), bytes);
        files.remove(&self.key(a));
        Ok(())
    }
    fn remove_file(&self, p: &Path) -> io::Result<()> {
        self.outputs.borrow_mut().remove(&self.key(p));
        Ok(())
    }
    fn observe_time(&self) {
        self.base.observe_time()
    }
    fn now(&self) -> SystemTime {
        self.base.now()
    }
    fn monotonic_nanos(&self) -> u128 {
        self.base.monotonic_nanos()
    }
    fn local_date(&self, t: SystemTime) -> DateTime<FixedOffset> {
        self.base.local_date(t)
    }
    fn local_timestamp(&self, t: DateTime<Utc>) -> DateTime<FixedOffset> {
        self.base.local_timestamp(t)
    }
    fn console(&self, e: bool, b: &[u8]) -> io::Result<()> {
        self.base.console(e, b)
    }
}
