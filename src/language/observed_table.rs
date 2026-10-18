//! Conservative read contracts for reusable execution. Deref escape or mutation
//! invalidates the recording; only keyed reads can form granular dependencies.
use crate::core::persistence::SharedTable;
use serde::{Deserialize, Serialize};
use std::{
    borrow::Borrow,
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    ops::{Deref, DerefMut},
};

const READ_BYTES: usize = 4 * 1024 * 1024;
const READ_KEYS: usize = 1024;
pub trait ObservationSize {
    fn observation_size(&self) -> usize;
}
impl ObservationSize for i64 {
    fn observation_size(&self) -> usize {
        8
    }
}
impl ObservationSize for str {
    fn observation_size(&self) -> usize {
        self.len().saturating_add(64)
    }
}
impl ObservationSize for String {
    fn observation_size(&self) -> usize {
        self.len() + 64
    }
}
impl ObservationSize for std::sync::Arc<str> {
    fn observation_size(&self) -> usize {
        self.len() + 64
    }
}
impl ObservationSize for super::records::ValueKey {
    fn observation_size(&self) -> usize {
        self.table.len()
            + 64
            + match &self.index {
                super::records::ValueIndex::Name(n) => n.len(),
                _ => 16,
            }
    }
}
impl ObservationSize for super::records::Quantity {
    fn observation_size(&self) -> usize {
        use super::records::Quantity::*;
        64 + match self {
            Integer(_) => 8,
            Glue(_) => 64,
            Tokens(t) => t.len().saturating_mul(32),
            Shape(s) => s.len().saturating_mul(32),
            Penalties(p) => p.len().saturating_mul(16),
        }
    }
}
impl ObservationSize for std::sync::Arc<super::records::Equivalence> {
    fn observation_size(&self) -> usize {
        use super::records::CommandOperand::*;
        256 + match &self.value {
            Macro(m) => {
                m.body.len().saturating_mul(256)
                    + m.parameters.as_ref().map_or(0, |p| {
                        (p.prefix.len() + p.delimiters.iter().map(Vec::len).sum::<usize>())
                            .saturating_mul(32)
                    })
            }
            Text(s) => s.len(),
            _ => 32,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(
    transparent,
    bound(deserialize = "K: Ord + Deserialize<'de>, V: Deserialize<'de>")
)]
pub struct ObservedTable<K: Ord, V> {
    table: SharedTable<K, V>,
    #[serde(skip)]
    active: Cell<bool>,
    #[serde(skip)]
    reads: RefCell<Option<Option<BTreeMap<K, Option<V>>>>>,
    #[serde(skip)]
    bytes: Cell<usize>,
    #[serde(skip)]
    item_limit: Cell<usize>,
    #[serde(skip)]
    allow_writes: Cell<bool>,
    #[serde(skip)]
    writes: RefCell<BTreeSet<K>>,
    #[serde(skip)]
    external: RefCell<BTreeSet<K>>,
    #[serde(skip)]
    shadowed: RefCell<BTreeSet<K>>,
}
impl<K: Ord, V> Default for ObservedTable<K, V> {
    fn default() -> Self {
        Self {
            table: SharedTable::new(),
            active: Cell::new(false),
            reads: RefCell::new(None),
            bytes: Cell::new(0),
            item_limit: Cell::new(64 * 1024),
            allow_writes: Cell::new(false),
            writes: RefCell::new(BTreeSet::new()),
            external: RefCell::new(BTreeSet::new()),
            shadowed: RefCell::new(BTreeSet::new()),
        }
    }
}
impl<K: Ord + Clone, V: Clone> Clone for ObservedTable<K, V> {
    fn clone(&self) -> Self {
        Self {
            table: self.table.clone(),
            active: Cell::new(false),
            reads: RefCell::new(None),
            bytes: Cell::new(0),
            item_limit: Cell::new(64 * 1024),
            allow_writes: Cell::new(false),
            writes: RefCell::new(BTreeSet::new()),
            external: RefCell::new(BTreeSet::new()),
            shadowed: RefCell::new(BTreeSet::new()),
        }
    }
}
impl<K: Ord, V: PartialEq> PartialEq for ObservedTable<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.table == other.table
    }
}
impl<K: Ord, V: Eq> Eq for ObservedTable<K, V> {}
impl<K: Ord + Clone, V: Clone> ObservedTable<K, V> {
    pub fn begin_reads(&self) {
        self.active.set(true);
        self.bytes.set(0);
        self.item_limit.set(64 * 1024);
        self.allow_writes.set(false);
        self.writes.borrow_mut().clear();
        self.external.borrow_mut().clear();
        self.shadowed.borrow_mut().clear();
        *self.reads.borrow_mut() = Some(Some(BTreeMap::new()));
    }
    pub fn begin_read_writes(&self) {
        self.begin_reads();
        self.allow_writes.set(true);
        self.item_limit.set(1024 * 1024);
    }
    pub fn finish_read_writes(&self) -> Option<(BTreeMap<K, Option<V>>, BTreeSet<K>, BTreeSet<K>)> {
        let reads = self.finish_reads()?;
        Some((
            reads,
            std::mem::take(&mut *self.writes.borrow_mut()),
            std::mem::take(&mut *self.external.borrow_mut()),
        ))
    }
    pub fn cancel_reads(&self) {
        self.active.set(false);
        *self.reads.borrow_mut() = None;
    }
    pub fn finish_reads(&self) -> Option<BTreeMap<K, Option<V>>> {
        self.active.set(false);
        self.reads.borrow_mut().take().flatten()
    }
    pub fn recording(&self) -> bool {
        self.active.get()
    }
    pub fn get<Q: Ord + ToOwned + ObservationSize + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q> + From<Q::Owned> + ObservationSize,
        V: ObservationSize,
    {
        self.read(key, true)
    }
    pub(crate) fn get_for_assignment<Q: Ord + ToOwned + ObservationSize + ?Sized>(
        &self,
        key: &Q,
    ) -> Option<&V>
    where
        K: Borrow<Q> + From<Q::Owned> + ObservationSize,
        V: ObservationSize,
    {
        self.read(key, false)
    }
    fn read<Q: Ord + ToOwned + ObservationSize + ?Sized>(
        &self,
        key: &Q,
        semantic: bool,
    ) -> Option<&V>
    where
        K: Borrow<Q> + From<Q::Owned> + ObservationSize,
        V: ObservationSize,
    {
        let value = self.table.get(key);
        if self.active.get() {
            if semantic && self.allow_writes.get() && !self.shadowed.borrow().contains(key) {
                self.external.borrow_mut().insert(K::from(key.to_owned()));
            }
            let mut state = self.reads.borrow_mut();
            if let Some(Some(reads)) = state.as_mut() {
                if !reads.contains_key(key) {
                    let size = key
                        .observation_size()
                        .saturating_add(value.map_or(0, ObservationSize::observation_size));
                    if reads.len() >= READ_KEYS
                        || size > self.item_limit.get()
                        || self.bytes.get().saturating_add(size) > READ_BYTES
                    {
                        *state = Some(None);
                        self.active.set(false);
                    } else {
                        self.bytes.set(self.bytes.get() + size);
                        reads.insert(K::from(key.to_owned()), value.cloned());
                    }
                }
            }
        }
        value
    }
    fn observe_write(&self, key: &K)
    where
        K: ObservationSize,
        V: ObservationSize,
    {
        if !self.active.get() {
            return;
        }
        if !self.allow_writes.get() {
            self.unsupported();
            return;
        }
        // Snapshot the pre-segment value before the first write. Subsequent
        // reads of an overwritten binding retain that initial dependency.
        self.get_for_assignment(key);
        if self.active.get() {
            self.writes.borrow_mut().insert(key.clone());
            self.shadowed.borrow_mut().insert(key.clone());
        }
    }
    /// Restoration can expose an entry binding again. Conservatively mark
    /// subsequent semantic reads as external, even when an enclosing segment
    /// assignment originally produced this saved value.
    pub(crate) fn restored<Q: Ord + ?Sized>(&self, key: &Q)
    where
        K: Borrow<Q>,
    {
        if self.active.get() && self.allow_writes.get() {
            self.shadowed.borrow_mut().remove(key);
        }
    }
    pub(crate) fn mark_assignment(&self, key: &K)
    where
        K: ObservationSize,
        V: ObservationSize,
    {
        self.observe_write(key);
    }
    pub fn insert(&mut self, key: K, value: V) -> Option<V>
    where
        K: ObservationSize,
        V: ObservationSize,
    {
        self.unsupported();
        self.table.insert(key, value)
    }
    pub(crate) fn insert_assigned(&mut self, key: K, value: V) -> Option<V>
    where
        K: ObservationSize,
        V: ObservationSize,
    {
        self.observe_write(&key);
        self.table.insert(key, value)
    }
    pub fn remove<Q: Ord + ObservationSize + ToOwned + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q> + From<Q::Owned> + ObservationSize,
        V: ObservationSize,
    {
        self.unsupported();
        self.table.remove(key)
    }
    pub(crate) fn remove_assigned<Q: Ord + ObservationSize + ToOwned + ?Sized>(
        &mut self,
        key: &Q,
    ) -> Option<V>
    where
        K: Borrow<Q> + From<Q::Owned> + ObservationSize,
        V: ObservationSize,
    {
        if self.active.get() {
            self.observe_write(&K::from(key.to_owned()));
        }
        self.table.remove(key)
    }
    pub(crate) fn raw(&self) -> &SharedTable<K, V> {
        &self.table
    }
}
impl<K: Ord, V> ObservedTable<K, V> {
    fn unsupported(&self) {
        if !self.active.get() {
            return;
        }
        self.active.set(false);
        let mut reads = self.reads.borrow_mut();
        if reads.is_some() {
            *reads = Some(None);
        }
    }
}
impl<K: Ord, V> Deref for ObservedTable<K, V> {
    type Target = SharedTable<K, V>;
    fn deref(&self) -> &Self::Target {
        self.unsupported();
        &self.table
    }
}
impl<K: Ord + Clone, V: Clone> DerefMut for ObservedTable<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.unsupported();
        &mut self.table
    }
}
impl<K: Ord, V> From<SharedTable<K, V>> for ObservedTable<K, V> {
    fn from(table: SharedTable<K, V>) -> Self {
        Self {
            table,
            active: Cell::new(false),
            reads: RefCell::new(None),
            bytes: Cell::new(0),
            item_limit: Cell::new(64 * 1024),
            allow_writes: Cell::new(false),
            writes: RefCell::new(BTreeSet::new()),
            external: RefCell::new(BTreeSet::new()),
            shadowed: RefCell::new(BTreeSet::new()),
        }
    }
}
impl<K: Ord, V> FromIterator<(K, V)> for ObservedTable<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(items: I) -> Self {
        SharedTable::from_iter(items).into()
    }
}
impl<'a, K: Ord, V> IntoIterator for &'a ObservedTable<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = std::collections::btree_map::Iter<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.unsupported();
        self.table.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_reads_mutation_and_escape_are_conservative() {
        let mut t: ObservedTable<String, i64> = [("a".into(), 1)].into_iter().collect();
        t.begin_reads();
        assert_eq!(t.get("a"), Some(&1));
        assert_eq!(t.get("missing"), None);
        assert_eq!(
            t.finish_reads().unwrap(),
            BTreeMap::from([("a".into(), Some(1)), ("missing".into(), None)])
        );
        t.begin_reads();
        let _ = t.len();
        assert!(t.finish_reads().is_none());
        t.begin_reads();
        t.insert("b".into(), 2);
        assert!(t.finish_reads().is_none());
        t.begin_reads();
        let copy = t.clone();
        assert!(!copy.recording());
        assert!(t.recording());
        assert_eq!(
            bincode::serialize(&t).unwrap(),
            bincode::serialize(t.raw()).unwrap()
        );
    }
    #[test]
    fn read_write_contract_preserves_initial_values_and_rejects_escape() {
        let mut t: ObservedTable<String, i64> = [("counter".into(), 7)].into_iter().collect();
        t.begin_read_writes();
        let n = *t.get("counter").unwrap();
        t.insert_assigned("counter".into(), n + 1);
        assert_eq!(t.get("counter"), Some(&8));
        t.insert_assigned("temporary".into(), 9);
        assert_eq!(t.get("temporary"), Some(&9));
        t.remove_assigned("temporary");
        let (reads, writes, external) = t.finish_read_writes().unwrap();
        assert_eq!(external, BTreeSet::from(["counter".into()]));
        assert_eq!(reads["counter"], Some(7));
        assert_eq!(reads["temporary"], None);
        assert_eq!(
            writes,
            BTreeSet::from(["counter".into(), "temporary".into()])
        );
        t.begin_read_writes();
        let _ = t.get_mut("counter");
        assert!(t.finish_read_writes().is_none());
    }
    #[test]
    fn raw_write_after_typed_write_still_rejects_the_contract() {
        let mut t: ObservedTable<String, i64> = Default::default();
        t.begin_read_writes();
        t.insert_assigned("counter".into(), 7);
        t.insert("counter".into(), 8);
        assert!(t.finish_read_writes().is_none());
    }
}
