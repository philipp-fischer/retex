//! Resident editing over the shared engine and catalog host.
//!
//! The initial policy replays from quiescent command state whose buffered
//! physical prefix and observed resources remain unchanged. No text sentinel,
//! suffix equivalence or paragraph shortcut is used.
use crate::{
    Engine,
    core::{
        config::EngineConfig,
        errors::{Error, Result},
    },
    engine::RunProgress,
    resources::{
        catalog::{CatalogDelta, ResourceChange, ResourceId},
        effects::DocumentRevision,
        virtual_host::{OutputCheckpoint, ResourceCheckpoint, VirtualHost, resource_path},
    },
};
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

struct LiveCheckpoint {
    local_id: u64,
    #[cfg(any(feature = "latency-diagnostics", feature = "output-replay-control"))]
    investigation_id: u64,
    engine: Engine,
    effects: OutputCheckpoint,
    resources: ResourceCheckpoint,
    source: Arc<[u8]>,
    readers: Vec<crate::engine::checkpoints::SourceReaderObservation>,
    location: String,
    occurrence: u64,
    offset: usize,
    clock_bucket: (u64, i32),
    wall_identity: Option<(std::time::SystemTime, i32)>,
    pinned: bool,
    pre_effect_pinned: bool,
    first_pass_pinned: bool,
    later_pass_pinned: bool,
    page_boundary: bool,
    body_boundary: bool,
    pass_inputs: Option<Arc<OutputCheckpoint>>,
    paragraph_seen: bool,
}
/// All replacements refer to the identified base snapshot, in ascending,
/// non-overlapping order. Preflight validates every file before cancelling a run.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct SourceReplacement {
    pub start: usize,
    pub end: usize,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct SourceChange {
    pub id: String,
    pub path: String,
    pub expected_version: u64,
    pub version: u64,
    pub replacements: Vec<SourceReplacement>,
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct SourceTransaction {
    pub project: String,
    pub document: String,
    pub base_revision: u64,
    pub revision: u64,
    pub catalog_epoch: u64,
    pub changes: Vec<SourceChange>,
}
struct PreparedSourceTransaction {
    delta: CatalogDelta,
    path: PathBuf,
    root: Arc<[u8]>,
    anchor: Option<(String, usize)>,
    tag: DocumentRevision,
}
struct Document {
    tag: DocumentRevision,
    path: PathBuf,
    bytes: Arc<[u8]>,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct PageRollbackObservation {
    pub pass: usize,
    pub input_offset: usize,
    pub completed_pages: usize,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct PageCheckpointCoverage {
    pub page: usize,
    pub after_page: usize,
    pub input_offset: usize,
    pub earlier_restart: bool,
}
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ReuseObservation {
    pub auxiliary_input_generation: Option<u64>,
    pub auxiliary_input_revision: Option<u64>,
    pub auxiliary_input_pass: Option<usize>,
    pub auxiliary_input_stable: Option<bool>,
    pub auxiliary_checkpoint_reuses: u64,
    pub restored_auxiliary_checkpoint: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restored_local_checkpoint: Option<u64>,
    pub local_updates: serde_json::Value,
    pub computation_reuse: crate::engine::ReuseStats,
    pub restored_offset: usize,
    pub restored_file: Option<String>,
    pub restored_occurrence: Option<u64>,
    pub restored_file_offset: usize,
    pub restored_kind: Option<String>,
    pub checkpoint_rejection: Option<String>,
    #[cfg(any(feature = "latency-diagnostics", feature = "output-replay-control"))]
    pub restored_checkpoint: Option<u64>,
    pub checkpoint_count: usize,
    pub checkpoint_captures: u64,
    pub checkpoint_capture_rejections: BTreeMap<String, u64>,
    pub initialized_restarts: u64,
    pub pass_checkpoint_reuses: u64,
    pub execution_steps: u64,
    pub page_checkpoint_captures: u64,
    pub page_checkpoint_reuses: u64,
    pub restored_pages: usize,
    pub body_checkpoint_captures: u64,
    pub body_checkpoint_reuses: u64,
    /// At most eight recent boundaries in the current revision. These expose
    /// pending state for convergence investigation, not a suffix-reuse proof.
    pub page_boundaries: Vec<crate::engine::PageBoundaryObservation>,
    pub page_restores: Vec<PageRollbackObservation>,
}
/// Revision and catalog identity are checked before accepting a resource reply.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ResourceRequest {
    pub request_id: u64,
    pub document: String,
    pub revision: u64,
    pub catalog_epoch: u64,
    pub resource: crate::core::errors::MissingResource,
}
pub enum SessionProgress {
    Running(RunProgress),
    NeedResource(ResourceRequest),
}
/// Private working input snapshots do not publish outputs or certify settlement.
#[derive(Clone, Debug, serde::Serialize)]
pub struct SettlingObservation {
    pub pass: usize,
    pub max_passes: usize,
    /// running, stable, limit, or failed (the engine diagnostics remain intact).
    pub status: String,
    pub generations: Vec<PassObservation>,
    pub input_generation: Option<u64>,
    /// Latest successfully completed pass's private snapshot, even while running.
    pub output_generation: Option<u64>,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct PassObservation {
    pub pass: usize,
    pub inputs: Vec<AuxiliaryObservation>,
    pub outputs: Vec<AuxiliaryObservation>,
    pub changed_paths: Vec<Vec<u8>>,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct AuxiliaryObservation {
    pub path: Vec<u8>,
    /// None means absent, distinct from a present empty file.
    pub sha256: Option<String>,
}
struct WorkingAuxiliary {
    document: String,
    path: PathBuf,
    revision: u64,
    pass: usize,
    stable: bool,
    generation: u64,
    inputs: BTreeMap<PathBuf, Option<Arc<[u8]>>>,
    seed: Arc<OutputCheckpoint>,
    resources: ResourceCheckpoint,
}
struct Settling {
    observation: SettlingObservation,
    inputs: BTreeMap<PathBuf, Option<Arc<[u8]>>>,
    pass_inputs: Option<Arc<OutputCheckpoint>>,
}
#[derive(Default, serde::Serialize)]
pub struct RestorationTimings {
    pub preflight_ms: f64,
    pub cancellation_ms: f64,
    pub catalog_ms: f64,
    pub selection_ms: f64,
    pub effects_ms: f64,
    pub resources_ms: f64,
    pub engine_clone_ms: f64,
    pub input_rebind_ms: f64,
    pub retention_ms: f64,
}
pub struct EditingSession {
    preview_bases: std::cell::RefCell<VecDeque<Rc<crate::paragraph_preview::Base>>>,
    preview_displayed: std::cell::RefCell<Option<Rc<crate::paragraph_preview::Base>>>,
    preview_pending: Option<(
        Rc<crate::paragraph_preview::Base>,
        crate::paragraph_preview::Ready,
        usize,
    )>,
    preview_published: bool,
    preview_reason: Option<String>,
    preview_execution_ms: f64,
    preview_mapping: serde_json::Value,
    local_bases: std::cell::RefCell<VecDeque<Rc<crate::local_page::Base>>>,
    early: Option<crate::local_page::Early>,
    local_hits: usize,
    local_target: usize,
    local_cost_ms: f64,
    local_recipe_ms: std::cell::Cell<f64>,
    local_paragraph_started: Option<u128>,
    local_paragraph_ms: f64,
    local_verified: bool,
    local_verify_ms: f64,
    config: EngineConfig,
    host: Rc<VirtualHost>,
    engine: Option<Engine>,
    /// One superseded engine can be reclaimed after page presentation. It can
    /// never execute or publish; its effects have already been discarded.
    retired_engine: Option<Engine>,
    baseline: Option<Engine>,
    baseline_resources: Option<ResourceCheckpoint>,
    initialized: Option<crate::serialization::formats::FormatSnapshot>,
    document: Option<Document>,
    project: String,
    checkpoints: Vec<LiveCheckpoint>,
    #[cfg(any(feature = "latency-diagnostics", feature = "output-replay-control"))]
    investigation_pin: Option<u64>,
    #[cfg(any(feature = "latency-diagnostics", feature = "output-replay-control"))]
    investigation_anchor: Option<usize>,
    retired_checkpoints: VecDeque<LiveCheckpoint>,
    active: bool,
    catalog_epoch: u64,
    current_valid: bool,
    source_version: u64,
    clock_baseline: u64,
    wall_baseline: u64,
    inherited_wall: bool,
    paragraph_seen: bool,
    preamble_resources: Option<ResourceCheckpoint>,
    baseline_clock: Option<(u64, i32)>,
    last_offset: usize,
    last_physical: Option<(String, u64, usize)>,
    last_checkpoint_pages: usize,
    returned_input_pending: bool,
    edit_anchor: usize,
    edit_path: String,
    observations: ReuseObservation,
    checkpoint_capture_ms: f64,
    resource_request: Option<ResourceRequest>,
    next_request_id: u64,
    settling: Option<Settling>,
    working_auxiliary: Option<WorkingAuxiliary>,
    entry_inputs: Option<Arc<OutputCheckpoint>>,
    next_auxiliary_generation: u64,
    settling_configurable: bool,
    pass_finished_pending: bool,
    restoration_timings: RestorationTimings,
}
impl EditingSession {
    pub fn new(config: EngineConfig, host: Rc<VirtualHost>) -> Result<Self> {
        config.validate()?;
        let catalog_epoch = host.epoch();
        Ok(Self {
            preview_bases: Default::default(),
            preview_pending: None,
            preview_displayed: Default::default(),
            preview_published: false,
            preview_reason: None,
            preview_execution_ms: 0.0,
            preview_mapping: serde_json::Value::Null,
            local_bases: Default::default(),
            early: None,
            local_hits: 0,
            local_target: 1,
            local_cost_ms: 0.0,
            local_recipe_ms: Default::default(),
            local_paragraph_started: None,
            local_paragraph_ms: 0.0,
            local_verified: false,
            local_verify_ms: 0.0,
            config,
            host,
            engine: None,
            retired_engine: None,
            baseline: None,
            baseline_resources: None,
            initialized: None,
            document: None,
            project: String::new(),
            checkpoints: Vec::new(),
            #[cfg(any(feature = "latency-diagnostics", feature = "output-replay-control"))]
            investigation_pin: None,
            #[cfg(any(feature = "latency-diagnostics", feature = "output-replay-control"))]
            investigation_anchor: None,
            retired_checkpoints: VecDeque::new(),
            active: false,
            catalog_epoch,
            current_valid: false,
            source_version: 0,
            clock_baseline: 0,
            wall_baseline: 0,
            inherited_wall: false,
            paragraph_seen: false,
            preamble_resources: None,
            baseline_clock: None,
            last_offset: 0,
            last_physical: None,
            last_checkpoint_pages: 0,
            returned_input_pending: false,
            edit_anchor: 0,
            edit_path: String::new(),
            observations: Default::default(),
            checkpoint_capture_ms: 0.0,
            resource_request: None,
            next_request_id: 1,
            settling: None,
            working_auxiliary: None,
            entry_inputs: None,
            next_auxiliary_generation: 1,
            settling_configurable: false,
            pass_finished_pending: false,
            restoration_timings: Default::default(),
        })
    }
    #[cfg(any(feature = "latency-diagnostics", feature = "output-replay-control"))]
    pub(crate) fn pin_investigation_checkpoint(
        &mut self,
        limit: usize,
        anchor: usize,
    ) -> Result<serde_json::Value> {
        use sha2::{Digest, Sha256};
        let document = self
            .document
            .as_ref()
            .ok_or_else(|| Error::value("No investigation document"))?;
        let checkpoint = self
            .checkpoints
            .iter()
            .rev()
            .find(|checkpoint| {
                checkpoint.pass_inputs.is_none()
                    && checkpoint.offset <= limit
                    && checkpoint.offset <= document.bytes.len()
                    && checkpoint.source[..checkpoint.offset] == document.bytes[..checkpoint.offset]
                    && checkpoint.clock_bucket == self.host.clock_bucket()
                    && self
                        .host
                        .resources_valid(&checkpoint.resources, Some(&document.path))
                    && checkpoint
                        .wall_identity
                        .is_none_or(|identity| identity == self.host.wall_identity())
            })
            .ok_or_else(|| Error::value("No guarded investigation checkpoint"))?;
        self.investigation_pin = Some(checkpoint.investigation_id);
        self.investigation_anchor = Some(anchor);
        Ok(
            serde_json::json!({"id":checkpoint.investigation_id,"offset":checkpoint.offset,"capture_anchor":anchor,
            "prefix_sha256":format!("{:x}",Sha256::digest(&checkpoint.source[..checkpoint.offset])),
            "boundary":checkpoint.engine.page_boundary_observation(checkpoint.offset),
            "paragraph_count":checkpoint.engine.layout.paragraph_count,
            "diagnostic_control":"Exact stored checkpoint is pinned; ordinary source/resource/clock guards remain authoritative."}),
        )
    }
    pub fn host(&self) -> &Rc<VirtualHost> {
        &self.host
    }
    pub fn engine(&self) -> Result<&Engine> {
        self.engine
            .as_ref()
            .ok_or_else(|| Error::value("No document execution"))
    }
    pub fn settled_engine(&self) -> Result<&Engine> {
        if !self.current_valid
            || self.active
            || self.resource_request.is_some()
            || self.catalog_epoch != self.host.epoch()
        {
            return Err(Error::value("Settled document revision is unavailable"));
        }
        self.engine()
    }
    pub(crate) fn engine_mut(&mut self) -> Result<&mut Engine> {
        self.engine
            .as_mut()
            .ok_or_else(|| Error::value("No document execution"))
    }
    pub fn tag(&self) -> Option<&DocumentRevision> {
        self.document.as_ref().map(|d| &d.tag)
    }
    pub fn reuse(&self) -> ReuseObservation {
        let mut observation = self.observations.clone();
        if let Some(engine) = &self.engine {
            observation.computation_reuse = engine.execution_reuse_stats();
            let local = &engine.core.local;
            let evidence = local.evidence.borrow();
            observation.local_updates = serde_json::json!({"enabled":engine.core.config.exact_local_updates,"ambient":{"main_queried":self.host.main_queried(),"clock_changed":self.clock_baseline!=self.host.clock_observations(),"wall_changed":self.wall_baseline!=self.host.wall_observations()},"prefix_checkpoints":local.checkpoints,"paragraph_boundaries":local.paragraphs,"rejected_commands":local.rejected_commands,"hits":self.local_hits,"background_verified":self.local_verified,"background_verification_enabled":self.config.exact_local_verify,"base_revision":self.early.as_ref().map(|e|serde_json::json!({"document":e.base_tag.document,"revision":e.base_tag.revision})),"base_checkpoint":self.early.as_ref().map(|e|e.base_checkpoint),"background_verification_ms":self.local_verify_ms,"replacement_ms":self.local_cost_ms,"recipe_capture_ms":self.local_recipe_ms.get(),"paragraph_remainder_ms":self.local_paragraph_ms,"validation_ms":self.early.as_ref().map(|e|e.validation_ms),"fragment_paint_ms":self.early.as_ref().map(|e|e.paint_ms),"published_before_page_shipment":self.early.as_ref().is_some_and(|e|e.page.page>engine.completed_page_count()),"retained_bases":self.local_bases.borrow().len(),"retained_page_nodes":self.local_bases.borrow().iter().map(|b|b.page.lines.iter().map(|l|l.nodes.len()).sum::<usize>()).sum::<usize>(),"retained_page_placements":self.local_bases.borrow().iter().map(|b|b.page.lines.iter().map(|l|l.placements.len()).sum::<usize>()).sum::<usize>(),"candidates":local.ready.len(),"prefix_misses":local.misses,
                "consumer_misses":evidence.misses,"unknown_sites":evidence.unknown_sites.iter().map(|((candidate,site),count)|serde_json::json!({"candidate":candidate,"site":site,"count":count})).collect::<Vec<_>>(),"consumers":evidence.consumers.iter().map(|((candidate,name),count)|serde_json::json!({"candidate":candidate,"operation":name,"count":count})).collect::<Vec<_>>(),"paint_sites":evidence.paint.len(),
                "origins":local.ready.iter().map(|r| (r.origin.id,r.origin.offset,r.end,r.roots.len())).collect::<Vec<_>>()});
        }
        observation
    }
    pub fn checkpoint_capture_ms(&self) -> f64 {
        self.checkpoint_capture_ms
    }
    pub fn release_previous(&mut self) {
        self.retired_engine = None;
    }
    /// Retired snapshots have no execution/publication path. Drop at most one
    /// per worker task after the current visible page, bounding both memory and
    /// individual reclamation work. Native non-progressive sessions stay eager.
    pub fn reclaim_retired(&mut self, maximum: usize) -> usize {
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase("retired_reclamation", "", None);
        let count = maximum.min(self.retired_checkpoints.len());
        for _ in 0..count {
            self.retired_checkpoints.pop_front();
        }
        count
    }
    fn retain_checkpoints(&mut self, mut keep: impl FnMut(&LiveCheckpoint) -> bool) {
        #[cfg(any(feature = "latency-diagnostics", feature = "output-replay-control"))]
        let pin = self.investigation_pin;
        let mut filter = |checkpoint: &LiveCheckpoint| {
            #[cfg(any(feature = "latency-diagnostics", feature = "output-replay-control"))]
            if pin == Some(checkpoint.investigation_id) {
                return true;
            }
            keep(checkpoint)
        };
        if !self.config.progressive_pages {
            self.checkpoints.retain(&mut filter);
            return;
        }
        for checkpoint in std::mem::take(&mut self.checkpoints) {
            if filter(&checkpoint) {
                self.checkpoints.push(checkpoint);
            } else {
                // Broad invalidation or uninterrupted supersession can exhaust
                // the retirement budget. Eagerly reclaim the oldest then.
                if self.retired_checkpoints.len() == 32 {
                    self.retired_checkpoints.pop_front();
                }
                self.retired_checkpoints.push_back(checkpoint);
            }
        }
    }
    fn preceding_body_ids(&self, private: bool) -> Vec<u64> {
        // Retention is only a placement heuristic. Check the focused prefix
        // cheaply here; selection still validates every reader, resource,
        // clock and effect before any state can execute. Revalidating whole
        // resource sets for every retention candidate makes ordinary root
        // typing pay for all loaded package dependencies repeatedly.
        let bytes = self.host.source_bytes(Path::new(&self.edit_path));
        let mut preceding: Vec<_> = self
            .checkpoints
            .iter()
            .filter(|state| {
                state.body_boundary
                    && state.pass_inputs.is_some() == private
                    && state.location == self.edit_path
                    && state.readers.last().is_some_and(|r| {
                        r.offset <= self.edit_anchor
                            && bytes.as_ref().is_some_and(|bytes| {
                                r.offset <= bytes.len()
                                    && r.offset <= r.bytes.len()
                                    && r.bytes[..r.offset] == bytes[..r.offset]
                            })
                    })
            })
            .collect();
        preceding.sort_by_key(|state| state.readers.last().map_or(0, |r| r.offset));
        preceding
            .into_iter()
            .rev()
            .take(2)
            .map(|state| state.local_id)
            .collect()
    }
    pub fn restoration_timings(&self) -> &RestorationTimings {
        &self.restoration_timings
    }
    fn elapsed_ms(&self, start: u128) -> f64 {
        self.host.measurement_nanos().saturating_sub(start) as f64 / 1_000_000.0
    }
    pub fn settling(&self) -> Option<&SettlingObservation> {
        self.settling.as_ref().map(|s| &s.observation)
    }
    fn current_pass_inputs(&self) -> Option<&Arc<OutputCheckpoint>> {
        self.settling
            .as_ref()
            .map_or(self.entry_inputs.as_ref(), |s| s.pass_inputs.as_ref())
    }
    /// One explicit restart for every completed page, including page zero.
    /// Long commands may require a restart before their pending material.
    pub fn page_checkpoint_coverage(&self) -> Vec<PageCheckpointCoverage> {
        let Some(document) = &self.document else {
            return Vec::new();
        };
        let count = self
            .engine
            .as_ref()
            .map_or(0, |engine| engine.completed_page_count());
        let inputs = self.current_pass_inputs();
        (0..=count)
            .map(|page| {
                let selected = self.checkpoints.iter().rev().find(|checkpoint| {
                    checkpoint.engine.layout.pages.len() <= page
                        && checkpoint.offset <= document.bytes.len()
                        && checkpoint.source[..checkpoint.offset]
                            == document.bytes[..checkpoint.offset]
                        && checkpoint.clock_bucket == self.host.clock_bucket()
                        && checkpoint
                            .wall_identity
                            .is_none_or(|time| time == self.host.wall_identity())
                        && self
                            .host
                            .resources_valid(&checkpoint.resources, Some(&document.path))
                        && match (&checkpoint.pass_inputs, inputs) {
                            (None, None) => true,
                            (Some(old), Some(new)) => old.same_pass_inputs(new),
                            _ => false,
                        }
                });
                let after_page =
                    selected.map_or(0, |checkpoint| checkpoint.engine.layout.pages.len());
                PageCheckpointCoverage {
                    page,
                    after_page,
                    input_offset: selected.map_or(0, |checkpoint| checkpoint.offset),
                    earlier_restart: after_page < page,
                }
            })
            .collect()
    }
    #[cfg(all(feature = "latency-diagnostics", target_arch = "wasm32"))]
    pub(crate) fn clear_investigation_material(&mut self) -> Result<()> {
        if self.active || self.engine()?.status() != "complete" {
            return Err(Error::value(
                "Diagnostic material clearing requires a settled successful revision",
            ));
        }
        self.engine_mut()?.core.reuse.clear_material();
        Ok(())
    }
    pub fn checkpoint_storage(&self) -> serde_json::Value {
        let mut payloads = std::collections::BTreeSet::new();
        let mut references = 0usize;
        for checkpoint in &self.checkpoints {
            for page in checkpoint.engine.layout.pages.iter() {
                payloads.insert(Arc::as_ptr(page) as usize);
                references += 1;
            }
        }
        serde_json::json!({"states":self.checkpoints.len(),"state_capacity":256,"retired_engines":usize::from(self.retired_engine.is_some()),
            "retired_checkpoint_states":self.retired_checkpoints.len(),"retired_checkpoint_capacity":32,
            "body_states":self.checkpoints.iter().filter(|state| state.body_boundary).count(),"body_capacity_per_input_kind":32,
            "unique_shipped_page_payloads":payloads.len(),"page_references":references,
            "source_boundaries":self.checkpoints.iter().rev().take(64).map(|c|serde_json::json!({"file":c.location,"occurrence":c.occurrence,"offset":c.readers.last().map(|r|r.offset),"readers":c.readers,"kind":if c.body_boundary {"body"} else if c.page_boundary {"page"} else {"command"},"valid":self.checkpoint_sources_valid(c),"resource_rejections":self.host.source_resource_rejections(&c.resources,&c.readers.iter().map(|r|PathBuf::from(&r.path)).collect::<Vec<_>>())})).collect::<Vec<_>>(),
            "coverage":self.page_checkpoint_coverage()})
    }
    /// Read-only navigation over physical origins, excluding macro call sites.
    /// It neither advances execution nor changes checkpoint/clock policy.
    pub fn source_location_pages(&self, file: &str, line: usize, column: usize) -> Vec<usize> {
        let Some(engine) = &self.engine else {
            return Vec::new();
        };
        engine
            .layout
            .pages
            .iter()
            .take(engine.completed_page_count())
            .filter(|page| {
                page.lines
                    .iter()
                    .flat_map(|l| &l.placements)
                    .any(|placement| {
                        placement.origins.iter().flatten().any(|source| {
                            source.file.as_ref() == file
                                && source.line == line
                                && source
                                    .byte_column
                                    .zip(source.end_byte_column)
                                    .is_some_and(|(a, b)| column >= a.get() && column <= b.get())
                        })
                    })
            })
            .map(|page| page.page)
            .collect()
    }
    /// Physical lines present on shipped pages, including macro call sites.
    /// This is a location index; byte-prefix/resource guards remain authoritative.
    pub fn source_pages(&self) -> serde_json::Value {
        let mut locations =
            BTreeMap::<Arc<str>, BTreeMap<usize, std::collections::BTreeSet<usize>>>::new();
        if let Some(engine) = &self.engine {
            for page in engine
                .layout
                .pages
                .iter()
                .take(engine.completed_page_count())
                .map(|page| page.as_ref())
                .chain(
                    self.early
                        .as_ref()
                        .filter(|e| e.pass == self.settling().map_or(1, |s| s.pass))
                        .into_iter()
                        .map(|e| &e.page),
                )
            {
                for origin in page
                    .lines
                    .iter()
                    .flat_map(|line| &line.nodes)
                    .flat_map(|node| &node.origins)
                    .filter_map(|origin| origin.as_deref())
                {
                    let mut span = Some(origin);
                    while let Some(source) = span {
                        locations
                            .entry(source.file.clone())
                            .or_default()
                            .entry(source.line)
                            .or_default()
                            .insert(page.page);
                        span = source.call_site.as_deref();
                    }
                }
            }
        }
        serde_json::json!(locations)
    }
    pub fn set_preview_target(&mut self, page: usize) -> Result<()> {
        if !(1..=256).contains(&page) {
            return Err(Error::value("Invalid preview target"));
        }
        if page != self.local_target && self.early.is_some() {
            if let Some(engine) = &mut self.engine {
                *engine
                    .core
                    .local
                    .misses
                    .entry("viewport_changed_after_ready".into())
                    .or_default() += 1;
            }
        }
        self.local_target = page;
        Ok(())
    }
    /// A host confirms admission of an authoritative scene, not serialization.
    /// A provisional fragment is never admitted as a placement base.
    pub fn acknowledge_paragraph_page(
        &self,
        document: &str,
        revision: u64,
        pass: usize,
        page: usize,
        hash: &str,
    ) -> bool {
        if !self.config.paragraph_preview
            || self
                .tag()
                .is_none_or(|tag| tag.document != document || revision > tag.revision)
        {
            return false;
        }
        let base = self
            .preview_bases
            .borrow()
            .iter()
            .find(|base| {
                base.document == document
                    && base.revision == revision
                    && base.pass == pass
                    && base.page.page == page
                    && base.page.content_sha256 == hash
            })
            .cloned();
        let Some(base) = base else { return false };
        let mut displayed = self.preview_displayed.borrow_mut();
        if displayed.as_ref().is_some_and(|old| {
            old.document == document
                && (old.revision > revision
                    || (old.revision == revision && old.page.page == page && old.pass > pass))
        }) {
            return false;
        }
        *displayed = Some(base);
        true
    }
    pub fn paragraph_preview_status(&self) -> serde_json::Value {
        serde_json::json!({"enabled":self.config.paragraph_preview,"available":self.preview_pending.is_some(),"published":self.preview_published,"reason":if self.preview_published {None} else {self.preview_reason.as_ref().or_else(||self.engine.as_ref().and_then(|e|e.core.paragraph_preview.reason.as_ref()))},"execution_to_ready_ms":self.preview_execution_ms,"mapping":self.preview_mapping})
    }
    pub fn take_paragraph_preview(
        &mut self,
        cache: &mut crate::fonts::outlines::OutlineCache,
        images: &mut crate::pdf::preview_images::ImageCache,
    ) -> Result<Option<crate::paragraph_preview::Fragment>> {
        let Some((base, ready, index)) = self.preview_pending.take() else {
            return Ok(None);
        };
        let _host = crate::resources::host::enter(self.host.clone());
        match base.render(
            &base.regions[index],
            &ready,
            &self
                .host
                .source_bytes(Path::new(&ready.region.file))
                .ok_or_else(|| Error::value("Paragraph source unavailable"))?,
            self.engine.as_mut().unwrap(),
            cache,
            images,
        ) {
            Ok(mut fragment) => {
                self.preview_published = true;
                self.preview_reason = None;
                fragment.timings["execution_to_ready_ms"] =
                    serde_json::json!(self.preview_execution_ms);
                Ok(Some(fragment))
            }
            Err(error) => {
                self.preview_reason = Some(error.message);
                Ok(None)
            }
        }
    }
    pub fn early_page(&self) -> Option<usize> {
        self.early
            .as_ref()
            .filter(|e| {
                self.settling().map_or(1, |s| s.pass) == e.pass && e.page.page == self.local_target
            })
            .map(|e| e.page.page)
    }
    /// Exact portable source/geometry observation for development differentials.
    pub fn early_page_layout(&self) -> Option<&crate::typesetting::display::PageDisplayList> {
        self.early.as_ref().map(|e| &e.page)
    }
    pub fn capture_page(
        &self,
        page: usize,
        cache: &mut crate::fonts::outlines::OutlineCache,
    ) -> Result<crate::pdf::preview::RetainedPreview> {
        self.capture_page_with_images(
            page,
            cache,
            &mut crate::pdf::preview_images::ImageCache::default(),
        )
    }
    pub fn capture_page_with_images(
        &self,
        page: usize,
        cache: &mut crate::fonts::outlines::OutlineCache,
        images: &mut crate::pdf::preview_images::ImageCache,
    ) -> Result<crate::pdf::preview::RetainedPreview> {
        if !self.current_valid
            || self.catalog_epoch != self.host.epoch()
            || self.resource_request.is_some()
            || self
                .engine
                .as_ref()
                .is_some_and(|engine| engine.core.failure.is_some())
        {
            return Err(Error::value("Current page execution unavailable"));
        }
        if let Some(early) = self.early.as_ref().filter(|e| {
            e.page.page == page
                && self.settling().map_or(1, |s| s.pass) == e.pass
                && !(self.config.paragraph_preview
                    && self
                        .engine
                        .as_ref()
                        .is_some_and(|e| e.completed_page_count() >= page))
        }) {
            return Ok(early.scene.clone());
        }
        let engine = self.engine()?;
        if !self.config.exact_local_updates && !self.config.paragraph_preview {
            return crate::pdf::preview::capture_page_with_images(engine, page, cache, images);
        }
        let (backend, fonts, prepared) = engine.prepare_preview_page(page)?;
        let mut scene =
            crate::pdf::preview::capture_prepared(&backend, &fonts, &prepared, cache, images)?;
        if self.config.paragraph_preview {
            let document = self.document.as_ref().unwrap();
            let base = crate::paragraph_preview::Base::capture(
                document.tag.document.clone(),
                document.tag.revision,
                self.settling().map_or(1, |s| s.pass),
                prepared
                    .lines
                    .iter()
                    .flat_map(|line| line.preview_spans.iter())
                    .filter_map(|span| {
                        let file = &span.owner.region.file;
                        self.host
                            .source_bytes(Path::new(file))
                            .map(|bytes| (file.clone(), bytes))
                    })
                    .collect(),
                &prepared,
                &mut scene,
            )?;
            let mut bases = self.preview_bases.borrow_mut();
            bases.retain(|b| b.document != base.document || b.page.page != page);
            bases.push_back(Rc::new(base));
            while bases.len() > 4 {
                bases.pop_front();
            }
        }
        let recipe_started = self.host.measurement_nanos();
        if self.config.exact_local_updates
            && self.settling().map_or(1, |s| s.pass) == 1
            && !self.host.main_queried()
            && self.clock_baseline == self.host.clock_observations()
        {
            let evidence = engine.core.local.evidence.borrow();
            for ready in &engine.core.local.ready {
                if evidence.misses.contains_key(&ready.candidate)
                    || !prepared
                        .lines
                        .iter()
                        .any(|l| l.local_spans.iter().any(|s| s.candidate == ready.candidate))
                {
                    continue;
                }
                let base = crate::local_page::Base {
                    tag: self
                        .tag()
                        .cloned()
                        .ok_or_else(|| Error::value("Missing page revision"))?,
                    pass: 1,
                    ready: ready.clone(),
                    epoch: self.host.epoch(),
                    policy: self.host.policy_version(),
                    clock: self.host.clock_bucket(),
                    wall: (self.inherited_wall
                        || self.wall_baseline != self.host.wall_observations())
                    .then(|| self.host.wall_identity()),
                    backend: backend.clone(),
                    fonts: fonts.detached_clone()?,
                    env: engine.env.clone(),
                    page: prepared.clone(),
                    scene: scene.clone(),
                    sites: evidence.paint.clone(),
                    peak_nodes: engine.builder.peak_nodes,
                    root_copies: *evidence.root_copies.get(&ready.candidate).unwrap_or(&0),
                    last_line_observed: evidence.last_line_observed.contains(&ready.candidate),
                    badness_observed: evidence.badness_observed.contains(&ready.candidate),
                };
                let mut bases = self.local_bases.borrow_mut();
                bases.retain(|old| {
                    old.ready.origin.path != base.ready.origin.path
                        || old.ready.source_lines != base.ready.source_lines
                        || old.page.page != page
                });
                bases.push_back(Rc::new(base));
                while bases.len() > 8 {
                    bases.pop_front();
                }
            }
        }
        self.local_recipe_ms
            .set(self.local_recipe_ms.get() + self.elapsed_ms(recipe_started));
        Ok(scene)
    }
    /// Opt in before advancing the first pass. Paths are caller-selected byte
    /// files, not extensions inferred by the semantic engine. No pass publishes.
    pub fn begin_settling(
        &mut self,
        document: &str,
        revision: u64,
        paths: &[PathBuf],
        max_passes: usize,
    ) -> Result<()> {
        if self.tag()
            != Some(&DocumentRevision {
                document: document.into(),
                revision,
            })
            || !self.current_valid
            || !self.active
            || self.resource_request.is_some()
            || self.settling.is_some()
            || !self.settling_configurable
            || !(1..=16).contains(&max_passes)
            || paths.is_empty()
            || paths.len() > 256
        {
            return Err(Error::value("Invalid auxiliary settling request"));
        }
        let mut inputs = BTreeMap::new();
        for path in paths {
            self.host.validate_document_path(path)?;
            if self.document.as_ref().unwrap().path == *path {
                return Err(Error::value("Document source cannot be a settling output"));
            }
            let initial = self
                .working_auxiliary
                .as_ref()
                .filter(|_| self.entry_inputs.is_some())
                .and_then(|working| working.inputs.get(path))
                .cloned()
                .unwrap_or(self.host.committed(path)?);
            if inputs.insert(path.clone(), initial).is_some() {
                return Err(Error::value("Duplicate settling path"));
            }
        }
        if self.entry_inputs.is_some()
            && self
                .working_auxiliary
                .as_ref()
                .is_some_and(|w| w.inputs.keys().ne(inputs.keys()))
        {
            let document = self.document.as_ref().unwrap();
            let (tag, path, bytes) = (
                document.tag.clone(),
                document.path.clone(),
                document.bytes.clone(),
            );
            self.working_auxiliary = None;
            self.cancel_current();
            self.start_revision(tag, &path, bytes)?;
            for (path, initial) in &mut inputs {
                *initial = self.host.committed(path)?;
            }
        }
        self.settling = Some(Settling {
            observation: SettlingObservation {
                pass: 1,
                max_passes,
                status: "running".into(),
                generations: Vec::new(),
                input_generation: self.observations.auxiliary_input_generation,
                output_generation: None,
            },
            inputs,
            pass_inputs: self.entry_inputs.clone(),
        });
        Ok(())
    }
    pub fn load_initialized(
        &mut self,
        state: crate::serialization::formats::FormatSnapshot,
        directory: &Path,
    ) -> Result<()> {
        if self.document.is_some() {
            return Err(Error::value(
                "Load initialized state before opening a document",
            ));
        }
        let mut config = self.config.clone();
        config.ini = true;
        self.host.reset_resources();
        let mut engine = Engine::new_with_host(config, self.host.clone())?;
        engine.restore_initialized_state(state.detached_clone()?, directory)?;
        self.baseline_clock = Some(self.host.clock_bucket());
        self.baseline_resources = Some(self.host.checkpoint_resources());
        self.initialized = Some(state);
        self.baseline = Some(engine);
        Ok(())
    }
    /// Retain only checkpoints whose byte, metadata and directory observations
    /// still hold after an atomic delta. Lookup policy changes invalidate all.
    pub fn update_resources(&mut self, delta: CatalogDelta) -> Result<()> {
        // Catalog preflight failures must not destroy a valid active revision.
        // The host's apply performs atomic validation; active effects prevent
        // direct application. A session validates first on a catalog-only copy.
        self.host.validate_delta(&delta)?;
        let replaces_auxiliary = self.working_auxiliary.as_ref().is_some_and(|working| {
            delta.changes.iter().any(|change| {
                let path = match change {
                    ResourceChange::Put { path, .. }
                    | ResourceChange::PutWithMetadata { path, .. }
                    | ResourceChange::Remove(path) => path,
                };
                working
                    .inputs
                    .keys()
                    .any(|declared| resource_path(declared) == *path)
            })
        });
        self.cancel_current();
        self.host.apply(delta)?;
        if replaces_auxiliary {
            self.working_auxiliary = None;
        }
        let main = self.document.as_ref().map(|d| d.path.as_path());
        let baseline_valid = self
            .baseline_resources
            .as_ref()
            .is_some_and(|r| self.host.resources_valid(r, None));
        // A changed executed preamble is a fresh-document boundary. This also
        // preserves the established preamble-resource invalidation contract;
        // body resources can roll back to that boundary independently.
        if !baseline_valid
            || self
                .preamble_resources
                .as_ref()
                .is_some_and(|r| !self.host.resources_valid(r, main))
        {
            self.checkpoints.clear();
            self.preamble_resources = None;
        } else {
            self.checkpoints
                .retain(|checkpoint| self.host.resources_valid(&checkpoint.resources, main));
        }
        if !baseline_valid {
            self.baseline = None;
        }
        self.observations.checkpoint_count = self.checkpoints.len();
        self.catalog_epoch = self.host.epoch();
        Ok(())
    }
    fn ensure_baseline(&mut self) -> Result<()> {
        if self.baseline_clock != Some(self.host.clock_bucket())
            || self
                .baseline_resources
                .as_ref()
                .is_some_and(|r| !self.host.resources_valid(r, None))
        {
            self.baseline = None;
        }
        if self.baseline.is_none() {
            let mut config = self.config.clone();
            config.ini |= self.initialized.is_some();
            self.host.reset_resources();
            let mut engine = Engine::new_with_host(config, self.host.clone())?;
            if let Some(state) = &self.initialized {
                engine.restore_initialized_state(
                    state.detached_clone()?,
                    Path::new("/bundle/formats"),
                )?;
            }
            self.baseline = Some(engine);
            self.baseline_resources = Some(self.host.checkpoint_resources());
            self.baseline_clock = Some(self.host.clock_bucket());
        }
        Ok(())
    }
    pub fn open_document(
        &mut self,
        document: &str,
        revision: u64,
        path: &Path,
        bytes: Arc<[u8]>,
    ) -> Result<()> {
        self.open_project_document(document, document, revision, path, bytes)
    }
    pub fn open_project_document(
        &mut self,
        project: &str,
        document: &str,
        revision: u64,
        path: &Path,
        bytes: Arc<[u8]>,
    ) -> Result<()> {
        self.restoration_timings = Default::default();
        let started = self.host.measurement_nanos();
        if project.is_empty()
            || project.len() > 1024
            || document.is_empty()
            || document.len() > 1024
            || revision == 0
            || bytes.len() > 64 * 1024 * 1024
        {
            return Err(Error::value(
                "Invalid document identity, revision or byte budget",
            ));
        }
        if let Some(old) = &self.document {
            if old.tag.document == document && revision <= old.tag.revision {
                return Err(Error::value("Stale document revision"));
            }
        }
        self.host.validate_document_path(path)?;
        if self.catalog_epoch != self.host.epoch() {
            self.checkpoints.clear();
            self.baseline = None;
            self.preamble_resources = None;
        }
        let same = self.document.as_ref().is_some_and(|old| {
            self.project == project && old.tag.document == document && old.path == path
        });
        self.edit_path = path.to_string_lossy().into_owned();
        self.edit_anchor = if same {
            let old = &self.document.as_ref().unwrap().bytes;
            old.iter()
                .zip(bytes.iter())
                .position(|(a, b)| a != b)
                .unwrap_or(old.len().min(bytes.len()))
        } else {
            0
        };
        #[cfg(any(feature = "latency-diagnostics", feature = "output-replay-control"))]
        if let Some(anchor) = self.investigation_anchor {
            self.edit_anchor = anchor;
        }
        if !same {
            self.checkpoints.clear();
            self.preamble_resources = None;
        }
        let tag = DocumentRevision {
            document: document.into(),
            revision,
        };
        let next_version = self
            .source_version
            .max(self.host.resource_version(path).unwrap_or(0))
            .checked_add(1)
            .ok_or_else(|| Error::value("Source resource version overflow"))?;
        let epoch = self.host.epoch();
        let delta = CatalogDelta {
            base_epoch: epoch,
            new_epoch: epoch
                .checked_add(1)
                .ok_or_else(|| Error::value("Catalog epoch overflow"))?,
            policy_version: self.host.policy_version(),
            changes: vec![ResourceChange::Put {
                id: self
                    .host
                    .resource_id(path)
                    .unwrap_or_else(|| ResourceId(path.to_string_lossy().into())),
                path: resource_path(path),
                version: next_version,
                bytes: bytes.clone(),
            }],
        };
        self.host.validate_delta(&delta)?;
        self.restoration_timings.preflight_ms = self.elapsed_ms(started);
        let started = self.host.measurement_nanos();
        self.cancel_current();
        self.restoration_timings.cancellation_ms = self.elapsed_ms(started);
        let started = self.host.measurement_nanos();
        if self.project != project {
            self.working_auxiliary = None;
        }
        self.project = project.into();
        self.source_version = next_version;
        self.host.apply(delta)?;
        self.catalog_epoch = self.host.epoch();
        self.restoration_timings.catalog_ms = self.elapsed_ms(started);
        self.start_revision(tag, path, bytes)
    }
    /// Resource replies resume the same accepted revision through the same
    /// guarded checkpoint selection as edits, without republishing its source.
    fn start_revision(
        &mut self,
        tag: DocumentRevision,
        path: &Path,
        bytes: Arc<[u8]>,
    ) -> Result<()> {
        let started = self.host.measurement_nanos();
        if self
            .baseline_resources
            .as_ref()
            .is_some_and(|r| !self.host.resources_valid(r, None))
        {
            self.checkpoints.clear();
            self.baseline = None;
            self.preamble_resources = None;
        }
        if self.working_auxiliary.as_ref().is_some_and(|w| {
            !self.config.persistent_auxiliary_inputs
                || w.document != tag.document
                || w.path != path
                || !self.host.auxiliary_resources_valid(&w.resources, path)
        }) {
            self.working_auxiliary = None;
        }
        let next_inputs = self.working_auxiliary.as_ref().map(|w| w.seed.clone());
        let same_inputs = match (&self.entry_inputs, &next_inputs) {
            (None, None) => true,
            (Some(a), Some(b)) => a.same_input_files(b),
            _ => false,
        };
        if !same_inputs {
            self.local_bases.borrow_mut().clear();
        }
        self.entry_inputs = next_inputs;
        self.observations.auxiliary_input_generation =
            self.working_auxiliary.as_ref().map(|w| w.generation);
        self.observations.auxiliary_input_revision =
            self.working_auxiliary.as_ref().map(|w| w.revision);
        self.observations.auxiliary_input_pass = self.working_auxiliary.as_ref().map(|w| w.pass);
        self.observations.auxiliary_input_stable =
            self.working_auxiliary.as_ref().map(|w| w.stable);
        if let Some(seed) = &self.entry_inputs {
            self.host.restore_effects(seed, tag.clone())?;
        }
        let main = path.to_string_lossy().into_owned();
        // Only exact physical prefixes qualify. A line boundary never keeps a
        // buffered suffix from the previous revision or cuts UTF-8/CRLF input.
        let selected = self
            .checkpoints
            .iter()
            .enumerate()
            .rev()
            .find(|(_, checkpoint)| {
                (match (&checkpoint.pass_inputs, &self.entry_inputs) {
                    (None, None) => true,
                    (Some(old), Some(new)) => checkpoint.effects.can_rebase_inputs(old, new),
                    (None, Some(_)) => {
                        self.host.read_only_fits_current(&checkpoint.effects)
                            && self
                                .host
                                .resources_match_effect_inputs(&checkpoint.resources, path)
                    }
                    _ => false,
                }) && {
                    #[cfg(any(feature = "latency-diagnostics", feature = "output-replay-control"))]
                    {
                        crate::investigation_checkpoint::allowed(checkpoint.offset)
                            && self
                                .investigation_pin
                                .is_none_or(|id| id == checkpoint.investigation_id)
                    }
                    #[cfg(not(any(
                        feature = "latency-diagnostics",
                        feature = "output-replay-control"
                    )))]
                    {
                        true
                    }
                } && checkpoint.offset <= bytes.len()
                    && checkpoint.source[..checkpoint.offset] == bytes[..checkpoint.offset]
                    && checkpoint.clock_bucket == self.host.clock_bucket()
                    && self.checkpoint_sources_valid(checkpoint)
                    && checkpoint
                        .wall_identity
                        .is_none_or(|identity| identity == self.host.wall_identity())
            })
            .map(|(i, _)| i);
        #[cfg(any(feature = "latency-diagnostics", feature = "output-replay-control"))]
        if self.investigation_pin.is_some() && selected.is_none() {
            return Err(Error::value(
                "Pinned diagnostic checkpoint failed normal guards",
            ));
        }
        self.restoration_timings.selection_ms = self.elapsed_ms(started);
        self.observations.restored_offset = 0;
        self.observations.restored_file = None;
        self.observations.restored_occurrence = None;
        self.observations.restored_kind = None;
        self.observations.checkpoint_rejection = selected
            .is_none()
            .then(|| "no_compatible_source_resource_clock_effect_boundary".into());
        self.observations.restored_auxiliary_checkpoint = false;
        self.observations.restored_local_checkpoint = None;
        self.checkpoint_capture_ms = 0.0;
        self.observations.restored_pages = 0;
        self.observations.page_boundaries.clear();
        self.observations.page_restores.clear();
        let result = (|| -> Result<Engine> {
            if let Some(index) = selected {
                let started = self.host.measurement_nanos();
                let checkpoint = &self.checkpoints[index];
                match (&checkpoint.pass_inputs, &self.entry_inputs) {
                    (Some(old), Some(new)) => {
                        let effects =
                            checkpoint.effects.rebase_inputs(old, new).ok_or_else(|| {
                                Error::value("Working-input checkpoint failed rebasing")
                            })?;
                        self.host.restore_effects(&effects, tag.clone())?;
                        self.observations.auxiliary_checkpoint_reuses += 1;
                        self.observations.restored_auxiliary_checkpoint = true;
                    }
                    (None, Some(_)) => {
                        if !self
                            .host
                            .restore_read_only_for_pass(&checkpoint.effects, &tag)?
                        {
                            return Err(Error::value(
                                "Working-input read-only checkpoint failed restoration",
                            ));
                        }
                    }
                    _ => self
                        .host
                        .restore_effects(&checkpoint.effects, tag.clone())?,
                }
                self.restoration_timings.effects_ms = self.elapsed_ms(started);
                let started = self.host.measurement_nanos();
                self.host.restore_source_resources(
                    &checkpoint.resources,
                    &checkpoint
                        .readers
                        .iter()
                        .map(|r| PathBuf::from(&r.path))
                        .collect::<Vec<_>>(),
                );
                self.restoration_timings.resources_ms = self.elapsed_ms(started);
                let started = self.host.measurement_nanos();
                let mut engine = checkpoint
                    .engine
                    .checkpoint_clone()?
                    .ok_or_else(|| Error::value("Stored checkpoint cannot resume"))?;
                self.restoration_timings.engine_clone_ms = self.elapsed_ms(started);
                let started = self.host.measurement_nanos();
                engine.replace_source_bytes(&self.rebound_sources(checkpoint))?;
                if self.config.exact_local_updates {
                    engine.local_origin(
                        checkpoint.local_id,
                        checkpoint.offset,
                        bytes.clone(),
                        main.clone(),
                    );
                }
                self.restoration_timings.input_rebind_ms = self.elapsed_ms(started);
                use crate::resources::host::Host;
                engine.core.pdf_utilities.creation_time = self.host.now();
                self.observations.restored_offset = checkpoint.offset;
                self.observations.restored_file = Some(checkpoint.location.clone());
                self.observations.restored_occurrence = Some(checkpoint.occurrence);
                self.observations.restored_file_offset =
                    checkpoint.readers.last().map_or(0, |r| r.offset);
                self.observations.restored_kind = Some(
                    if checkpoint.body_boundary {
                        "body"
                    } else if checkpoint.page_boundary {
                        "page"
                    } else {
                        "command"
                    }
                    .into(),
                );
                self.observations.restored_local_checkpoint =
                    self.config.exact_local_trace.then_some(checkpoint.local_id);
                #[cfg(any(feature = "latency-diagnostics", feature = "output-replay-control"))]
                {
                    self.observations.restored_checkpoint = Some(checkpoint.investigation_id);
                }
                self.observations.restored_pages = engine.layout.pages.len();
                if checkpoint.body_boundary {
                    self.observations.body_checkpoint_reuses += 1;
                }
                if checkpoint.page_boundary {
                    self.observations.page_checkpoint_reuses += 1;
                    self.observations
                        .page_restores
                        .push(PageRollbackObservation {
                            pass: 1,
                            input_offset: checkpoint.offset,
                            completed_pages: engine.layout.pages.len(),
                        });
                }
                self.inherited_wall = checkpoint.wall_identity.is_some();
                self.paragraph_seen = checkpoint.paragraph_seen;
                let mut position = 0;
                let started = self.host.measurement_nanos();
                let progressive = self.config.progressive_pages;
                self.retain_checkpoints(|checkpoint| {
                    let retain = position <= index
                        || checkpoint.later_pass_pinned
                        || ((checkpoint.page_boundary || checkpoint.body_boundary)
                            && (progressive || checkpoint.pass_inputs.is_some()));
                    position += 1;
                    retain
                });
                self.restoration_timings.retention_ms = self.elapsed_ms(started);
                Ok(engine)
            } else {
                self.checkpoints.clear();
                self.inherited_wall = false;
                self.paragraph_seen = false;
                self.preamble_resources = None;
                self.ensure_baseline()?;
                self.host
                    .restore_resources(self.baseline_resources.as_ref().unwrap());
                if let Some(seed) = &self.entry_inputs {
                    self.host.restore_effects(seed, tag.clone())?;
                } else {
                    self.host.begin_effects(tag.clone(), 64 * 1024 * 1024)?;
                }
                let mut engine = self
                    .baseline
                    .as_ref()
                    .unwrap()
                    .checkpoint_clone()?
                    .ok_or_else(|| Error::value("Initialized engine cannot be captured"))?;
                if !engine.start_input(path) {
                    return Err(Error::value(
                        engine
                            .core
                            .input
                            .last_input_error
                            .clone()
                            .unwrap_or_else(|| "Cannot open document".into()),
                    ));
                }
                self.host.observe_main(path);
                self.host.take_console();
                use crate::resources::host::Host;
                engine.core.extended.timer_start = self.host.monotonic_nanos();
                engine.core.pdf_utilities.creation_time = self.host.now();
                self.clock_baseline = self.host.clock_observations();
                self.wall_baseline = self.host.wall_observations();
                engine.begin_run()?;
                self.observations.initialized_restarts += 1;
                Ok(engine)
            }
        })();
        match result {
            Ok(mut engine) => {
                engine.core.reuse.grouped.host = Rc::downgrade(&self.host);
                engine.core.reuse.edited_offset = if self.document.as_ref().is_some_and(|old| {
                    old.tag.document == tag.document
                        && old.path == path
                        && (old.bytes.len() != bytes.len()
                            || self.edit_anchor < old.bytes.len().min(bytes.len()))
                }) {
                    Some(self.edit_anchor)
                } else {
                    None
                };
                #[cfg(feature = "latency-diagnostics")]
                if let Some(offset) = engine.core.reuse.edited_offset {
                    crate::local_update_audit::target(
                        path.to_string_lossy().into_owned(),
                        1 + bytes[..offset.min(bytes.len())]
                            .iter()
                            .filter(|&&b| b == b'\n')
                            .count(),
                    );
                }
                self.engine = Some(engine);
                self.document = Some(Document {
                    tag,
                    path: path.into(),
                    bytes,
                });
                self.active = true;
                self.pass_finished_pending = false;
                self.settling_configurable = true;
                self.current_valid = true;
                self.last_offset = self.observations.restored_offset;
                self.last_physical = self
                    .engine
                    .as_ref()
                    .and_then(|e| e.checkpoint_sources())
                    .and_then(|sources| {
                        sources
                            .last()
                            .map(|r| (r.path.clone(), r.occurrence, r.offset))
                    });
                self.last_checkpoint_pages = self.engine.as_ref().unwrap().layout.pages.len();
                self.returned_input_pending = false;
                if self.observations.restored_offset > 0 {
                    self.host.observe_main(path);
                    self.clock_baseline = self.host.clock_observations();
                    self.wall_baseline = self.host.wall_observations();
                }
                self.observations.checkpoint_count = self.checkpoints.len();
                Ok(())
            }
            Err(error) => {
                self.host.discard_effects();
                self.engine = None;
                Err(error)
            }
        }
    }
    pub fn validate_source_transaction(&self, tx: SourceTransaction) -> Result<()> {
        self.prepare_source_transaction(tx).map(|_| ())
    }
    fn prepare_source_transaction(
        &self,
        tx: SourceTransaction,
    ) -> Result<PreparedSourceTransaction> {
        let old = self
            .document
            .as_ref()
            .ok_or_else(|| Error::value("No open document"))?;
        if tx.project != self.project
            || tx.document != old.tag.document
            || tx.base_revision != old.tag.revision
            || tx.revision <= tx.base_revision
            || tx.catalog_epoch != self.host.epoch()
            || tx.changes.len() > 16000
        {
            return Err(Error::value(
                "Stale source transaction identity, revision or catalog epoch",
            ));
        }
        let path = old.path.clone();
        let mut root = old.bytes.clone();
        let mut changes = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut anchor = None;
        for change in tx.changes {
            let physical = Path::new(&change.path);
            self.host.validate_document_path(physical)?;
            if !change.path.starts_with("/project/")
                || !seen.insert(change.path.clone())
                || self.host.resource_id(physical).as_ref().map(|id| &id.0) != Some(&change.id)
                || self.host.resource_version(physical) != Some(change.expected_version)
                || change.version <= change.expected_version
                || change.replacements.len() > 100000
            {
                return Err(Error::value(
                    "Invalid source identity, version or duplicate path",
                ));
            }
            let before = self
                .host
                .source_bytes(physical)
                .ok_or_else(|| Error::value("Source unavailable"))?;
            let mut bytes = Vec::new();
            let mut end = 0;
            for replacement in change.replacements {
                if replacement.start < end
                    || replacement.start > replacement.end
                    || replacement.end > before.len()
                {
                    return Err(Error::value(
                        "Source ranges must be ordered, disjoint and in bounds",
                    ));
                }
                if anchor.is_none() {
                    anchor = Some((change.path.clone(), replacement.start));
                }
                bytes.extend_from_slice(&before[end..replacement.start]);
                bytes.extend_from_slice(&replacement.bytes);
                end = replacement.end;
                if bytes.len() > 64 * 1024 * 1024 {
                    return Err(Error::value("Source byte budget exceeded"));
                }
            }
            bytes.extend_from_slice(&before[end..]);
            if bytes.len() > 64 * 1024 * 1024 {
                return Err(Error::value("Source byte budget exceeded"));
            }
            let bytes: Arc<[u8]> = bytes.into();
            if physical == path {
                root = bytes.clone();
            }
            changes.push(
                if let Some(modified) = self.host.source_modified(physical) {
                    ResourceChange::PutWithMetadata {
                        path: resource_path(physical),
                        id: ResourceId(change.id),
                        version: change.version,
                        bytes,
                        modified,
                    }
                } else {
                    ResourceChange::Put {
                        path: resource_path(physical),
                        id: ResourceId(change.id),
                        version: change.version,
                        bytes,
                    }
                },
            );
        }
        let epoch = self.host.epoch();
        let delta = CatalogDelta {
            base_epoch: epoch,
            new_epoch: epoch
                .checked_add(1)
                .ok_or_else(|| Error::value("Catalog epoch overflow"))?,
            policy_version: self.host.policy_version(),
            changes,
        };
        self.host.validate_delta(&delta)?;
        Ok(PreparedSourceTransaction {
            delta,
            path,
            root,
            anchor,
            tag: DocumentRevision {
                document: tx.document,
                revision: tx.revision,
            },
        })
    }
    pub fn set_checkpoint_focus(
        &mut self,
        document: &str,
        path: &Path,
        offset: usize,
    ) -> Result<()> {
        if self.tag().is_none_or(|tag| tag.document != document) {
            return Err(Error::value("Checkpoint focus document unavailable"));
        }
        self.host.validate_document_path(path)?;
        let bytes = self
            .host
            .source_bytes(path)
            .ok_or_else(|| Error::value("Checkpoint focus source unavailable"))?;
        if offset > bytes.len() {
            return Err(Error::value("Checkpoint focus offset outside source"));
        }
        self.edit_path = path.to_string_lossy().into_owned();
        self.edit_anchor = offset;
        Ok(())
    }
    pub fn apply_source_transaction(&mut self, tx: SourceTransaction) -> Result<()> {
        let update = self.prepare_source_transaction(tx)?;
        self.restoration_timings = Default::default();
        let started = self.host.measurement_nanos();
        self.cancel_current();
        self.restoration_timings.cancellation_ms = self.elapsed_ms(started);
        self.host.apply(update.delta)?;
        self.catalog_epoch = self.host.epoch();
        if let Some((path, anchor)) = update.anchor {
            self.edit_path = path;
            self.edit_anchor = anchor;
        }
        self.start_revision(update.tag, &update.path, update.root)
    }
    fn checkpoint_sources_valid(&self, checkpoint: &LiveCheckpoint) -> bool {
        let input = &checkpoint.engine.core.input;
        let mut exempt = Vec::new();
        for reader in &checkpoint.readers {
            let path = Path::new(&reader.path);
            let Some(bytes) = self.host.source_bytes(path) else {
                return false;
            };
            let active = checkpoint
                .readers
                .iter()
                .filter(|r| r.path == reader.path)
                .count() as u64;
            // A completed earlier occurrence can have consumed arbitrary bytes.
            let completed = input
                .source_occurrences
                .get(&reader.path)
                .copied()
                .unwrap_or(0)
                > active;
            // Decoded/current and suspended physical lines are immutable prefix.
            // The raw read-ahead window is separately owned by an input-only
            // inode (catalog_bytes proved this at capture). Rebinding refreshes
            // that entire window without moving its logical cursor. A mutable
            // or aliased output inode cannot produce this witness.
            let limit = if completed {
                reader.bytes.len()
            } else {
                reader.offset
            };
            if limit > bytes.len()
                || limit > reader.bytes.len()
                || reader.bytes[..limit] != bytes[..limit]
                || (completed && bytes.len() != reader.bytes.len())
            {
                return false;
            }
            exempt.push(path.to_path_buf());
        }
        // Explicit open streams are not physical source continuations. Even an
        // unread stream must keep its original bytes unless closed before here.
        for stream in checkpoint.engine.core.streams.readers.values() {
            if let Some(path) = stream.handle.physical_path() {
                if exempt.iter().any(|p| p == path)
                    && stream
                        .handle
                        .catalog_bytes()
                        .zip(self.host.source_bytes(path))
                        .is_none_or(|(a, b)| a != b)
                {
                    return false;
                }
            }
        }
        self.host
            .resources_valid_sources(&checkpoint.resources, &exempt)
    }
    fn rebound_sources(&self, checkpoint: &LiveCheckpoint) -> Vec<(usize, String, Arc<[u8]>)> {
        checkpoint
            .readers
            .iter()
            .filter_map(|reader| {
                self.host
                    .source_bytes(Path::new(&reader.path))
                    .map(|bytes| (reader.slot, reader.path.clone(), bytes))
            })
            .collect()
    }
    pub fn apply_edit(
        &mut self,
        document: &str,
        base_revision: u64,
        start: usize,
        end: usize,
        replacement: &[u8],
        revision: u64,
    ) -> Result<()> {
        let old = self
            .document
            .as_ref()
            .ok_or_else(|| Error::value("Open a document before applying edits"))?;
        if old.tag.document != document || old.tag.revision != base_revision {
            return Err(Error::value(
                "Stale edit base; resynchronize document bytes",
            ));
        }
        if revision <= base_revision || start > end || end > old.bytes.len() {
            return Err(Error::value("Invalid edit range or revision"));
        }
        let length = (old.bytes.len() - (end - start))
            .checked_add(replacement.len())
            .ok_or_else(|| Error::value("Edit length overflow"))?;
        if length > 64 * 1024 * 1024 {
            return Err(Error::value("Document byte budget exceeded"));
        }
        let path = old.path.clone();
        let mut bytes = Vec::with_capacity(length);
        bytes.extend_from_slice(&old.bytes[..start]);
        bytes.extend_from_slice(replacement);
        bytes.extend_from_slice(&old.bytes[end..]);
        self.open_document(document, revision, &path, bytes.into())
    }
    /// Required inputs can wait for a revision-tagged host reply. The failed
    /// engine/effects remain private so denial retains reference diagnostics.
    /// Supply replays from the latest compatible quiescent checkpoint, or from
    /// initialized state when the changed resource affects the preamble.
    pub fn advance_interactive(&mut self, max_steps: usize) -> Result<SessionProgress> {
        if let Some(request) = &self.resource_request {
            return Ok(SessionProgress::NeedResource(request.clone()));
        }
        let progress = self.advance(max_steps)?;
        if progress == RunProgress::Failed {
            if let Some(resource) = self
                .engine()?
                .core
                .failure
                .as_ref()
                .and_then(|e| e.missing_resource.clone())
            {
                if resource.name.len() <= 4096
                    && resource.directory.as_ref().is_none_or(|p| p.len() <= 4096)
                {
                    let tag = self.tag().unwrap();
                    let request = ResourceRequest {
                        request_id: self.next_request_id,
                        document: tag.document.clone(),
                        revision: tag.revision,
                        catalog_epoch: self.host.epoch(),
                        resource,
                    };
                    self.next_request_id = self
                        .next_request_id
                        .checked_add(1)
                        .ok_or_else(|| Error::value("Resource request identity overflow"))?;
                    self.resource_request = Some(request.clone());
                    return Ok(SessionProgress::NeedResource(request));
                }
            }
        }
        Ok(SessionProgress::Running(progress))
    }
    pub fn needed_resource(&self) -> Option<&ResourceRequest> {
        self.resource_request.as_ref()
    }
    pub fn resolve_resource(
        &mut self,
        document: &str,
        revision: u64,
        request_id: u64,
        delta: Option<CatalogDelta>,
    ) -> Result<()> {
        let request = self
            .resource_request
            .as_ref()
            .ok_or_else(|| Error::value("No pending resource request"))?;
        if request.document != document
            || request.revision != revision
            || request.request_id != request_id
            || request.catalog_epoch != self.host.epoch()
        {
            return Err(Error::value("Stale resource request reply"));
        }
        if let Some(delta) = delta {
            let main = resource_path(&self.document.as_ref().unwrap().path);
            for change in &delta.changes {
                let path = match change {
                    ResourceChange::Put { path, .. }
                    | ResourceChange::PutWithMetadata { path, .. }
                    | ResourceChange::Remove(path) => path,
                };
                if self.host.normalize_resource_path(path)? == main {
                    return Err(Error::value(
                        "Resource replies cannot replace the document source",
                    ));
                }
            }
            // Preflight validation must preserve waiting state and private effects.
            self.host.validate_delta(&delta)?;
            let document = self.document.as_ref().unwrap();
            let (tag, path, bytes) = (
                document.tag.clone(),
                document.path.clone(),
                document.bytes.clone(),
            );
            // A new resource generation restarts the complete private chain.
            // Earlier pass inputs were computed with the previous resources.
            let settling = self.settling.as_ref().map(|s| {
                (
                    s.inputs.keys().cloned().collect::<Vec<_>>(),
                    s.observation.max_passes,
                )
            });
            self.update_resources(delta)?;
            self.start_revision(tag.clone(), &path, bytes)?;
            if let Some((paths, max_passes)) = settling {
                self.begin_settling(&tag.document, tag.revision, &paths, max_passes)?;
            }
        } else {
            self.resource_request = None;
            if let Some(engine) = self.engine.as_mut() {
                if engine.retry_recovery(true)? {
                    self.active = true;
                }
            }
        }
        Ok(())
    }
    pub fn advance(&mut self, max_steps: usize) -> Result<RunProgress> {
        #[cfg(feature = "latency-diagnostics")]
        let _output_slice = crate::output_diagnostics::slice();
        #[cfg(feature = "latency-diagnostics")]
        let _reuse_clock = crate::latency_diagnostics::execution_slice();
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase("engine_residual", "", None);
        if !self.active {
            return Err(Error::value("No active revision"));
        }
        if self.pass_finished_pending {
            self.pass_finished_pending = false;
            self.active = false;
            return if self.finish_pass()? {
                Ok(RunProgress::Yielded)
            } else {
                Ok(RunProgress::Complete)
            };
        }
        let main = self
            .document
            .as_ref()
            .unwrap()
            .path
            .to_string_lossy()
            .into_owned();
        for _ in 0..max_steps {
            #[cfg(feature = "latency-diagnostics")]
            if self.observations.execution_steps % 128 == 0 && crate::latency_diagnostics::enabled()
            {
                let engine = self.engine.as_ref().unwrap();
                crate::latency_diagnostics::switch_region(
                    crate::latency_diagnostics::region(engine.core.input.source().as_ref()),
                    engine.page_builder.active,
                    &crate::latency_diagnostics::source_chain(engine.core.input.source().as_ref()),
                );
            }
            self.settling_configurable = false;
            let input_depth = self.engine.as_ref().unwrap().core.input.in_open;
            let previous_pages = self.engine.as_ref().unwrap().completed_page_count();
            let previous_paragraphs = self.engine.as_ref().unwrap().layout.paragraph_count;
            let was_paragraph = self.engine.as_ref().unwrap().builder.context.mode
                == crate::core::enums::ListMode::Horizontal
                && !self.engine.as_ref().unwrap().builder.context.restricted;
            if self.config.exact_local_updates
                && was_paragraph
                && self.local_paragraph_started.is_none()
            {
                self.local_paragraph_started = Some(self.host.measurement_nanos());
            }
            let preview_step_started = self
                .config
                .paragraph_preview_timings
                .then(crate::local_investigation::now);
            let progress = self.engine.as_mut().unwrap().advance_run(1)?;
            if let Some(started) = preview_step_started {
                if !self.preview_published {
                    self.preview_execution_ms +=
                        crate::local_investigation::now().saturating_sub(started) as f64 / 1e6;
                }
            }
            if self.config.paragraph_preview {
                if let Some(ready) = self
                    .engine
                    .as_mut()
                    .unwrap()
                    .core
                    .paragraph_preview
                    .pending
                    .take()
                {
                    if !self.preview_published && self.settling().map_or(1, |s| s.pass) == 1 {
                        let document = self.document.as_ref().unwrap();
                        let source = self
                            .host
                            .source_bytes(Path::new(&ready.region.file))
                            .unwrap_or_default();
                        let bases: Vec<_> =
                            self.preview_displayed.borrow().iter().cloned().collect();
                        if self.preview_reason.is_none() {
                            self.preview_reason = Some("no_displayed_source_correspondence".into());
                        }
                        for base in bases.iter().rev().filter(|b| {
                            b.document == document.tag.document
                                && b.revision < document.tag.revision
                                && b.page.page == self.local_target
                        }) {
                            if self.config.paragraph_preview_timings {
                                let edit_line = 1 + source[..self.edit_anchor.min(source.len())]
                                    .iter()
                                    .filter(|&&b| b == b'\n')
                                    .count();
                                if ready.region.first_line <= edit_line
                                    && ready.region.last_line >= edit_line
                                {
                                    self.preview_mapping = base.mapping_evidence(&ready, &source);
                                }
                            }
                            if let Some(region) = base.matching(&ready, &source, self.edit_anchor) {
                                let index =
                                    base.regions.iter().position(|r| r.id == region.id).unwrap();
                                self.preview_pending = Some((base.clone(), ready, index));
                                self.preview_reason = None;
                                return Ok(RunProgress::Yielded);
                            }
                        }
                    }
                }
            }
            self.observations.execution_steps += 1;
            if self.config.exact_local_updates
                && self.early.is_none()
                && self.settling().map_or(1, |s| s.pass) == 1
            {
                if let Some(ready) = self.engine.as_mut().unwrap().core.local.pending.take() {
                    self.local_paragraph_ms = self
                        .local_paragraph_started
                        .take()
                        .map_or(0.0, |started| self.elapsed_ms(started));
                    let bases: Vec<_> = self.local_bases.borrow().iter().cloned().collect();
                    let started = self.host.measurement_nanos();
                    if self.config.exact_local_trace {
                        self.engine.as_mut().unwrap().core.local.trace(serde_json::json!({"event":"candidate_ready","new":ready.investigation(),"base_count":bases.len(),"target_page":self.local_target}));
                    }
                    for base in bases.iter().rev() {
                        let tag = self.tag().unwrap();
                        if base.tag.document != tag.document
                            || base.tag.revision >= tag.revision
                            || base.pass != 1
                        {
                            if self.config.exact_local_trace {
                                let event = serde_json::json!({"event":"base_skipped","reason":"page_revision_lineage","base_revision":base.tag.revision,"new_revision":tag.revision,"base_pass":base.pass});
                                self.engine.as_mut().unwrap().core.local.trace(event);
                            }
                            *self
                                .engine
                                .as_mut()
                                .unwrap()
                                .core
                                .local
                                .misses
                                .entry("page_revision_lineage".into())
                                .or_default() += 1;
                            continue;
                        }
                        if base.page.page != self.local_target {
                            if self.config.exact_local_trace {
                                self.engine.as_mut().unwrap().core.local.trace(serde_json::json!({"event":"base_skipped","reason":"target_page","old":base.page.page,"new":self.local_target}));
                            }
                            continue;
                        }
                        if base.ready.source_lines != ready.source_lines
                            || base.ready.origin.path != main
                        {
                            if self.config.exact_local_trace {
                                self.engine.as_mut().unwrap().core.local.trace(serde_json::json!({"event":"base_skipped","reason":"paragraph_source_correspondence","old_lines":base.ready.source_lines,"new_lines":ready.source_lines,"old_path":base.ready.origin.path,"new_path":main}));
                            }
                            continue;
                        }
                        if !self.host.only_main_changes_since(
                            base.epoch,
                            base.policy,
                            Path::new(&main),
                        ) || base.clock != self.host.clock_bucket()
                            || base
                                .wall
                                .is_some_and(|wall| wall != self.host.wall_identity())
                        {
                            if self.config.exact_local_trace {
                                self.engine.as_mut().unwrap().core.local.trace(serde_json::json!({"event":"base_skipped","reason":"catalog_or_clock_witness","base_revision":base.tag.revision}));
                            }
                            *self
                                .engine
                                .as_mut()
                                .unwrap()
                                .core
                                .local
                                .misses
                                .entry("catalog_or_clock_witness".into())
                                .or_default() += 1;
                            continue;
                        }
                        if self.clock_baseline != self.host.clock_observations()
                            || self.host.main_queried()
                        {
                            if self.config.exact_local_trace {
                                self.engine.as_mut().unwrap().core.local.trace(serde_json::json!({"event":"base_skipped","reason":"ambient_observation","main_queried":self.host.main_queried(),"clock_changed":self.clock_baseline!=self.host.clock_observations()}));
                            }
                            continue;
                        }
                        let _host = crate::resources::host::enter(self.host.clone());
                        let measurement = self.host.clone();
                        let replacement =
                            base.replace(self.engine.as_mut().unwrap(), &ready, &|| {
                                measurement.measurement_nanos()
                            });
                        if self.config.exact_local_trace {
                            let font_guards_equal = base.ready.current_font
                                == self.engine.as_ref().unwrap().fonts.current
                                && base.ready.fonts.len()
                                    == self.engine.as_ref().unwrap().fonts.items.len()
                                && base
                                    .ready
                                    .fonts
                                    .iter()
                                    .zip(&self.engine.as_ref().unwrap().fonts.items)
                                    .all(|(a, b)| a.matches(b));
                            self.engine.as_mut().unwrap().core.local.trace(serde_json::json!({"event":"replacement_attempt","operation":"Base::replace / Ready::compatible","base_revision":base.tag.revision,"base_page":base.page.page,"old":base.ready.investigation(),"new":ready.investigation(),"font_guards_equal":font_guards_equal,"last_line_observed":base.last_line_observed,"badness_observed":base.badness_observed,"outcome":replacement.as_ref().map(|_|"published".to_string()).unwrap_or_else(|e|e.message.clone())}));
                        }
                        match replacement {
                            Ok(early) => {
                                self.early = Some(early);
                                self.local_hits += 1;
                                break;
                            }
                            Err(error) => {
                                let local = &mut self.engine.as_mut().unwrap().core.local;
                                *local.misses.entry(error.message).or_default() += 1;
                            }
                        }
                    }
                    self.local_cost_ms = self.elapsed_ms(started);
                    if self.early.is_some() {
                        return Ok(RunProgress::Yielded);
                    }
                }
            }
            if self.engine.as_ref().unwrap().layout.paragraph_count != previous_paragraphs {
                self.local_paragraph_started = None;
            }
            if self.config.exact_local_verify
                && !self.local_verified
                && self.early.as_ref().is_some_and(|e| {
                    e.pass == self.settling().map_or(1, |s| s.pass)
                        && e.page.page <= self.engine.as_ref().unwrap().completed_page_count()
                })
            {
                let started = self.host.measurement_nanos();
                let early = self.early.as_ref().unwrap();
                let engine = self.engine.as_ref().unwrap();
                let (backend, fonts, ordinary) = engine.prepare_preview_page(early.page.page)?;
                let scene = crate::pdf::preview::capture_prepared(
                    &backend,
                    &fonts,
                    &ordinary,
                    &mut Default::default(),
                    &mut Default::default(),
                )?;
                if scene.pages[0].content_sha256 != early.scene.pages[0].content_sha256
                    || crate::local_page::geometry(&ordinary)?
                        != crate::local_page::geometry(&early.page)?
                {
                    self.current_valid = false;
                    return Err(Error::runtime(format!(
                        "Exact local publication differs from ordinary execution; publication contract bug: {:?}",
                        crate::local_page::difference(
                            &crate::local_page::geometry(&ordinary)?,
                            &crate::local_page::geometry(&early.page)?,
                            "page"
                        )
                    )));
                }
                self.local_verified = true;
                self.local_verify_ms = self.elapsed_ms(started);
            }
            if self.config.progressive_pages
                && self.engine.as_ref().unwrap().completed_page_count() > 256
            {
                return Err(Error::value(
                    "Progressive page capacity exceeded (256 pages); native CLI export remains available",
                ));
            }
            if input_depth > 1 && self.engine.as_ref().unwrap().core.input.in_open == 1 {
                // The main line may already be buffered at its earlier offset.
                // Prefer one later quiescent snapshot after an included file
                // actually executes, without capturing every command on a line.
                self.returned_input_pending = true;
            }
            if self.observations.restored_offset > 0
                && self.clock_baseline != self.host.clock_observations()
            {
                // A time-dependent suffix makes skipping execution observable.
                // Discard its speculative state and replay this revision fresh.
                self.restart_fresh()?;
                continue;
            }
            if progress != RunProgress::Yielded {
                if progress == RunProgress::Complete && self.config.progressive_pages {
                    self.pass_finished_pending = true;
                    return Ok(RunProgress::Yielded);
                }
                self.active = false;
                if progress == RunProgress::Complete && self.finish_pass()? {
                    return Ok(RunProgress::Yielded);
                }
                if progress == RunProgress::Failed {
                    if let Some(settling) = &mut self.settling {
                        settling.observation.status = "failed".into();
                    }
                }
                return Ok(progress);
            }
            let engine = self.engine.as_ref().unwrap();
            if !self.paragraph_seen
                && engine.builder.context.mode == crate::core::enums::ListMode::Horizontal
                && !engine.builder.context.restricted
            {
                // Prefer retaining state immediately before the first ordinary
                // paragraph, regardless of the source spelling or document class.
                // This changes placement only: all ordinary reuse guards apply.
                self.paragraph_seen = true;
                self.preamble_resources = Some(self.host.checkpoint_resources());
                let first_pass = self
                    .settling
                    .as_ref()
                    .is_some_and(|settling| settling.observation.pass == 1);
                let current_inputs = self.current_pass_inputs().cloned();
                let paragraph_offset = engine
                    .core
                    .input
                    .input_file
                    .first()
                    .and_then(Option::as_ref)
                    .and_then(|input| input.byte_position())
                    .unwrap_or(0);
                let document = self.document.as_ref().unwrap();
                for checkpoint in &mut self.checkpoints {
                    checkpoint.pinned = false;
                    if first_pass {
                        checkpoint.first_pass_pinned = false;
                    } else if current_inputs.is_some() {
                        checkpoint.later_pass_pinned = false;
                    }
                }
                if let Some(checkpoint) = self.checkpoints.iter_mut().rev().find(|checkpoint| {
                    // Retained downstream slots are not preamble states.
                    // Prefer a valid prefix no later than this first
                    // paragraph's buffered input, in the current pass.
                    (!self.config.progressive_pages
                        || (checkpoint.engine.layout.pages.is_empty()
                            && checkpoint.offset <= paragraph_offset
                            && checkpoint.offset <= document.bytes.len()
                            && checkpoint.source[..checkpoint.offset]
                                == document.bytes[..checkpoint.offset]))
                        && match (&checkpoint.pass_inputs, current_inputs.as_ref()) {
                            (None, None) => true,
                            (Some(previous), Some(current)) => previous.same_pass_inputs(current),
                            _ => false,
                        }
                }) {
                    checkpoint.pinned = true;
                    if first_pass {
                        checkpoint.first_pass_pinned = true;
                    } else if current_inputs.is_some() {
                        checkpoint.later_pass_pinned = true;
                    }
                }
            }
            let completed_pages = self.engine.as_ref().unwrap().layout.pages.len();
            let page_boundary = (self.config.page_checkpoints || self.config.progressive_pages)
                && completed_pages > self.last_checkpoint_pages;
            let engine = self.engine.as_ref().unwrap();
            let in_paragraph = engine.builder.context.mode
                == crate::core::enums::ListMode::Horizontal
                && !engine.builder.context.restricted;
            // A physical line is buffered ahead of token consumption. Use the
            // reader's full byte boundary, never the displayed source location:
            // prefix matching must retain every byte already read or expanded.
            // The ordinary checkpoint/ownership guards still decide whether this
            // execution boundary can be cloned and safely resumed.
            let input = &engine.core.input;
            let slot = input.in_open.saturating_sub(1);
            let reader = input.input_file.get(slot).and_then(Option::as_ref);
            let physical = reader
                .and_then(|r| r.physical_path())
                .and_then(|p| p.to_str())
                .or_else(|| {
                    std::iter::once(&input.cursor)
                        .chain(input.input_stack.iter())
                        .find(|f| f.index == slot && f.token_list.is_none())
                        .and_then(|f| f.name.as_deref())
                });
            let physical_offset = reader.and_then(|r| r.byte_position()).unwrap_or(0);
            let occurrence = input.input_occurrences.get(slot).copied().unwrap_or(0);
            let near_focus = physical == Some(self.edit_path.as_str())
                && (self.edit_anchor == 0 || physical_offset.abs_diff(self.edit_anchor) <= 1024);
            let advanced =
                self.last_physical
                    .as_ref()
                    .is_none_or(|(path, old_occurrence, offset)| {
                        Some(path.as_str()) != physical
                            || *old_occurrence != occurrence
                            || *offset < physical_offset
                    });
            let input_advanced_in_paragraph = in_paragraph && near_focus && advanced;
            let body_boundary = self.config.progressive_pages
                && !page_boundary
                && near_focus
                && ((!was_paragraph && in_paragraph)
                    || engine.layout.paragraph_count > previous_paragraphs
                    || input_advanced_in_paragraph);
            // A returned include is useful as a nearby parent boundary, not
            // an invitation to recapture every other file on each focused edit.
            // Coarse page publication/checkpoints remain independent of focus.
            if self.returned_input_pending && !near_focus {
                self.returned_input_pending = false;
            }
            let capture_region = !self.config.progressive_pages
                || !self.paragraph_seen
                || page_boundary
                || body_boundary
                || self.returned_input_pending;
            let offset = if capture_region {
                self.engine.as_ref().unwrap().checkpoint_offset(&main)
            } else {
                None
            };
            if body_boundary
                && self.engine.as_ref().unwrap().core.input.in_open > 1
                && offset.is_none()
            {
                let reason = self.engine.as_ref().unwrap().source_checkpoint_blocker();
                *self
                    .observations
                    .checkpoint_capture_rejections
                    .entry(reason.into())
                    .or_default() += 1;
            }
            if let Some(offset) = offset.filter(|offset| {
                *offset > self.last_offset
                    || (self.returned_input_pending && *offset == self.last_offset)
                    || page_boundary
                    || body_boundary
            }) {
                let source = self.document.as_ref().unwrap().bytes.clone();
                let readers = self
                    .engine
                    .as_ref()
                    .unwrap()
                    .checkpoint_sources()
                    .unwrap_or_default();
                let location = readers.last().map_or(main.clone(), |r| r.path.clone());
                let occurrence = readers.last().map_or(0, |r| r.occurrence);
                let file_offset = readers.last().map_or(offset, |r| r.offset);
                self.last_physical = Some((location.clone(), occurrence, file_offset));
                let private = self.current_pass_inputs().is_some();
                let preceding = self.preceding_body_ids(private);
                let candidates: Vec<_> = self
                    .checkpoints
                    .iter()
                    .filter(|c| c.body_boundary && c.pass_inputs.is_some() == private)
                    .collect();
                let distance = if location == self.edit_path {
                    file_offset.abs_diff(self.edit_anchor)
                } else {
                    usize::MAX
                };
                let retain_body = !body_boundary
                    || candidates.len() < 32
                    || candidates.iter().any(|c| {
                        c.location == location
                            && c.readers.last().is_some_and(|r| r.offset == file_offset)
                    })
                    || candidates.iter().any(|c| {
                        !preceding.contains(&c.local_id)
                            && (if c.location == self.edit_path {
                                c.readers
                                    .last()
                                    .map_or(usize::MAX, |r| r.offset.abs_diff(self.edit_anchor))
                            } else {
                                usize::MAX
                            }) > distance
                    });

                let distinct = !self.config.progressive_pages
                    || page_boundary
                    || self.returned_input_pending
                    || (!was_paragraph && in_paragraph)
                    || engine.layout.paragraph_count > previous_paragraphs
                    || !self.checkpoints.iter().rev().take(1).any(|old| {
                        old.location == location
                            && old.occurrence == occurrence
                            && old.readers.last().map(|r| r.offset) == Some(file_offset)
                            && old.pass_inputs.is_some() == private
                    });
                if body_boundary && self.engine.as_ref().unwrap().core.input.in_open > 1 {
                    let reason = if readers.is_empty() {
                        Some("source_readers")
                    } else if self.host.main_queried() {
                        Some("main_query")
                    } else if self.clock_baseline != self.host.clock_observations() {
                        Some("clock")
                    } else {
                        None
                    };
                    if let Some(reason) = reason {
                        *self
                            .observations
                            .checkpoint_capture_rejections
                            .entry(reason.into())
                            .or_default() += 1;
                    }
                }
                if capture_region
                    && distinct
                    && retain_body
                    && !readers.is_empty()
                    && offset <= source.len()
                    && offset != 0
                    && (self.config.progressive_pages
                        || matches!(source[offset - 1], b'\n' | b'\r'))
                    && !self.host.main_queried()
                    && self.clock_baseline == self.host.clock_observations()
                {
                    #[cfg(feature = "latency-diagnostics")]
                    let _diag =
                        crate::latency_diagnostics::phase("checkpoint_capture_retention", "", None);
                    let capture_start = self.host.measurement_nanos();
                    let capture_count = self.observations.checkpoint_captures;
                    if let Some(engine) = self.engine.as_ref().unwrap().checkpoint_clone()? {
                        let effects = self.host.checkpoint_effects()?;
                        let pre_effect_pinned = effects.read_only();
                        if pre_effect_pinned {
                            for checkpoint in &mut self.checkpoints {
                                checkpoint.pre_effect_pinned = false;
                            }
                        }
                        if self.config.progressive_pages && page_boundary {
                            let private = self.current_pass_inputs().is_some();
                            // Replace a page slot, rather than accumulating a
                            // new document history on every keystroke/pass.
                            self.checkpoints.retain(|old| {
                                !old.page_boundary
                                    || old.engine.layout.pages.len() != completed_pages
                                    || old.pass_inputs.is_some() != private
                            });
                        }
                        if body_boundary {
                            self.retain_checkpoints(|old| {
                                !old.body_boundary
                                    || old.location != location
                                    || old.occurrence != occurrence
                                    || old.readers.last().is_none_or(|r| r.offset != file_offset)
                                    || old.pass_inputs.is_some() != private
                            });
                            let same_kind: Vec<_> = self
                                .checkpoints
                                .iter()
                                .enumerate()
                                .filter(|(_, old)| {
                                    old.body_boundary
                                        && old.pass_inputs.is_some() == private
                                        && !preceding.contains(&old.local_id)
                                })
                                .collect();
                            let total = self
                                .checkpoints
                                .iter()
                                .filter(|old| {
                                    old.body_boundary && old.pass_inputs.is_some() == private
                                })
                                .count();
                            if total >= 32 {
                                let evict = same_kind
                                    .into_iter()
                                    .max_by_key(|(_, old)| {
                                        if old.location == self.edit_path {
                                            (
                                                0,
                                                old.readers.last().map_or(0, |r| {
                                                    r.offset.abs_diff(self.edit_anchor)
                                                }),
                                            )
                                        } else {
                                            (1, usize::MAX.saturating_sub(old.local_id as usize))
                                        }
                                    })
                                    .unwrap()
                                    .0;
                                let mut index = 0;
                                self.retain_checkpoints(|_| {
                                    let keep = index != evict;
                                    index += 1;
                                    keep
                                });
                            }
                        }
                        if self.checkpoints.len() >= 256 {
                            let evict = self
                                .checkpoints
                                .iter()
                                .position(|c| {
                                    !c.pinned
                                        && !c.pre_effect_pinned
                                        && !c.first_pass_pinned
                                        && !c.later_pass_pinned
                                        && !c.body_boundary
                                })
                                .or_else(|| {
                                    self.checkpoints.iter().position(|c| {
                                        !c.pinned
                                            && !c.pre_effect_pinned
                                            && !c.first_pass_pinned
                                            && !c.later_pass_pinned
                                    })
                                })
                                .unwrap_or(0);
                            let mut index = 0;
                            self.retain_checkpoints(|_| {
                                let keep = index != evict;
                                index += 1;
                                keep
                            });
                        }
                        if self.config.exact_local_updates {
                            self.engine.as_mut().unwrap().local_origin(
                                self.observations.checkpoint_captures + 1,
                                offset,
                                source.clone(),
                                main.clone(),
                            );
                        }
                        self.checkpoints.push(LiveCheckpoint {
                            local_id: self.observations.checkpoint_captures + 1,
                            #[cfg(any(
                                feature = "latency-diagnostics",
                                feature = "output-replay-control"
                            ))]
                            investigation_id: self.observations.checkpoint_captures + 1,
                            engine,
                            effects,
                            resources: self.host.checkpoint_resources(),
                            pinned: false,
                            pre_effect_pinned,
                            first_pass_pinned: false,
                            later_pass_pinned: false,
                            page_boundary,
                            body_boundary,
                            pass_inputs: self.current_pass_inputs().cloned(),
                            paragraph_seen: self.paragraph_seen,
                            readers,
                            location,
                            occurrence,
                            offset,
                            source,
                            clock_bucket: self.host.clock_bucket(),
                            wall_identity: if self.inherited_wall
                                || self.wall_baseline != self.host.wall_observations()
                            {
                                Some(self.host.wall_identity())
                            } else {
                                None
                            },
                        });
                        // Keep the earliest boundary plus recent body boundaries.
                        if self.checkpoints.len() > 8 && !self.config.progressive_pages {
                            let unpinned = |&index: &usize| {
                                !self.checkpoints[index].pinned
                                    && !self.checkpoints[index].pre_effect_pinned
                                    && !self.checkpoints[index].first_pass_pinned
                                    && !self.checkpoints[index].later_pass_pinned
                            };
                            let evict = (1..self.checkpoints.len())
                                .filter(unpinned)
                                .find(|&index| !self.checkpoints[index].page_boundary)
                                .or_else(|| {
                                    // Balance catalog-input and private-input
                                    // page candidates; one pass must not evict
                                    // all rollback points of the other pass.
                                    let candidates: Vec<_> =
                                        (1..self.checkpoints.len()).filter(unpinned).collect();
                                    let first = candidates
                                        .iter()
                                        .filter(|&&index| {
                                            self.checkpoints[index].pass_inputs.is_none()
                                        })
                                        .count();
                                    let later = candidates.len() - first;
                                    let mut phase = candidates.into_iter().filter(|&index| {
                                        self.checkpoints[index].pass_inputs.is_none()
                                            == (first >= later)
                                    });
                                    let earliest = phase.next();
                                    // Keep an early page as well as recent pages
                                    // so a middle edit can still skip a prefix.
                                    phase.next().or(earliest)
                                })
                                .unwrap_or(1);
                            self.checkpoints.remove(evict);
                        }
                        self.observations.checkpoint_captures += 1;
                        if body_boundary {
                            self.observations.body_checkpoint_captures += 1;
                        }
                        if page_boundary {
                            self.observations.page_checkpoint_captures += 1;
                            self.observations.page_boundaries.push(
                                self.engine
                                    .as_ref()
                                    .unwrap()
                                    .page_boundary_observation(offset),
                            );
                            if self.observations.page_boundaries.len() > 8 {
                                self.observations.page_boundaries.remove(0);
                            }
                        }
                        self.last_checkpoint_pages = completed_pages;
                        self.returned_input_pending = false;
                    }
                    if body_boundary
                        && self.engine.as_ref().unwrap().core.input.in_open > 1
                        && self.observations.checkpoint_captures == capture_count
                    {
                        let reason = self.engine.as_ref().unwrap().source_checkpoint_blocker();
                        *self
                            .observations
                            .checkpoint_capture_rejections
                            .entry(reason.into())
                            .or_default() += 1;
                    }
                    self.checkpoint_capture_ms +=
                        self.host.measurement_nanos().saturating_sub(capture_start) as f64
                            / 1_000_000.0;
                }
                self.last_offset = offset;
                self.observations.checkpoint_count = self.checkpoints.len();
            }
            if self.config.progressive_pages
                && self.engine.as_ref().unwrap().completed_page_count() > previous_pages
            {
                #[cfg(feature = "latency-diagnostics")]
                crate::local_update_audit::publication();
                #[cfg(feature = "latency-diagnostics")]
                {
                    let engine = self.engine.as_ref().unwrap();
                    let blocker = if self.last_checkpoint_pages == engine.layout.pages.len() {
                        "captured"
                    } else {
                        engine.publication_checkpoint_blocker()
                    };
                    let _publication =
                        crate::latency_diagnostics::phase("publication_checkpoint", blocker, None);
                    crate::latency_diagnostics::count("pages", engine.completed_page_count());
                    crate::latency_diagnostics::count(
                        "main_offset",
                        engine
                            .core
                            .input
                            .input_file
                            .first()
                            .and_then(Option::as_ref)
                            .and_then(|s| s.byte_position())
                            .unwrap_or(0),
                    );
                }
                return Ok(RunProgress::Yielded);
            }
        }
        Ok(RunProgress::Yielded)
    }
    fn finish_pass(&mut self) -> Result<bool> {
        if self.engine.as_ref().is_some_and(|e| e.recovered()) {
            if let Some(settling) = &mut self.settling {
                settling.observation.status = "recovered".into();
            }
            return Ok(false);
        }
        use crate::resources::host::Host;
        let Some(settling) = &mut self.settling else {
            return Ok(false);
        };
        let mut changed_paths = Vec::new();
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        use sha2::{Digest, Sha256};
        for (path, previous) in &mut settling.inputs {
            let identity = |bytes: &Option<Arc<[u8]>>| AuxiliaryObservation {
                path: resource_path(path).0,
                sha256: bytes
                    .as_ref()
                    .map(|bytes| format!("{:x}", Sha256::digest(bytes))),
            };
            inputs.push(identity(previous));
            let current = match self.host.read(path) {
                Ok(bytes) => Some(Arc::<[u8]>::from(bytes)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            outputs.push(identity(&current));
            if *previous != current {
                changed_paths.push(resource_path(path).0);
                *previous = current;
            }
        }
        let stable = changed_paths.is_empty();
        settling.observation.generations.push(PassObservation {
            pass: settling.observation.pass,
            inputs,
            outputs,
            changed_paths,
        });
        // Only successful complete passes reach here. Their declared outputs
        // are useful inputs before convergence; in-flight writes never replace
        // this immutable snapshot, and it does not make the revision settled.
        if self.config.persistent_auxiliary_inputs {
            let document = self.document.as_ref().unwrap();
            let generation = if let Some(old) = self.working_auxiliary.as_ref().filter(|w| {
                w.document == document.tag.document
                    && w.path == document.path
                    && w.inputs == settling.inputs
            }) {
                old.generation
            } else {
                let generation = self.next_auxiliary_generation;
                self.next_auxiliary_generation = generation
                    .checked_add(1)
                    .ok_or_else(|| Error::value("Auxiliary generation overflow"))?;
                generation
            };
            let seed = Arc::new(
                self.host
                    .auxiliary_input_seed(document.tag.clone(), &settling.inputs)?,
            );
            self.working_auxiliary = Some(WorkingAuxiliary {
                document: document.tag.document.clone(),
                path: document.path.clone(),
                revision: document.tag.revision,
                pass: settling.observation.pass,
                stable,
                generation,
                inputs: settling.inputs.clone(),
                seed,
                resources: self.host.checkpoint_auxiliary_resources(&settling.inputs),
            });
            settling.observation.output_generation = Some(generation);
        }
        if stable || settling.observation.pass == settling.observation.max_passes {
            settling.observation.status = if stable { "stable" } else { "limit" }.into();
            return Ok(false);
        }
        self.host
            .next_effect_pass(&self.document.as_ref().unwrap().tag)?;
        let seed = self.host.checkpoint_effects()?;
        // The opt-in input generation contains files, not a prior compilation's
        // log. Restarting a pass installs its own validated prefix console.
        settling.pass_inputs = Some(Arc::new(if self.config.persistent_auxiliary_inputs {
            seed.input_files_only()
        } else {
            seed
        }));
        settling.observation.pass += 1;
        self.restart_settling_pass()?;
        self.active = true;
        Ok(true)
    }
    fn restart_settling_pass(&mut self) -> Result<()> {
        use crate::resources::host::Host;
        let document = self.document.as_ref().unwrap();
        let inputs = self
            .settling
            .as_ref()
            .and_then(|settling| settling.pass_inputs.as_ref());
        let selected = self
            .checkpoints
            .iter()
            .enumerate()
            .rev()
            .find(|(_, checkpoint)| {
                inputs
                    .is_none_or(|inputs| inputs.main_input_matches(&document.path, &document.bytes))
                    && ((checkpoint.pass_inputs.is_none() && checkpoint.effects.read_only())
                        || checkpoint
                            .pass_inputs
                            .as_ref()
                            .zip(inputs)
                            .is_some_and(|(previous, current)| previous.same_pass_inputs(current)))
                    && checkpoint.offset <= document.bytes.len()
                    && checkpoint.source[..checkpoint.offset] == document.bytes[..checkpoint.offset]
                    && self.checkpoint_sources_valid(checkpoint)
                    && checkpoint.clock_bucket == self.host.clock_bucket()
                    && checkpoint
                        .wall_identity
                        .is_none_or(|time| time == self.host.wall_identity())
                    && if checkpoint.pass_inputs.is_some() {
                        self.host
                            .resources_valid(&checkpoint.resources, Some(&document.path))
                    } else {
                        self.host
                            .resources_match_effect_inputs(&checkpoint.resources, &document.path)
                    }
            })
            .map(|(index, _)| index);
        if let Some(index) = selected {
            let checkpoint = &self.checkpoints[index];
            let restored = if checkpoint.pass_inputs.is_some() {
                self.host
                    .restore_effects(&checkpoint.effects, document.tag.clone())?;
                true
            } else {
                self.host
                    .restore_read_only_for_pass(&checkpoint.effects, &document.tag)?
            };
            if restored {
                self.host.restore_source_resources(
                    &checkpoint.resources,
                    &checkpoint
                        .readers
                        .iter()
                        .map(|r| PathBuf::from(&r.path))
                        .collect::<Vec<_>>(),
                );
                let mut engine = checkpoint.engine.checkpoint_clone()?.ok_or_else(|| {
                    Error::value("Stored auxiliary-pass checkpoint cannot resume")
                })?;
                engine.replace_source_bytes(&self.rebound_sources(checkpoint))?;
                engine.core.pdf_utilities.creation_time = self.host.now();
                self.observations.restored_offset = checkpoint.offset;
                self.observations.restored_pages = engine.layout.pages.len();
                if checkpoint.body_boundary {
                    self.observations.body_checkpoint_reuses += 1;
                }
                if checkpoint.page_boundary {
                    self.observations.page_checkpoint_reuses += 1;
                    self.observations
                        .page_restores
                        .push(PageRollbackObservation {
                            pass: self.settling.as_ref().unwrap().observation.pass,
                            input_offset: checkpoint.offset,
                            completed_pages: engine.layout.pages.len(),
                        });
                    if self.observations.page_restores.len() > 8 {
                        self.observations.page_restores.remove(0);
                    }
                }
                self.observations.pass_checkpoint_reuses += 1;
                self.inherited_wall = checkpoint.wall_identity.is_some();
                self.paragraph_seen = checkpoint.paragraph_seen;
                self.last_offset = checkpoint.offset;
                self.last_checkpoint_pages = engine.layout.pages.len();
                self.returned_input_pending = false;
                // Keep the first-pass preamble and the latest later-pass
                // preamble as bounded candidates. Their different input
                // contexts are validated before each future restoration.
                let mut position = 0;
                self.checkpoints.retain(|checkpoint| {
                    let retain = position <= index
                        || checkpoint.first_pass_pinned
                        || checkpoint.later_pass_pinned
                        || checkpoint.page_boundary
                        || checkpoint.body_boundary;
                    position += 1;
                    retain
                });
                self.engine = Some(engine);
                self.host.observe_main(&document.path);
                self.clock_baseline = self.host.clock_observations();
                self.wall_baseline = self.host.wall_observations();
                self.observations.checkpoint_count = self.checkpoints.len();
                return Ok(());
            }
        }
        self.restart_fresh()
    }
    fn restart_fresh(&mut self) -> Result<()> {
        use crate::resources::host::Host;
        let document = self.document.as_ref().unwrap();
        self.host.discard_effects();
        self.checkpoints.retain(|checkpoint| {
            checkpoint.first_pass_pinned
                || checkpoint.later_pass_pinned
                || checkpoint.page_boundary
                || checkpoint.body_boundary
        });
        self.inherited_wall = false;
        self.paragraph_seen = false;
        self.preamble_resources = None;
        if let Some(inputs) = self.settling.as_ref().and_then(|s| s.pass_inputs.as_ref()) {
            self.host.restore_effects(inputs, document.tag.clone())?;
        } else if let Some(seed) = &self.entry_inputs {
            self.host.restore_effects(seed, document.tag.clone())?;
        } else {
            self.host
                .begin_effects(document.tag.clone(), 64 * 1024 * 1024)?;
        }
        self.host
            .restore_resources(self.baseline_resources.as_ref().unwrap());
        let mut engine = self
            .baseline
            .as_ref()
            .unwrap()
            .checkpoint_clone()?
            .ok_or_else(|| Error::value("Initialized state cannot restart"))?;
        if !engine.start_input(&document.path) {
            return Err(Error::value("Cannot reopen document"));
        }
        self.host.observe_main(&document.path);
        self.host.take_console();
        engine.core.extended.timer_start = self.host.monotonic_nanos();
        engine.core.pdf_utilities.creation_time = self.host.now();
        self.clock_baseline = self.host.clock_observations();
        self.wall_baseline = self.host.wall_observations();
        engine.begin_run()?;
        self.engine = Some(engine);
        self.last_offset = 0;
        self.last_physical = None;
        self.last_checkpoint_pages = 0;
        self.returned_input_pending = false;
        self.observations.restored_offset = 0;
        self.observations.restored_pages = 0;
        self.observations.restored_auxiliary_checkpoint = false;
        self.observations.initialized_restarts += 1;
        Ok(())
    }
    fn cancel_current(&mut self) {
        self.preview_pending = None;
        self.preview_published = false;
        self.preview_reason = None;
        self.preview_execution_ms = 0.0;
        self.preview_mapping = serde_json::Value::Null;
        self.early = None;
        self.local_hits = 0;
        self.local_cost_ms = 0.0;
        self.local_recipe_ms.set(0.0);
        self.local_paragraph_started = None;
        self.local_paragraph_ms = 0.0;
        self.local_verified = false;
        self.local_verify_ms = 0.0;
        #[cfg(feature = "latency-diagnostics")]
        let _cancel =
            crate::latency_diagnostics::phase("revision_cancel", "rollback and retirement", None);
        // Later passes can contain an earlier pass's unaccepted auxiliary bytes.
        // Retain catalog-read prefixes, plus preamble candidates only after a
        // completed stable job. First-pass and later-pass input contexts remain
        // separate. The opt-in policy additionally admits private prefixes only
        // against the last successfully completed pass's working input generation.
        let completed_stable = !self.active
            && self.current_valid
            && self
                .engine
                .as_ref()
                .is_some_and(|engine| engine.status() == "complete")
            && self
                .settling
                .as_ref()
                .is_some_and(|settling| settling.observation.status == "stable");
        let superseded_first_pass = self.config.progressive_pages
            && self.active
            && self.current_valid
            && self.resource_request.is_none()
            && self
                .engine
                .as_ref()
                .is_some_and(|engine| engine.status() == "running");
        #[cfg(feature = "latency-diagnostics")]
        let retain_diag = crate::latency_diagnostics::phase(
            "cancel_checkpoint_retention",
            "filter and bounded retirement",
            None,
        );
        if self.settling.take().is_some() {
            let working_seed = self
                .config
                .persistent_auxiliary_inputs
                .then(|| self.working_auxiliary.as_ref().map(|w| w.seed.clone()))
                .flatten();
            self.retain_checkpoints(|checkpoint| {
                (checkpoint.effects.read_only() && checkpoint.pass_inputs.is_none())
                    // These states depend only on this run's catalog inputs
                    // and its exact executed prefix. Restoration still checks
                    // source, resources, time and effects. Working inputs below
                    // require their own successful completed-pass generation.
                    || (superseded_first_pass && checkpoint.body_boundary && checkpoint.pass_inputs.is_none())
                    || (working_seed.is_some() && superseded_first_pass
                        && (checkpoint.body_boundary || checkpoint.page_boundary || checkpoint.first_pass_pinned || checkpoint.later_pass_pinned)
                        && checkpoint.pass_inputs.as_ref().zip(working_seed.as_ref())
                            .is_some_and(|(old, seed)| checkpoint.effects.can_rebase_inputs(old, seed)))
                    || (completed_stable
                        && (checkpoint.first_pass_pinned
                            || checkpoint.later_pass_pinned
                            || checkpoint.page_boundary
                            || checkpoint.body_boundary))
            });
            self.preamble_resources = None;
            self.observations.checkpoint_count = self.checkpoints.len();
        }
        #[cfg(feature = "latency-diagnostics")]
        drop(retain_diag);
        self.active = false;
        self.pass_finished_pending = false;
        self.current_valid = false;
        self.resource_request = None;
        {
            #[cfg(feature = "latency-diagnostics")]
            let _effects = crate::latency_diagnostics::phase(
                "cancel_effect_rollback",
                "discard private transaction",
                None,
            );
            self.host.discard_effects();
        }
        {
            #[cfg(feature = "latency-diagnostics")]
            let _recorder = crate::latency_diagnostics::phase(
                "cancel_reuse_recording",
                "discard partial segment",
                None,
            );
            if let Some(engine) = self.engine.as_mut() {
                // Retired engines can retain published material until acknowledgement,
                // but must not keep a host-wide dependency recorder alive.
                engine.core.reuse.cancel(&engine.env);
            }
        }
        #[cfg(feature = "latency-diagnostics")]
        let _retire = crate::latency_diagnostics::phase(
            "cancel_engine_retirement",
            "replace bounded retired engine",
            None,
        );
        // Drop unfinished command/evaluation frames and their temporary roots.
        // Independently owned eligible checkpoints remain available for replay.
        if self.config.progressive_pages {
            // Replacing this slot bounds retirement even during rapid edits.
            // A prior retired engine is reclaimed here if the host never
            // acknowledged a page. No obsolete continuation is advanced.
            self.retired_engine = self.engine.take();
        } else {
            self.engine = None;
        }
    }
    pub fn cancel(&mut self, document: &str, revision: u64) -> Result<()> {
        if self.tag()
            != Some(&DocumentRevision {
                document: document.into(),
                revision,
            })
        {
            return Err(Error::value("Cancellation revision unavailable"));
        }
        // Explicit cancellation discards private prefixes as well as current
        // effects, including a previously completed result the host rejects.
        for checkpoint in &mut self.checkpoints {
            checkpoint.first_pass_pinned = false;
            checkpoint.later_pass_pinned = false;
            checkpoint.page_boundary = false;
            checkpoint.body_boundary = false;
        }
        self.cancel_current();
        Ok(())
    }
    pub fn pdf_output_available(&self) -> Result<bool> {
        Ok(self.settled_engine()?.pdf_output_available())
    }
    pub fn export_pdf(&mut self) -> Result<Option<Vec<u8>>> {
        if !self.current_valid
            || self.active
            || self.resource_request.is_some()
            || self.catalog_epoch != self.host.epoch()
        {
            return Err(Error::value("PDF export revision is unavailable"));
        }
        // Preserve the CLI's publication policy for interactive downloads.
        // Failure finalization still retains Python's partial pages internally.
        if self.engine()?.status() != "complete" {
            return Ok(None);
        }
        self.engine_mut()?.export_pdf_bytes()
    }
    pub fn commit_outputs(&mut self, document: &str, revision: u64) -> Result<()> {
        if self.engine.as_ref().is_some_and(|e| e.recovered()) {
            return Err(Error::value(
                "Recovered previews cannot publish auxiliary outputs",
            ));
        }
        let tag = DocumentRevision {
            document: document.into(),
            revision,
        };
        if self.tag() != Some(&tag)
            || !self.current_valid
            || self.active
            || self.resource_request.is_some()
            || self.engine()?.status() != "complete"
        {
            return Err(Error::value(
                "Only a completed accepted revision can publish outputs",
            ));
        }
        self.host.commit_effects(&tag)?;
        // Accepted auxiliary inputs are a new resource generation.
        self.checkpoints.clear();
        self.baseline = None;
        self.preamble_resources = None;
        self.catalog_epoch = self.host.epoch();
        Ok(())
    }
}
impl Drop for EditingSession {
    fn drop(&mut self) {
        self.host.discard_effects();
    }
}

#[cfg(all(
    test,
    any(feature = "latency-diagnostics", feature = "output-replay-control")
))]
mod investigation_tests {
    use super::*;
    #[test]
    fn diagnostic_pin_restores_one_object_and_rejects_changed_prefix() {
        use crate::resources::{catalog::ResourceCatalog, host::FixedClock};
        let source = b"\\count0=7\n\\count1=8\n\\shipout\\hbox{}\\end\n";
        let mut catalog = ResourceCatalog::new(Default::default());
        catalog
            .apply(CatalogDelta {
                base_epoch: 0,
                new_epoch: 1,
                policy_version: 0,
                changes: vec![ResourceChange::Put {
                    id: ResourceId("source".into()),
                    path: resource_path(Path::new("/project/document.tex")),
                    version: 1,
                    bytes: source.as_slice().into(),
                }],
            })
            .unwrap();
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
        let mut fonts = Vec::new();
        for dir in [assets.join("assets"), assets.join("metrics")] {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_file() {
                    fonts.push(ResourceChange::Put {
                        id: ResourceId(path.display().to_string()),
                        path: resource_path(&path),
                        version: 1,
                        bytes: std::fs::read(&path).unwrap().into(),
                    });
                }
            }
        }
        catalog
            .apply(CatalogDelta {
                base_epoch: 1,
                new_epoch: 2,
                policy_version: 0,
                changes: fonts,
            })
            .unwrap();
        let host = Rc::new(
            VirtualHost::new(
                catalog,
                PathBuf::from("/project"),
                BTreeMap::new(),
                FixedClock::new(1600000000, 0, 0).unwrap(),
                1_000_000,
            )
            .unwrap(),
        );
        let mut session = EditingSession::new(
            EngineConfig {
                progressive_pages: true,
                exact_repro: true,
                interaction_mode: Some(0),
                configuration_paths: Some(vec![]),
                tex_input_roots: Some(vec!["/project".into()]),
                output_directory: Some("/project".into()),
                ..Default::default()
            },
            host,
        )
        .unwrap();
        session
            .open_document(
                "probe",
                1,
                Path::new("/project/document.tex"),
                source.as_slice().into(),
            )
            .unwrap();
        while session.advance(128).unwrap() == RunProgress::Yielded {}
        let pin = session.pin_investigation_checkpoint(10, 10).unwrap();
        let id = pin["id"].as_u64().unwrap();
        crate::investigation_checkpoint::set(10);
        for revision in 2..5 {
            session
                .apply_edit(
                    "probe",
                    revision - 1,
                    source.len(),
                    source.len(),
                    b"",
                    revision,
                )
                .unwrap();
            assert_eq!(session.observations.restored_checkpoint, Some(id));
            while session.advance(128).unwrap() == RunProgress::Yielded {}
            assert!(
                session
                    .checkpoints
                    .iter()
                    .any(|checkpoint| checkpoint.investigation_id == id)
            );
        }
        assert!(session.apply_edit("probe", 4, 0, 1, b"X", 5).is_err());
        crate::investigation_checkpoint::set(usize::MAX);
    }
}
