//! Immutable project bytes and positive/negative lookup observations.
//!
//! Paths are host-resolved byte identities, not editor text. The host must
//! advance the catalog epoch for every resource/lookup-policy change. Missed
//! notifications require reconciliation before reusing an execution checkpoint.
//! Existing filesystem adapters have not yet been migrated to this catalog.
use crate::core::errors::{Error, Result};
use chrono::{DateTime, Datelike, Utc};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResourcePath(pub Vec<u8>);
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResourceId(pub String);

#[derive(Clone, Debug)]
pub struct Resource {
    pub id: ResourceId,
    pub path: ResourcePath,
    pub version: u64,
    /// Distinguishes remove/recreate even if a host restarts its version counter.
    pub generation: u64,
    pub bytes: Arc<[u8]>,
    pub sha256: [u8; 32],
    /// Supplied physical metadata; None preserves the legacy host-clock policy.
    pub modified: Option<DateTime<Utc>>,
}
#[derive(Clone, Debug)]
pub enum ResourceChange {
    Put {
        id: ResourceId,
        path: ResourcePath,
        version: u64,
        bytes: Arc<[u8]>,
    },
    PutWithMetadata {
        id: ResourceId,
        path: ResourcePath,
        version: u64,
        bytes: Arc<[u8]>,
        modified: DateTime<Utc>,
    },
    Remove(ResourcePath),
}
impl ResourceChange {
    /// Bytes and supplied timestamp change atomically under one content version.
    pub fn put(
        id: ResourceId,
        path: ResourcePath,
        version: u64,
        bytes: Arc<[u8]>,
        modified_unix_ms: Option<i64>,
    ) -> Result<Self> {
        let Some(milliseconds) = modified_unix_ms else {
            return Ok(Self::Put {
                id,
                path,
                version,
                bytes,
            });
        };
        // Match the reference datetime's supported physical date range, and
        // reject a host timestamp that could panic a later chrono conversion.
        if !(-62_135_596_800_000..=253_402_300_799_999).contains(&milliseconds) {
            return Err(Error::value(
                "Resource modification time must be within years 1 through 9999",
            ));
        }
        let modified = DateTime::<Utc>::from_timestamp_millis(milliseconds)
            .ok_or_else(|| Error::value("Resource modification time is out of range"))?;
        Ok(Self::PutWithMetadata {
            id,
            path,
            version,
            bytes,
            modified,
        })
    }
    fn path(&self) -> &ResourcePath {
        match self {
            Self::Put { path, .. } | Self::PutWithMetadata { path, .. } | Self::Remove(path) => {
                path
            }
        }
    }
}
#[derive(Clone, Debug)]
pub struct CatalogDelta {
    pub base_epoch: u64,
    pub new_epoch: u64,
    /// Changes to search precedence/configuration invalidate all observations.
    pub policy_version: u64,
    pub changes: Vec<ResourceChange>,
}
#[derive(Clone, Debug)]
pub struct CatalogLimits {
    pub max_resource_bytes: usize,
    pub max_total_bytes: usize,
    pub max_resources: usize,
    pub max_lookup_entries: usize,
    pub max_path_bytes: usize,
    pub max_id_bytes: usize,
    pub max_lookup_candidates: usize,
    pub max_delta_changes: usize,
}
impl Default for CatalogLimits {
    fn default() -> Self {
        Self {
            max_resource_bytes: 64 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024,
            max_resources: 20_000,
            max_lookup_entries: 1024,
            max_path_bytes: 4096,
            max_id_bytes: 1024,
            max_lookup_candidates: 4096,
            max_delta_changes: 20_000,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookupObservation {
    pub policy_version: u64,
    /// Include misses before the selected file so a newly present higher
    /// priority file invalidates reuse. A negative result records every candidate.
    pub candidates: Vec<(ResourcePath, Option<(ResourceId, u64, u64)>)>,
}
#[derive(Clone, Debug)]
pub struct LookupResult {
    pub resource: Option<Arc<Resource>>,
    pub observation: LookupObservation,
}
pub struct ResourceCatalog {
    epoch: u64,
    policy_version: u64,
    total_bytes: usize,
    limits: CatalogLimits,
    resources: BTreeMap<ResourcePath, Arc<Resource>>,
    ids: BTreeMap<ResourceId, ResourcePath>,
    // A bounded FIFO cache is sufficient initially. Observations validate only
    // queried candidate versions; no directory-tree hashing happens on lookup.
    lookups: BTreeMap<Arc<[ResourcePath]>, LookupResult>,
    lookup_order: VecDeque<Arc<[ResourcePath]>>,
}
impl ResourceCatalog {
    pub fn new(limits: CatalogLimits) -> Self {
        Self {
            epoch: 0,
            policy_version: 0,
            total_bytes: 0,
            limits,
            resources: BTreeMap::new(),
            ids: BTreeMap::new(),
            lookups: BTreeMap::new(),
            lookup_order: VecDeque::new(),
        }
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn policy_version(&self) -> u64 {
        self.policy_version
    }
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }
    pub fn resource_count(&self) -> usize {
        self.resources.len()
    }
    pub fn cached_lookup_count(&self) -> usize {
        self.lookups.len()
    }
    pub fn get(&self, path: &ResourcePath) -> Option<Arc<Resource>> {
        self.resources.get(path).cloned()
    }
    pub fn paths(&self) -> impl Iterator<Item = &ResourcePath> {
        self.resources.keys()
    }

    /// Validate the whole delta before changing visible resources. Rejected
    /// stale replies, identity collisions or budgets cannot partially apply.
    pub fn validate(&self, delta: &CatalogDelta) -> Result<usize> {
        if delta.base_epoch != self.epoch || delta.new_epoch <= self.epoch {
            return Err(Error::value(
                "Stale or non-advancing resource catalog delta",
            ));
        }
        if delta.policy_version < self.policy_version {
            return Err(Error::value(
                "Resource lookup policy version moved backwards",
            ));
        }
        let mut paths = BTreeSet::new();
        let mut ids = BTreeSet::new();
        let mut bytes = self.total_bytes;
        let mut count = self.resources.len();
        if delta.changes.len() > self.limits.max_delta_changes {
            return Err(Error::value("Resource delta exceeds change count budget"));
        }
        let removed = delta
            .changes
            .iter()
            .filter_map(|c| match c {
                ResourceChange::Remove(path) => Some(path),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        // Subtract all old data before adding replacements. Preflight must be
        // independent of ordering in an otherwise valid atomic delta.
        for change in &delta.changes {
            let path = change.path();
            if path.0.is_empty()
                || path.0.len() > self.limits.max_path_bytes
                || path.0.contains(&0)
                || !paths.insert(path.clone())
            {
                return Err(Error::value(
                    "Invalid or duplicate resource path in catalog delta",
                ));
            }
            let previous = self.resources.get(path);
            if let Some(previous) = previous {
                bytes -= previous.bytes.len();
                count -= 1;
            }
        }
        for change in &delta.changes {
            let path = change.path();
            let previous = self.resources.get(path);
            if let ResourceChange::PutWithMetadata { modified, .. } = change {
                if !(1..=9999).contains(&modified.year()) {
                    return Err(Error::value(
                        "Resource modification time must be within years 1 through 9999",
                    ));
                }
            }
            if let ResourceChange::Put {
                id,
                version,
                bytes: data,
                ..
            }
            | ResourceChange::PutWithMetadata {
                id,
                version,
                bytes: data,
                ..
            } = change
            {
                if id.0.is_empty()
                    || id.0.len() > self.limits.max_id_bytes
                    || !ids.insert(id.clone())
                    || *version == 0
                {
                    return Err(Error::value(
                        "Invalid or duplicate resource identity/version",
                    ));
                }
                if let Some(old_path) = self.ids.get(id).filter(|old_path| *old_path != path) {
                    if !removed.contains(old_path) || *version <= self.resources[old_path].version {
                        return Err(Error::value(
                            "Resource rename requires removing the old path and advancing its version",
                        ));
                    }
                }
                if previous.is_some_and(|old| old.id != *id || *version <= old.version) {
                    return Err(Error::value(
                        "Resource identity changed or content version did not advance",
                    ));
                }
                if data.len() > self.limits.max_resource_bytes {
                    return Err(Error::value("Resource exceeds catalog byte budget"));
                }
                bytes = bytes
                    .checked_add(data.len())
                    .ok_or_else(|| Error::value("Catalog byte count overflow"))?;
                count += 1;
            }
        }
        if bytes > self.limits.max_total_bytes || count > self.limits.max_resources {
            return Err(Error::value(
                "Resource catalog exceeds total byte/count budget",
            ));
        }
        Ok(bytes)
    }
    pub fn apply(&mut self, delta: CatalogDelta) -> Result<()> {
        let bytes = self.validate(&delta)?;
        let removed = delta
            .changes
            .iter()
            .filter_map(|change| match change {
                ResourceChange::Remove(path) => Some(path),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        // Remove first so a rename does not erase the newly inserted ID.
        for path in removed {
            if let Some(old) = self.resources.remove(path) {
                self.ids.remove(&old.id);
            }
        }
        for change in delta.changes {
            let modified = match &change {
                ResourceChange::PutWithMetadata { modified, .. } => Some(*modified),
                _ => None,
            };
            match change {
                ResourceChange::Remove(path) => {
                    if let Some(old) = self.resources.remove(&path) {
                        self.ids.remove(&old.id);
                    }
                }
                ResourceChange::Put {
                    id,
                    path,
                    version,
                    bytes,
                }
                | ResourceChange::PutWithMetadata {
                    id,
                    path,
                    version,
                    bytes,
                    ..
                } => {
                    let sha256 = Sha256::digest(&bytes).into();
                    self.ids.insert(id.clone(), path.clone());
                    self.resources.insert(
                        path.clone(),
                        Arc::new(Resource {
                            id,
                            path,
                            version,
                            generation: delta.new_epoch,
                            bytes,
                            sha256,
                            modified,
                        }),
                    );
                }
            }
        }
        self.total_bytes = bytes;
        self.epoch = delta.new_epoch;
        self.policy_version = delta.policy_version;
        // Release stale cached resources immediately, including their byte Arcs.
        let mut retained = std::mem::take(&mut self.lookups);
        retained.retain(|_, result| self.observation_valid(&result.observation));
        self.lookup_order.retain(|key| retained.contains_key(key));
        self.lookups = retained;
        Ok(())
    }
    pub fn observation_valid(&self, observation: &LookupObservation) -> bool {
        observation.policy_version == self.policy_version
            && observation.candidates.iter().all(|(path, version)| {
                self.resources
                    .get(path)
                    .map(|resource| (&resource.id, resource.version, resource.generation))
                    == version
                        .as_ref()
                        .map(|(id, version, generation)| (id, *version, *generation))
            })
    }
    /// A metadata classification needs the resource, not a cached ordered
    /// lookup observation. Retain the same single-candidate transport limits.
    pub(crate) fn metadata_resource(&self, path: &ResourcePath) -> Result<Option<Arc<Resource>>> {
        if self.limits.max_lookup_candidates < 1
            || path.0.len() > self.limits.max_path_bytes
            || path.0.is_empty()
            || path.0.contains(&0)
        {
            return Err(Error::value(
                "Invalid resource lookup or candidate budget exceeded",
            ));
        }
        Ok(self.get(path))
    }
    pub fn lookup(&mut self, candidates: &[ResourcePath]) -> Result<LookupResult> {
        if candidates.len() > self.limits.max_lookup_candidates
            || candidates.iter().any(|p| {
                p.0.len() > self.limits.max_path_bytes || p.0.is_empty() || p.0.contains(&0)
            })
        {
            return Err(Error::value(
                "Invalid resource lookup or candidate budget exceeded",
            ));
        }
        if let Some(result) = self.lookups.get(candidates) {
            return Ok(result.clone());
        }
        let mut result = LookupResult {
            resource: None,
            observation: LookupObservation {
                policy_version: self.policy_version,
                candidates: Vec::new(),
            },
        };
        for path in candidates {
            let resource = self.get(path);
            result.observation.candidates.push((
                path.clone(),
                resource
                    .as_ref()
                    .map(|r| (r.id.clone(), r.version, r.generation)),
            ));
            if resource.is_some() {
                result.resource = resource;
                break;
            }
        }
        if self.limits.max_lookup_entries > 0 {
            if self.lookups.len() == self.limits.max_lookup_entries {
                let oldest = self.lookup_order.pop_front().unwrap();
                self.lookups.remove(&oldest);
            }
            // FIFO hits do not change eviction order. Share the immutable key
            // between the ordered index and queue instead of copying paths.
            let key: Arc<[ResourcePath]> = candidates.into();
            self.lookup_order.push_back(key.clone());
            self.lookups.insert(key, result.clone());
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path(name: &[u8]) -> ResourcePath {
        ResourcePath(name.into())
    }
    fn put(name: &[u8], version: u64, bytes: &[u8]) -> ResourceChange {
        ResourceChange::Put {
            path: path(name),
            id: ResourceId(format!("{:?}", name)),
            version,
            bytes: Arc::from(bytes),
        }
    }
    fn apply(catalog: &mut ResourceCatalog, changes: Vec<ResourceChange>) {
        catalog
            .apply(CatalogDelta {
                base_epoch: catalog.epoch,
                new_epoch: catalog.epoch + 1,
                policy_version: catalog.policy_version,
                changes,
            })
            .unwrap();
    }
    #[test]
    fn new_higher_priority_files_invalidate_positive_and_negative_results() {
        let mut catalog = ResourceCatalog::new(Default::default());
        let candidates = [path(b"project/a.tex"), path(b"bundle/a.tex")];
        let missing = catalog.lookup(&candidates).unwrap().observation;
        apply(&mut catalog, vec![put(b"bundle/a.tex", 1, b"bundled")]);
        assert!(!catalog.observation_valid(&missing));
        let bundled = catalog.lookup(&candidates).unwrap();
        apply(&mut catalog, vec![put(b"project/a.tex", 1, b"project")]);
        assert!(!catalog.observation_valid(&bundled.observation));
        assert_eq!(
            catalog
                .lookup(&candidates)
                .unwrap()
                .resource
                .unwrap()
                .bytes
                .as_ref(),
            b"project"
        );
        assert_eq!(bundled.resource.unwrap().bytes.as_ref(), b"bundled");
    }
    #[test]
    fn lower_priority_changes_do_not_invalidate_selected_input() {
        let mut catalog = ResourceCatalog::new(Default::default());
        apply(&mut catalog, vec![put(b"first", 1, b"selected")]);
        let selected = catalog
            .lookup(&[path(b"first"), path(b"second")])
            .unwrap()
            .observation;
        apply(&mut catalog, vec![put(b"second", 1, b"unused")]);
        assert!(catalog.observation_valid(&selected));
        let delta = CatalogDelta {
            base_epoch: catalog.epoch,
            new_epoch: catalog.epoch + 1,
            policy_version: 1,
            changes: vec![],
        };
        catalog.apply(delta).unwrap();
        assert!(!catalog.observation_valid(&selected));
    }
    #[test]
    fn stale_and_invalid_deltas_are_atomic_and_bytes_stay_immutable() {
        let mut catalog = ResourceCatalog::new(Default::default());
        apply(&mut catalog, vec![put(b"octet-\xff.tex", 1, b"\xff\r\n")]);
        let old = catalog.get(&path(b"octet-\xff.tex")).unwrap();
        let bad = CatalogDelta {
            base_epoch: 1,
            new_epoch: 2,
            policy_version: 0,
            changes: vec![
                put(b"new", 1, b"new"),
                put(b"octet-\xff.tex", 1, b"overwrite"),
            ],
        };
        assert!(catalog.apply(bad).is_err());
        assert!(catalog.get(&path(b"new")).is_none());
        assert_eq!(catalog.epoch(), 1);
        apply(&mut catalog, vec![put(b"octet-\xff.tex", 2, b"updated")]);
        assert_eq!(old.bytes.as_ref(), b"\xff\r\n");
        assert!(!catalog.observation_valid(&LookupObservation {
            policy_version: 0,
            candidates: vec![(
                old.path.clone(),
                Some((old.id.clone(), old.version, old.generation))
            )]
        }));
        assert!(
            catalog
                .apply(CatalogDelta {
                    base_epoch: 1,
                    new_epoch: 3,
                    policy_version: 0,
                    changes: vec![]
                })
                .is_err()
        );
    }
    #[test]
    fn eviction_and_replacement_do_not_accumulate_catalog_storage() {
        let mut catalog = ResourceCatalog::new(CatalogLimits {
            max_lookup_entries: 4,
            ..Default::default()
        });
        for version in 1..=1000 {
            apply(
                &mut catalog,
                vec![put(b"document.tex", version, b"bounded")],
            );
            catalog
                .lookup(&[path(format!("missing-{version}").as_bytes())])
                .unwrap();
            catalog.lookup(&[path(b"document.tex")]).unwrap();
            assert_eq!(catalog.total_bytes(), 7);
            assert_eq!(catalog.resource_count(), 1);
            assert!(catalog.cached_lookup_count() <= 4);
            assert_eq!(catalog.lookup_order.len(), catalog.cached_lookup_count());
        }
        apply(
            &mut catalog,
            vec![ResourceChange::Remove(path(b"document.tex"))],
        );
        assert_eq!(catalog.total_bytes(), 0);
        assert_eq!(catalog.resource_count(), 0);
    }
    #[test]
    fn rename_and_recreation_invalidate_old_identities_atomically() {
        let mut catalog = ResourceCatalog::new(Default::default());
        apply(&mut catalog, vec![put(b"old", 1, b"original")]);
        let original = catalog.lookup(&[path(b"old")]).unwrap().observation;
        let ResourceChange::Put { id, .. } = put(b"old", 1, b"") else {
            unreachable!()
        };
        apply(
            &mut catalog,
            vec![
                ResourceChange::Put {
                    id,
                    path: path(b"new"),
                    version: 2,
                    bytes: Arc::from(b"renamed".as_slice()),
                },
                ResourceChange::Remove(path(b"old")),
            ],
        );
        assert!(catalog.get(&path(b"old")).is_none());
        assert!(!catalog.observation_valid(&original));
        assert_eq!(catalog.total_bytes(), 7);
        // The moved ID cannot simultaneously belong to the old path. Remove
        // the renamed resource before recreating its former identity.
        apply(&mut catalog, vec![ResourceChange::Remove(path(b"new"))]);
        apply(&mut catalog, vec![put(b"old", 1, b"different")]);
        assert!(!catalog.observation_valid(&original));
    }
    #[test]
    fn oversized_lookup_and_delta_metadata_cannot_fill_unbounded_caches() {
        let mut catalog = ResourceCatalog::new(CatalogLimits {
            max_path_bytes: 4,
            max_lookup_candidates: 2,
            max_delta_changes: 1,
            ..Default::default()
        });
        assert!(catalog.lookup(&[path(b"large")]).is_err());
        assert!(
            catalog
                .lookup(&[path(b"a"), path(b"b"), path(b"c")])
                .is_err()
        );
        assert_eq!(catalog.cached_lookup_count(), 0);
        let delta = CatalogDelta {
            base_epoch: 0,
            new_epoch: 1,
            policy_version: 0,
            changes: vec![put(b"a", 1, b"ok"), put(b"b", 1, b"ok")],
        };
        assert!(catalog.apply(delta).is_err());
        assert_eq!(catalog.resource_count(), 0);
        assert_eq!(catalog.epoch(), 0);
    }
}
