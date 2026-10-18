//! Revision-tagged speculative output bytes used by resident catalog hosts.
//! The conventional native CLI retains direct filesystem stream behavior.
use super::catalog::{CatalogDelta, ResourceCatalog, ResourceChange, ResourceId, ResourcePath};
use crate::core::errors::{Error, Result};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentRevision {
    pub document: String,
    pub revision: u64,
}
pub struct EffectTransaction {
    tag: DocumentRevision,
    base_epoch: u64,
    byte_budget: usize,
    bytes: usize,
    outputs: BTreeMap<ResourcePath, Option<Arc<[u8]>>>,
}
impl EffectTransaction {
    pub fn new(tag: DocumentRevision, catalog: &ResourceCatalog, byte_budget: usize) -> Self {
        Self {
            tag,
            base_epoch: catalog.epoch(),
            byte_budget,
            bytes: 0,
            outputs: BTreeMap::new(),
        }
    }
    pub fn tag(&self) -> &DocumentRevision {
        &self.tag
    }
    pub fn output_bytes(&self) -> usize {
        self.bytes
    }
    pub fn read(
        &self,
        catalog: &ResourceCatalog,
        path: &ResourcePath,
    ) -> Result<Option<Arc<[u8]>>> {
        if self.base_epoch != catalog.epoch() {
            return Err(Error::value(
                "Resource catalog changed during speculative execution",
            ));
        }
        Ok(match self.outputs.get(path) {
            Some(bytes) => bytes.clone(),
            None => catalog.get(path).map(|r| r.bytes.clone()),
        })
    }
    pub fn write(&mut self, path: ResourcePath, bytes: Arc<[u8]>) -> Result<()> {
        self.replace(path, Some(bytes))
    }
    fn replace(&mut self, path: ResourcePath, bytes: Option<Arc<[u8]>>) -> Result<()> {
        if path.0.is_empty() || path.0.contains(&0) {
            return Err(Error::value("Invalid speculative output path"));
        }
        if !self.outputs.contains_key(&path) && self.outputs.len() >= 20_000 {
            return Err(Error::value("Speculative effect path budget exceeded"));
        }
        let old = self
            .outputs
            .get(&path)
            .and_then(Option::as_ref)
            .map_or(0, |data| data.len());
        let total = (self.bytes - old)
            .checked_add(bytes.as_ref().map_or(0, |data| data.len()))
            .ok_or_else(|| Error::value("Speculative output byte count overflow"))?;
        if total > self.byte_budget {
            return Err(Error::value("Speculative output byte budget exceeded"));
        }
        self.outputs.insert(path, bytes);
        self.bytes = total;
        Ok(())
    }
    pub fn remove(&mut self, catalog: &ResourceCatalog, path: ResourcePath) -> Result<()> {
        if self.read(catalog, &path)?.is_none() {
            return Err(Error::value("Speculative removal source does not exist"));
        }
        self.replace(path, None)
    }
    pub fn rename(
        &mut self,
        catalog: &ResourceCatalog,
        from: ResourcePath,
        to: ResourcePath,
    ) -> Result<()> {
        let bytes = self
            .read(catalog, &from)?
            .ok_or_else(|| Error::value("Speculative rename source does not exist"))?;
        if from == to {
            return Ok(());
        }
        // Remove before adding so moving within the byte budget does not
        // require space for two copies. Restore the journal on any rejection.
        let previous = self.outputs.clone();
        let previous_bytes = self.bytes;
        let result = self
            .replace(from, None)
            .and_then(|()| self.write(to, bytes));
        if result.is_err() {
            self.outputs = previous;
            self.bytes = previous_bytes;
        }
        result
    }
    /// Consuming the transaction guarantees a commit cannot be repeated.
    /// Dropping it (cancel/coalesce/failure) publishes no catalog bytes.
    pub fn commit(self, catalog: &mut ResourceCatalog, accepted: &DocumentRevision) -> Result<()> {
        if &self.tag != accepted {
            return Err(Error::value(
                "Obsolete document revision cannot publish outputs",
            ));
        }
        if self.base_epoch != catalog.epoch() {
            return Err(Error::value(
                "Resource catalog changed before output commit",
            ));
        }
        if self.outputs.is_empty() {
            return Ok(());
        }
        let mut changes = Vec::with_capacity(self.outputs.len());
        for (path, bytes) in self.outputs {
            let Some(bytes) = bytes else {
                changes.push(ResourceChange::Remove(path));
                continue;
            };
            let old = catalog.get(&path);
            let version = old.as_ref().map_or(Ok(1), |r| {
                r.version
                    .checked_add(1)
                    .ok_or_else(|| Error::value("Output resource version overflow"))
            })?;
            let id = old.map_or_else(
                || ResourceId(format!("output:{:x}", Sha256::digest(&path.0))),
                |r| r.id.clone(),
            );
            changes.push(ResourceChange::Put {
                id,
                path,
                version,
                bytes,
            });
        }
        let epoch = catalog.epoch();
        catalog.apply(CatalogDelta {
            base_epoch: epoch,
            new_epoch: epoch
                .checked_add(1)
                .ok_or_else(|| Error::value("Resource catalog epoch overflow"))?,
            policy_version: catalog.policy_version(),
            changes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tag(revision: u64) -> DocumentRevision {
        DocumentRevision {
            document: "document".into(),
            revision,
        }
    }
    fn path() -> ResourcePath {
        ResourcePath(b"document.aux".to_vec())
    }
    #[test]
    fn speculative_reads_see_writes_but_cancel_and_stale_commits_publish_nothing() {
        let mut catalog = ResourceCatalog::new(Default::default());
        let mut transaction = EffectTransaction::new(tag(1), &catalog, 100);
        transaction
            .write(path(), Arc::from(b"first\n".as_slice()))
            .unwrap();
        assert_eq!(
            transaction
                .read(&catalog, &path())
                .unwrap()
                .unwrap()
                .as_ref(),
            b"first\n"
        );
        assert!(catalog.get(&path()).is_none());
        drop(transaction);
        assert_eq!(catalog.epoch(), 0);
        let mut late = EffectTransaction::new(tag(2), &catalog, 100);
        late.write(path(), Arc::from(b"late".as_slice())).unwrap();
        assert!(late.commit(&mut catalog, &tag(3)).is_err());
        assert!(catalog.get(&path()).is_none());
    }
    #[test]
    fn accepted_auxiliary_generations_are_distinct_immutable_inputs() {
        let mut catalog = ResourceCatalog::new(Default::default());
        for revision in [1, 2] {
            let before = catalog.get(&path());
            let mut transaction = EffectTransaction::new(tag(revision), &catalog, 100);
            transaction
                .write(path(), Arc::from(vec![revision as u8]))
                .unwrap();
            transaction.commit(&mut catalog, &tag(revision)).unwrap();
            assert_eq!(catalog.get(&path()).unwrap().version, revision);
            if let Some(before) = before {
                assert_eq!(before.bytes.as_ref(), &[1]);
            }
        }
        assert_eq!(catalog.total_bytes(), 1);
        assert_eq!(catalog.resource_count(), 1);
    }
    #[test]
    fn failed_budget_and_changed_catalog_do_not_partial_commit() {
        let mut catalog = ResourceCatalog::new(Default::default());
        let mut transaction = EffectTransaction::new(tag(1), &catalog, 3);
        transaction
            .write(path(), Arc::from(b"old".as_slice()))
            .unwrap();
        assert!(
            transaction
                .write(path(), Arc::from(b"larger".as_slice()))
                .is_err()
        );
        assert_eq!(transaction.output_bytes(), 3);
        catalog
            .apply(CatalogDelta {
                base_epoch: 0,
                new_epoch: 1,
                policy_version: 1,
                changes: vec![],
            })
            .unwrap();
        assert!(transaction.read(&catalog, &path()).is_err());
        assert!(transaction.commit(&mut catalog, &tag(1)).is_err());
        assert!(catalog.get(&path()).is_none());
    }

    #[test]
    fn rename_and_removal_publish_as_one_generation_and_cancel_has_no_effect() {
        let mut catalog = ResourceCatalog::new(Default::default());
        let a = ResourcePath(b"a".to_vec());
        let b = ResourcePath(b"b".to_vec());
        let mut initial = EffectTransaction::new(tag(1), &catalog, 3);
        initial
            .write(a.clone(), Arc::from(b"old".as_slice()))
            .unwrap();
        initial.commit(&mut catalog, &tag(1)).unwrap();
        let epoch = catalog.epoch();
        let mut cancelled = EffectTransaction::new(tag(2), &catalog, 3);
        cancelled.rename(&catalog, a.clone(), b.clone()).unwrap();
        assert!(cancelled.read(&catalog, &a).unwrap().is_none());
        assert_eq!(
            cancelled.read(&catalog, &b).unwrap().unwrap().as_ref(),
            b"old"
        );
        drop(cancelled);
        assert_eq!(catalog.epoch(), epoch);
        assert!(catalog.get(&a).is_some());
        let mut moved = EffectTransaction::new(tag(3), &catalog, 3);
        moved.rename(&catalog, a.clone(), b.clone()).unwrap();
        moved.commit(&mut catalog, &tag(3)).unwrap();
        assert_eq!(catalog.epoch(), epoch + 1);
        assert!(catalog.get(&a).is_none());
        assert_eq!(catalog.get(&b).unwrap().bytes.as_ref(), b"old");
        let mut removed = EffectTransaction::new(tag(4), &catalog, 0);
        removed.remove(&catalog, b.clone()).unwrap();
        removed.commit(&mut catalog, &tag(4)).unwrap();
        assert!(catalog.get(&b).is_none());
    }

    #[test]
    fn failed_rename_restores_the_entire_effect_journal() {
        let catalog = ResourceCatalog::new(Default::default());
        let mut effects = EffectTransaction::new(tag(1), &catalog, 3);
        effects.write(path(), Arc::from(b"abc".as_slice())).unwrap();
        assert!(
            effects
                .rename(&catalog, path(), ResourcePath(vec![0]))
                .is_err()
        );
        assert_eq!(effects.output_bytes(), 3);
        assert_eq!(
            effects.read(&catalog, &path()).unwrap().unwrap().as_ref(),
            b"abc"
        );
        let moved = ResourcePath(b"moved".to_vec());
        effects.rename(&catalog, path(), moved.clone()).unwrap();
        assert_eq!(effects.output_bytes(), 3);
        assert!(effects.read(&catalog, &path()).unwrap().is_none());
        assert_eq!(
            effects.read(&catalog, &moved).unwrap().unwrap().as_ref(),
            b"abc"
        );
    }
}
