//! Pure lookup memo, guarded by immutable table owner and complete symbol name.
//! Weak owners are dissociated by SharedTable's Arc::make_mut on every mutation;
//! they cannot retain old macro bodies or make another revision mutable.
use super::records::Equivalence;
use crate::core::persistence::SharedTable;
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Weak},
};
type Meanings = BTreeMap<Arc<str>, Arc<Equivalence>>;
const ROOTS: usize = 8;
const ENTRIES: usize = 512;
const BUDGET: usize = 4 * 1024 * 1024;
struct Entry {
    symbol: i64,
    name: String,
    value: Option<Weak<Equivalence>>,
}
struct Root {
    owner: Weak<Meanings>,
    entries: Vec<Option<Entry>>,
    occupied: usize,
    bytes: usize,
    name_bytes: usize,
}
impl Root {
    fn measure_bytes(&self) -> usize {
        // Conservative hash-table allocation allowance and retained Weak headers.
        std::mem::size_of::<Self>()
            + std::mem::size_of::<Meanings>()
            + 2 * std::mem::size_of::<usize>()
            + self.entries.capacity() * std::mem::size_of::<Option<Entry>>()
            + self.name_bytes
            + self.occupied
                * (std::mem::size_of::<Equivalence>() + 2 * std::mem::size_of::<usize>())
    }
}
#[derive(Default)]
struct Cache {
    roots: VecDeque<Root>,
    hits: u64,
    bytes: usize,
}
thread_local! { static CACHE: RefCell<Cache> = RefCell::new(Cache::default()); }
#[cfg(feature = "latency-diagnostics")]
#[allow(dead_code)]
pub(crate) fn diagnostics() -> serde_json::Value {
    CACHE.with(|cache| { let c=cache.borrow(); serde_json::json!({"roots":c.roots.len(),"bytes":c.bytes,"cumulative_hits":c.hits,"byte_budget":BUDGET,"root_budget":ROOTS,"slots_per_root":ENTRIES}) })
}
pub(super) fn resolve(
    table: &SharedTable<Arc<str>, Arc<Equivalence>>,
    symbol: i64,
    name: &str,
) -> Option<Arc<Equivalence>> {
    #[cfg(feature = "latency-diagnostics")]
    let _timing = crate::output_diagnostics::detailed()
        .then(|| crate::output_diagnostics::operation("meaning_lookup"));
    if name.len() > 4096 {
        return table.get(name).cloned();
    }
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::count("meaning_cache_lookups", 1);
        let address = table.storage_address();
        let index = cache
            .roots
            .iter()
            .rposition(|root| root.owner.as_ptr() as usize == address);
        let mut root = if let Some(index) = index {
            let root = cache.roots.remove(index).unwrap();
            cache.bytes -= root.bytes;
            root
        } else {
            #[cfg(feature = "latency-diagnostics")]
            crate::latency_diagnostics::count("meaning_cache_root_misses", 1);
            #[cfg(feature = "latency-diagnostics")]
            crate::output_diagnostics::count("meaning_root_misses", 1);
            cache.roots.retain(|root| root.owner.strong_count() != 0);
            cache.bytes = cache.roots.iter().map(|root| root.bytes).sum();
            Root {
                owner: table.weak_storage(),
                entries: (0..ENTRIES).map(|_| None).collect(),
                occupied: 0,
                bytes: 0,
                name_bytes: 0,
            }
        };
        let slot = symbol as usize % ENTRIES;
        let cached = root.entries[slot]
            .as_ref()
            .filter(|entry| entry.symbol == symbol && entry.name == name)
            .and_then(|entry| match &entry.value {
                None => Some(None),
                Some(value) => value.upgrade().map(Some),
            });
        let value = if let Some(value) = cached {
            cache.hits += 1;
            #[cfg(feature = "latency-diagnostics")]
            crate::latency_diagnostics::count("meaning_cache_hits", 1);
            #[cfg(feature = "latency-diagnostics")]
            crate::output_diagnostics::count("meaning_hits", 1);
            value
        } else {
            #[cfg(feature = "latency-diagnostics")]
            crate::latency_diagnostics::count("meaning_cache_misses", 1);
            #[cfg(feature = "latency-diagnostics")]
            crate::output_diagnostics::count("meaning_misses", 1);
            let value = table.get(name).cloned();
            if let Some(old) = root.entries[slot].take() {
                root.name_bytes -= old.name.capacity();
            } else {
                root.occupied += 1;
            }
            let name = name.to_owned();
            root.name_bytes += name.capacity();
            root.entries[slot] = Some(Entry {
                symbol,
                name,
                value: value.as_ref().map(Arc::downgrade),
            });
            root.bytes = root.measure_bytes();
            value
        };
        while cache.roots.len() >= ROOTS {
            let old = cache.roots.pop_front().unwrap();
            #[cfg(feature = "latency-diagnostics")]
            crate::latency_diagnostics::count("meaning_cache_root_evictions", 1);
            cache.bytes -= old.bytes;
        }
        cache.bytes += root.bytes;
        cache.roots.push_back(root);
        while cache.bytes > BUDGET {
            let old = cache.roots.pop_front().unwrap();
            #[cfg(feature = "latency-diagnostics")]
            crate::latency_diagnostics::count("meaning_cache_root_evictions", 1);
            cache.bytes -= old.bytes;
        }
        value
    })
}
#[cfg(test)]
mod tests {
    use super::super::records::{CommandOperand, MeaningCommand};
    use super::*;
    use crate::core::enums::Command;
    fn value(number: i64) -> Arc<Equivalence> {
        Arc::new(Equivalence {
            command: MeaningCommand::Operation(Command::Relax),
            level: 1,
            value: CommandOperand::Number(number),
        })
    }
    #[test]
    fn mutation_rollback_names_and_undefined_bindings_never_reuse_stale_values() {
        CACHE.with(|cache| *cache.borrow_mut() = Cache::default());
        let mut table: SharedTable<Arc<str>, Arc<Equivalence>> = BTreeMap::from([
            (Arc::from("name"), value(1)),
            (Arc::from("other"), value(2)),
        ])
        .into();
        assert_eq!(
            resolve(&table, 1, "name").unwrap().value,
            CommandOperand::Number(1)
        );
        let snapshot = table.clone();
        table.insert(Arc::from("name"), value(3));
        assert_eq!(
            resolve(&table, 1, "name").unwrap().value,
            CommandOperand::Number(3)
        );
        assert_eq!(
            resolve(&snapshot, 1, "name").unwrap().value,
            CommandOperand::Number(1)
        );
        assert_eq!(
            resolve(&table, 1, "other").unwrap().value,
            CommandOperand::Number(2)
        );
        assert!(resolve(&table, 1, "missing").is_none());
        table.insert(Arc::from("missing"), value(4));
        assert_eq!(
            resolve(&table, 1, "missing").unwrap().value,
            CommandOperand::Number(4)
        );
        drop(snapshot);
        resolve(&table, 2, "name");
        let old = table.weak_storage();
        table.remove("name");
        assert!(
            old.upgrade().is_none(),
            "unique mutable tables must dissociate cached weak owners"
        );
        assert!(resolve(&table, 2, "name").is_none());
    }
    #[test]
    fn retention_and_old_equivalence_payloads_are_bounded() {
        CACHE.with(|cache| *cache.borrow_mut() = Cache::default());
        let mut tables = Vec::new();
        for root in 0..20 {
            let table: SharedTable<Arc<str>, Arc<Equivalence>> = (0..1000)
                .map(|id| (Arc::from(format!("name{root}_{id}")), value(id)))
                .collect();
            for id in 0..1000 {
                resolve(&table, id, &format!("name{root}_{id}"));
            }
            tables.push(table);
        }
        CACHE.with(|cache| {
            let cache = cache.borrow();
            assert!(cache.roots.len() <= ROOTS);
            assert!(cache.roots.iter().all(|root| root.occupied <= ENTRIES));
            assert!(cache.roots.iter().map(Root::measure_bytes).sum::<usize>() <= BUDGET);
        });
        let retained = tables.last().unwrap().get("name19_999").unwrap().clone();
        let weak = Arc::downgrade(&retained);
        drop(retained);
        drop(tables);
        assert_eq!(
            weak.strong_count(),
            0,
            "memo must not retain equivalence data"
        );
    }
}
