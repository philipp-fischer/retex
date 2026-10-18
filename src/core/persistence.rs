//! Component-owned typed initialized-state export (`core/persistence.py`).
use super::errors::Result;
use serde::Deserialize;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    ops::{Deref, DerefMut},
    sync::Arc,
};

/// Immutable table roots for live rollback. Capturing a root is constant time;
/// the first subsequent mutation detaches only that table. Ordered iteration
/// and the serialized map representation are identical to BTreeMap. This is a
/// deliberately coarse initial COW table, not a full engine checkpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SharedTable<K: Ord, V>(Arc<BTreeMap<K, V>>);
impl<K: Ord, V> Default for SharedTable<K, V> {
    fn default() -> Self {
        Self(Arc::new(BTreeMap::new()))
    }
}
impl<K: Ord, V> SharedTable<K, V> {
    pub fn new() -> Self {
        Self::default()
    }
    pub(crate) fn storage_address(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
    pub(crate) fn weak_storage(&self) -> std::sync::Weak<BTreeMap<K, V>> {
        Arc::downgrade(&self.0)
    }
    pub fn shares_storage_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl<K: Ord, V> Deref for SharedTable<K, V> {
    type Target = BTreeMap<K, V>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl<K: Ord + Clone, V: Clone> DerefMut for SharedTable<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        #[cfg(feature = "latency-diagnostics")]
        let _diag = (crate::latency_diagnostics::enabled() || crate::output_diagnostics::enabled())
            .then(|| {
                if Arc::strong_count(&self.0) > 1 {
                    let guard = crate::latency_diagnostics::phase(
                        "shared_table_clone",
                        std::any::type_name::<Self>(),
                        None,
                    );
                    crate::latency_diagnostics::count("entries_cloned", self.0.len());
                    crate::output_diagnostics::count("table_entries_cloned", self.0.len());
                    crate::output_diagnostics::event(
                        "table_copy",
                        format!(
                            "{}|entries={}|trigger={}",
                            std::any::type_name::<Self>(),
                            self.0.len(),
                            crate::output_diagnostics::current_trigger()
                        ),
                    );
                    crate::latency_diagnostics::count(
                        "shallow_entry_bytes",
                        self.0.len() * (std::mem::size_of::<K>() + std::mem::size_of::<V>()),
                    );
                    Some(guard)
                } else {
                    if Arc::weak_count(&self.0) > 0 {
                        crate::latency_diagnostics::count("shared_table_weak_detaches", 1);
                        crate::output_diagnostics::count("weak_detaches", 1);
                        crate::output_diagnostics::event(
                            "weak_detach",
                            format!(
                                "{}|trigger={}",
                                std::any::type_name::<Self>(),
                                crate::output_diagnostics::current_trigger()
                            ),
                        );
                    }
                    None
                }
            });
        Arc::make_mut(&mut self.0)
    }
}
impl<K: Ord, V> From<BTreeMap<K, V>> for SharedTable<K, V> {
    fn from(map: BTreeMap<K, V>) -> Self {
        Self(Arc::new(map))
    }
}
impl<K: Ord, V> FromIterator<(K, V)> for SharedTable<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(items: I) -> Self {
        BTreeMap::from_iter(items).into()
    }
}
impl<'a, K: Ord, V> IntoIterator for &'a SharedTable<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = std::collections::btree_map::Iter<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Transient execution state is excluded by each component's snapshot type.
pub trait PersistentComponent {
    type State: Clone + Serialize + DeserializeOwned;
    fn dump_state(&self) -> Self::State;
    fn restore_state(&mut self, state: Self::State) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_tables_detach_and_preserve_initialized_map_bytes() {
        let original = BTreeMap::from([(2u32, "second".to_string()), (1, "first".to_string())]);
        let mut live: SharedTable<_, _> = original.clone().into();
        let snapshot = live.clone();
        assert!(live.shares_storage_with(&snapshot));
        assert_eq!(
            bincode::serialize(&live).unwrap(),
            bincode::serialize(&original).unwrap()
        );
        let legacy: SharedTable<u32, String> =
            bincode::deserialize(&bincode::serialize(&original).unwrap()).unwrap();
        assert_eq!(legacy, snapshot);
        live.insert(1, "edited".into());
        assert!(!live.shares_storage_with(&snapshot));
        assert_eq!(snapshot.get(&1).unwrap(), "first");
    }
}
