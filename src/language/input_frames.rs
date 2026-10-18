//! Owned source streams and full cursor suspension (`input_frames.py`).
use super::input::{CurrentToken, InputProcessor};
use super::input_adapters::PseudoInput;
use super::records::Quantity;
use super::state::Environment;
use crate::core::capacities::{reached, read_size};
use crate::core::characters::LanguageProfile;
use crate::core::enums::InputStateKind;
use crate::core::errors::{Error, ErrorKind, Result};
use crate::core::source::SourceSpan;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::Arc;

pub fn end_of_input() -> Error {
    Error::new(ErrorKind::Eof, "No more input")
}
pub trait PhysicalStream {
    fn readline(
        &mut self,
        limit: Option<usize>,
        profile: LanguageProfile,
    ) -> Result<Option<String>>;
    fn is_pseudo(&self) -> bool {
        false
    }
    fn close(&mut self);
    fn fork(&self) -> Option<Box<dyn PhysicalStream>> {
        None
    }
    fn byte_position(&self) -> Option<usize> {
        None
    }
    fn buffered_end(&self) -> Option<usize> {
        self.byte_position()
    }
    fn physical_path(&self) -> Option<&Path> {
        None
    }
    fn observe_explicit_read(&self) -> Result<()> {
        Ok(())
    }
    fn catalog_bytes(&self) -> Option<Arc<[u8]>> {
        None
    }
    fn seek_catalog(&mut self, _position: usize) -> Result<()> {
        Err(Error::value("Input is not seekable catalog material"))
    }
    fn replace_bytes(&mut self, _bytes: Arc<[u8]>) -> Result<()> {
        Err(Error::value("Input stream cannot be rebound"))
    }
}
pub struct SourceStream<R: BufRead> {
    pub(crate) stream: Option<R>,
}
impl<R: BufRead> SourceStream<R> {
    pub fn new(stream: R) -> Self {
        Self {
            stream: Some(stream),
        }
    }
    fn byte(&mut self) -> Result<Option<u8>> {
        let Some(stream) = &mut self.stream else {
            return Ok(None);
        };
        let buffer = stream.fill_buf()?;
        if buffer.is_empty() {
            return Ok(None);
        }
        let byte = buffer[0];
        stream.consume(1);
        Ok(Some(byte))
    }
    fn optional_lf(&mut self) -> Result<()> {
        if let Some(stream) = &mut self.stream {
            if stream.fill_buf()?.first() == Some(&b'\n') {
                stream.consume(1);
            }
        }
        Ok(())
    }
}
impl<R: BufRead> PhysicalStream for SourceStream<R> {
    fn readline(
        &mut self,
        limit: Option<usize>,
        profile: LanguageProfile,
    ) -> Result<Option<String>> {
        let mut line = String::new();
        let mut count = 0;
        let mut bytes_seen = 0;
        while limit.is_none_or(|limit| count < limit) {
            let Some(first) = self.byte()? else {
                break;
            };
            if first == b'\r' {
                self.optional_lf()?;
                line.push('\n');
                break;
            }
            if first == b'\n' {
                line.push('\n');
                break;
            }
            let character = if profile == LanguageProfile::Pdftex {
                char::from(first)
            } else {
                let length = match first {
                    0..=127 => 1,
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf4 => 4,
                    _ => 0,
                };
                let mut bytes = [0; 4];
                bytes[0] = first;
                let mut truncated = false;
                for index in 1..length {
                    match self.byte()? {
                        Some(byte) => bytes[index] = byte,
                        None => {
                            truncated = true;
                            break;
                        }
                    }
                }
                let decoded = if length != 0 && !truncated {
                    std::str::from_utf8(&bytes[..length])
                        .ok()
                        .and_then(|text| text.chars().next())
                } else {
                    None
                };
                let Some(character) = decoded else {
                    let mut error = Error::value("Invalid UTF-8 source input");
                    let mut source = SourceSpan::new("", 0, count + 1);
                    source.byte_column = std::num::NonZeroUsize::new(bytes_seen + 1);
                    error.source = Some(Arc::new(source));
                    return Err(error);
                };
                character
            };
            bytes_seen += if profile == LanguageProfile::Unicode {
                character.len_utf8()
            } else {
                1
            };
            line.push(character);
            count += 1;
        }
        if line.is_empty() {
            Ok(None)
        } else {
            Ok(Some(line))
        }
    }
    fn close(&mut self) {
        self.stream.take();
    }
}
/// Catalog inputs own immutable physical bytes and an independent byte cursor.
/// Decoding uses exactly the existing SourceStream implementation.
pub struct CatalogStream(SourceStream<std::io::Cursor<Arc<[u8]>>>);
impl CatalogStream {
    pub fn new(bytes: Arc<[u8]>) -> Self {
        Self(SourceStream::new(std::io::Cursor::new(bytes)))
    }
}
impl PhysicalStream for CatalogStream {
    fn readline(
        &mut self,
        limit: Option<usize>,
        profile: LanguageProfile,
    ) -> Result<Option<String>> {
        self.0.readline(limit, profile)
    }
    fn close(&mut self) {
        self.0.close();
    }
    fn fork(&self) -> Option<Box<dyn PhysicalStream>> {
        Some(Box::new(Self(SourceStream {
            stream: self.0.stream.clone(),
        })))
    }
    fn byte_position(&self) -> Option<usize> {
        self.0.stream.as_ref().map(|s| s.position() as usize)
    }
    fn catalog_bytes(&self) -> Option<Arc<[u8]>> {
        self.0.stream.as_ref().map(|s| s.get_ref().clone())
    }
    fn seek_catalog(&mut self, position: usize) -> Result<()> {
        let stream = self
            .0
            .stream
            .as_mut()
            .ok_or_else(|| Error::value("Closed input"))?;
        if position > stream.get_ref().len() {
            return Err(Error::value("Input seek past end"));
        }
        stream.set_position(position as u64);
        Ok(())
    }
    fn replace_bytes(&mut self, bytes: Arc<[u8]>) -> Result<()> {
        let position = self
            .byte_position()
            .ok_or_else(|| Error::value("Closed input stream"))?;
        if position > bytes.len() {
            return Err(Error::value("Checkpoint past new input"));
        }
        let mut stream = std::io::Cursor::new(bytes);
        stream.set_position(position as u64);
        self.0.stream = Some(stream);
        Ok(())
    }
}
pub fn open_physical(path: &Path) -> Result<Box<dyn PhysicalStream>> {
    let host = crate::resources::host::current();
    if let Some(stream) = host.open_physical(path)? {
        Ok(stream)
    } else if let Some(bytes) = host.input_bytes(path)? {
        Ok(Box::new(CatalogStream::new(bytes)))
    } else {
        Ok(Box::new(SourceStream::new(BufReader::new(
            crate::resources::host::open_read(path)?,
        ))))
    }
}
impl PhysicalStream for PseudoInput {
    fn readline(
        &mut self,
        limit: Option<usize>,
        _profile: LanguageProfile,
    ) -> Result<Option<String>> {
        Ok(PseudoInput::readline(self, limit))
    }
    fn is_pseudo(&self) -> bool {
        true
    }
    fn close(&mut self) {
        PseudoInput::close(self);
    }
}
impl InputProcessor {
    pub fn move_to_next_line(&mut self, env: &mut Environment) -> Result<bool> {
        if self.read_line_only {
            return Err(end_of_input());
        }
        if self.cursor.name.is_none() {
            return Ok(false);
        }
        if self.end_input_requested || !self.input_ln(env)? {
            let hook = match env
                .values
                .get(&super::records::ValueKey::parameter("everyeof"))
            {
                Some(Quantity::Tokens(hook)) => Some(hook.clone()),
                _ => None,
            };
            if !self.end_input_requested && !self.eof_seen.contains(&self.cursor.index) {
                if let Some(hook) = hook.filter(|hook| !hook.is_empty()) {
                    self.eof_seen.insert(self.cursor.index);
                    self.cursor.line += 1;
                    self.cursor.loc = 0;
                    self.cursor.limit = -1;
                    self.begin_tokens(hook, self.source(), None)?;
                    return Ok(false);
                }
            }
            return self.end_file_reading(env);
        }
        self.cursor.line += 1;
        self.cursor.loc = 0;
        Ok(true)
    }
    pub fn input_ln(&mut self, env: &Environment) -> Result<bool> {
        let available = read_size(self.capacities.buf_size, self.cursor.buffer_base)?;
        let Some(stream) = self
            .input_file
            .get_mut(self.cursor.index)
            .and_then(Option::as_mut)
        else {
            return Ok(false);
        };
        let pseudo = stream.is_pseudo();
        let line = match stream.readline(available, self.profile) {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(false),
            Err(mut error) => {
                if error.message == "Invalid UTF-8 source input" {
                    let mut source = error
                        .source
                        .as_deref()
                        .cloned()
                        .unwrap_or_else(|| SourceSpan::new("", 0, 1));
                    source.file = self.cursor.name.clone().unwrap_or_default().into();
                    source.line = self.cursor.line + 1;
                    self.cursor.source = Some(Arc::new(source));
                    error.source = self.source();
                }
                return Err(error);
            }
        };
        let line = if pseudo {
            line.as_str()
        } else {
            line.trim_end_matches(['\n', '\r'])
        };
        let length = line.chars().count();
        if reached(self.capacities.buf_size, length + self.cursor.buffer_base) {
            return Err(Error::runtime("Buffer overflow")
                .with_issue(crate::core::recovery::Issue::Capacity));
        }
        self.max_buf_stack = self.max_buf_stack.max(self.cursor.buffer_base + length);
        let mut line = line.trim_end_matches(' ');
        let mut byte_start = 1;
        if self.profile == LanguageProfile::Unicode && self.cursor.line == 0 {
            if let Some(stripped) = line.strip_prefix('\u{feff}') {
                line = stripped;
                byte_start += 3;
            }
        }
        let mut buffer = Vec::new();
        let mut byte_columns = vec![byte_start];
        for character in line.chars() {
            if self.profile == LanguageProfile::Pdftex && character as u32 > 255 {
                return Err(Error::value(format!(
                    "Unsupported input character U+{:04X}; input tokens are 8-bit",
                    character as u32
                )));
            }
            buffer.push(character as u32);
            byte_columns.push(
                byte_columns.last().copied().unwrap()
                    + if self.profile == LanguageProfile::Unicode {
                        character.len_utf8()
                    } else {
                        1
                    },
            );
        }
        self.cursor.last = buffer.len();
        let columns = (1..=buffer.len() + 1).collect();
        buffer.push(0);
        let endline = Self::parameter(env, "endlinechar", 13);
        if self.profile.accepts(endline) {
            buffer[self.cursor.last] = endline as u32;
            self.cursor.limit = self.cursor.last as isize;
        } else {
            self.cursor.limit = self.cursor.last as isize - 1;
        }
        self.cursor.buffer = Some(Arc::new(buffer));
        self.cursor.columns = Some(Arc::new(columns));
        self.cursor.byte_columns = Some(Arc::new(byte_columns));
        Ok(true)
    }
    pub fn end_file_reading(&mut self, env: &Environment) -> Result<bool> {
        self.check_scanner_validity(env, true)?;
        if let Some(stream) = self
            .input_file
            .get_mut(self.cursor.index)
            .and_then(Option::take)
            .as_mut()
        {
            stream.close();
        }
        self.end_input_requested = false;
        self.eof_seen.remove(&self.cursor.index);
        self.in_open = self.in_open.saturating_sub(1);
        if !self.input_stack.is_empty() {
            self.pop_input_state();
            if self.in_open != 0 || self.cursor.state == InputStateKind::TokenList {
                return Ok(false);
            }
        }
        self.current = CurrentToken::default();
        Err(end_of_input())
    }
    pub fn request_end_input(&mut self) {
        self.end_input_requested = true;
    }
    pub fn push_input_state(&mut self) -> Result<()> {
        if reached(self.capacities.stack_size, self.input_stack.len()) {
            return Err(Error::runtime("Input stack overflow")
                .with_issue(crate::core::recovery::Issue::Capacity));
        }
        self.input_stack.push(self.cursor.clone());
        self.peak_frame_depth = self.peak_frame_depth.max(self.input_stack.len());
        Ok(())
    }
    pub fn restore_input_state(&mut self) {
        if let Some(saved) = self.input_stack.last().cloned() {
            self.restore_saved_input_state(saved);
        }
    }
    pub(crate) fn pop_input_state(&mut self) {
        if let Some(saved) = self.input_stack.pop() {
            self.restore_saved_input_state(saved);
        }
    }
    fn restore_saved_input_state(&mut self, mut saved: super::records::InputState) {
        if saved.buffer.is_none() {
            saved.buffer = self.cursor.buffer.take();
        }
        if saved.columns.is_none() {
            saved.columns = self.cursor.columns.take();
        }
        if saved.byte_columns.is_none() {
            saved.byte_columns = saved.columns.clone();
        }
        self.cursor = saved;
    }
    pub fn start_input(
        &mut self,
        env: &Environment,
        filename: &Path,
        directory: Option<&Path>,
        stream: Option<Box<dyn PhysicalStream>>,
    ) -> bool {
        self.last_input_error = None;
        self.last_missing_resource = None;
        let previous_open = self.in_open;
        let mut pushed = false;
        let supplied = stream.is_some();
        let mut opened = stream;
        let result = (|| -> Result<()> {
            let directory = directory.map(Path::to_path_buf).or_else(|| {
                if self.in_open != 0 {
                    self.cursor
                        .name
                        .as_deref()
                        .and_then(|name| Path::new(name).parent())
                        .map(Path::to_path_buf)
                } else {
                    None
                }
            });
            let path = if supplied {
                filename.to_path_buf()
            } else {
                let resource = crate::core::errors::MissingResource {
                    kind: "tex".into(),
                    name: filesystem_bytes(filename.as_os_str()),
                    // Native lookup ignores the source directory. Reporting
                    // it as a supply location would put nested uploads outside
                    // the configured search, causing the same request again.
                    directory: if self.resources.native {
                        None
                    } else {
                        directory
                            .as_deref()
                            .map(|p| filesystem_bytes(p.as_os_str()))
                    },
                };
                let resolved = self.resources.resolve_path(
                    filename,
                    directory.as_deref(),
                    ".tex",
                    true,
                    false,
                );
                match resolved {
                    Ok(Some(path)) => path,
                    result => {
                        let error = match result {
                            Err(error) => error,
                            _ => Error::new(
                                ErrorKind::Io,
                                self.resources.last_error.clone().unwrap_or_else(|| {
                                    format!("Input file not found: {}", filename.display())
                                }),
                            ),
                        };
                        return Err(if self.resources.last_error.is_none() {
                            error.with_missing_resource(resource)
                        } else {
                            error
                        });
                    }
                }
            };
            if reached(self.capacities.max_in_open, self.in_open) {
                return Err(Error::runtime("Too many open files")
                    .with_issue(crate::core::recovery::Issue::Capacity));
            }
            if reached(self.capacities.stack_size, self.input_stack.len()) {
                return Err(Error::runtime("Input stack overflow")
                    .with_issue(crate::core::recovery::Issue::Capacity));
            }
            if opened.is_none() {
                opened = Some(open_physical(&path)?);
            }
            if !supplied {
                let data = crate::resources::host::current().physical_source_bytes(&path)?;
                self.source_resources.insert(
                    crate::resources::paths::resolve_path(&path)
                        .to_string_lossy()
                        .into_owned(),
                    format!("{:x}", Sha256::digest(&data)),
                );
                if self.job_name.is_none() {
                    let stem = path.file_stem().unwrap_or_default();
                    self.job_name = Some(if self.profile == LanguageProfile::Unicode {
                        stem.to_string_lossy().into_owned()
                    } else {
                        filesystem_octets(stem)
                    });
                }
            }
            self.push_input_state()?;
            pushed = true;
            if previous_open != 0 {
                self.cursor.buffer_base += (self.cursor.limit + 1).max(0) as usize;
            } else if supplied {
                self.cursor.buffer_base = 1;
            } else {
                let bytes = filesystem_bytes(filename.as_os_str());
                self.cursor.buffer_base =
                    bytes.len() + 2 + if bytes.contains(&b' ') { 2 } else { 0 };
            }
            if self.in_open == self.input_file.len() {
                self.input_file.push(opened.take());
            } else {
                self.input_file[self.in_open] = opened.take();
            }
            let key = path.to_string_lossy().into_owned();
            let occurrence = self.source_occurrences.entry(key).or_default();
            *occurrence += 1;
            if self.input_occurrences.len() <= self.in_open {
                self.input_occurrences.resize(self.in_open + 1, 0);
            }
            self.input_occurrences[self.in_open] = *occurrence;
            self.eof_seen.remove(&self.in_open);
            self.cursor.index = self.in_open;
            self.in_open += 1;
            self.cursor.name = Some(path.to_string_lossy().into_owned());
            self.cursor.line = 0;
            self.cursor.state = InputStateKind::NewLine;
            self.cursor.loc = 0;
            self.cursor.limit = -1;
            if self.input_ln(env)? {
                self.cursor.line = 1;
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.last_input_error = Some(error.to_string());
            self.last_missing_resource = error.missing_resource.clone();
            if let Some(stream) = &mut opened {
                stream.close();
            }
            for slot in self
                .input_file
                .iter_mut()
                .take(self.in_open)
                .skip(previous_open)
            {
                if let Some(mut stream) = slot.take() {
                    stream.close();
                }
            }
            self.in_open = previous_open;
            if pushed {
                self.pop_input_state();
            }
            false
        } else {
            true
        }
    }
    pub fn close_files_and_terminate(&mut self) {
        for slot in &mut self.input_file {
            if let Some(mut stream) = slot.take() {
                stream.close();
            }
        }
        self.in_open = 0;
        self.input_stack.clear();
        self.cursor.token_list = None;
        self.end_input_requested = false;
        self.log_file.take();
    }
    pub fn get_first_line(&mut self) {
        if self.in_open == 0 {
            self.first = 0;
            self.cursor.last = 0;
            self.cursor.limit = 0;
            self.cursor.loc = 0;
        }
    }
}
pub fn filesystem_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        value.as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        value.to_string_lossy().as_bytes().to_vec()
    }
}
pub fn filesystem_octets(value: &std::ffi::OsStr) -> String {
    filesystem_bytes(value)
        .into_iter()
        .map(char::from)
        .collect()
}
