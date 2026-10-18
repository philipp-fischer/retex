//! Bounded receipts for already-open VirtualHost writers. No open, close, seek,
//! rename or main-source write is represented. A cache hit uses the current
//! handle; a retained receipt never owns an output handle or transaction.
use super::host::{ReuseWriterState, Writer};
use std::{collections::BTreeMap, rc::Rc, sync::Arc};
const BUDGET: usize = 512 * 1024;
#[derive(Clone, Default)]
pub(crate) struct Journal {
    pub bindings: Vec<ReuseWriterState>,
    pub events: Vec<(u64, usize, Arc<[u8]>)>,
    bytes: usize,
}
impl Journal {
    pub fn record(&mut self, state: ReuseWriterState, bytes: &[u8], order: u64) -> bool {
        if state.stream.is_none() {
            return false;
        }
        if bytes.len() > 64 * 1024
            || state.bytes.capacity() > 64 * 1024
            || state.position > 64 * 1024
            || state.path.as_os_str().len() > 4096
            || self.events.len() >= 128
        {
            return false;
        }
        let known = self
            .bindings
            .iter()
            .position(|old| old.stream == state.stream);
        if known.is_some_and(|index| !Rc::ptr_eq(&self.bindings[index].instance, &state.instance)) {
            return false;
        }
        if known.is_none()
            && (self.bindings.len() >= 16 || self.bindings.iter().any(|old| old.path == state.path))
        {
            return false;
        }
        let extra = bytes.len()
            + 128
            + if known.is_none() {
                state.bytes.capacity() + state.path.as_os_str().len() + 512
            } else {
                0
            };
        let Some(total) = self.bytes.checked_add(extra).filter(|n| *n <= BUDGET) else {
            return false;
        };
        let index = known.unwrap_or(self.bindings.len());
        if known.is_none() {
            self.bindings.push(state);
        }
        self.events.push((order, index, Arc::from(bytes)));
        self.bytes = total;
        true
    }
    pub fn bind(&self, writers: &BTreeMap<u8, Writer>) -> Option<Vec<u8>> {
        let mut keys = Vec::new();
        for expected in &self.bindings {
            let key = expected.stream?;
            let state = writers.get(&key)?.reuse_state()?;
            if !Arc::ptr_eq(&state.owner, &expected.owner)
                || state.path != expected.path
                || state.position != expected.position
                || state.append != expected.append
                || !(Arc::ptr_eq(&state.bytes, &expected.bytes) || state.bytes == expected.bytes)
            {
                return None;
            }
            keys.push(key);
        }
        Some(keys)
    }
    pub fn growth(&self) -> Option<usize> {
        let mut lengths = self
            .bindings
            .iter()
            .map(|b| b.bytes.len())
            .collect::<Vec<_>>();
        let mut positions = self.bindings.iter().map(|b| b.position).collect::<Vec<_>>();
        for (_, index, bytes) in &self.events {
            let start = if self.bindings[*index].append {
                lengths[*index]
            } else {
                positions[*index]
            };
            let end = start.checked_add(bytes.len())?;
            lengths[*index] = lengths[*index].max(end);
            positions[*index] = end;
        }
        lengths
            .iter()
            .zip(&self.bindings)
            .try_fold(0usize, |sum, (length, b)| {
                sum.checked_add(length.checked_sub(b.bytes.len())?)
            })
    }
    pub fn retained_bytes(&self) -> usize {
        self.bytes + 512
    }
}
