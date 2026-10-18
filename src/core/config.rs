//! Per-job immutable configuration (`core/config.py`).
use super::characters::LanguageProfile;
use super::errors::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    /// Explicit bundle anchor for hosts without repository filesystem paths.
    #[serde(default)]
    pub resource_root: Option<String>,
    pub recovery_policy: super::recovery::RecoveryPolicy,
    pub recovery_limit: usize,
    pub font_dir: Option<String>,
    pub encoding_file: Option<String>,
    pub trace_enabled: bool,
    pub event_limit: usize,
    pub expansion_limit: Option<usize>,
    pub ini: bool,
    pub greedy_paragraphs: bool,
    pub eight_bit_output: bool,
    pub interaction_mode: Option<u8>,
    pub program_name: String,
    pub capacities: Vec<(String, i64)>,
    pub output_directory: Option<String>,
    pub exact_repro: bool,
    pub language_profile: LanguageProfile,
    pub tex_input_roots: Option<Vec<String>>,
    pub configuration_paths: Option<Vec<String>>,
    /// Experimental bounded page rollback. Off until capture/retention costs
    /// beat ordinary execution on representative edit workloads.
    pub page_checkpoints: bool,
    /// Resident page publication and complete page-checkpoint retention.
    pub progressive_pages: bool,
    /// Checked preview substitution; ordinary execution always settles independently.
    pub exact_local_updates: bool,
    /// Development differential after ordinary shipment; never needed for publication.
    pub exact_local_verify: bool,
    /// Optional bounded decision/provenance records for offline investigation.
    pub exact_local_trace: bool,
    /// Experimental mixed-revision paragraph presentation. Never engine state.
    pub paragraph_preview: bool,
    /// Optional stage clocks, independent of the provisional display switch.
    pub paragraph_preview_timings: bool,
    /// Keep declared auxiliary inputs from successful complete passes between revisions.
    pub persistent_auxiliary_inputs: bool,
    /// Conservative cross-edit paragraph execution reuse; false is a differential control.
    pub execution_reuse: bool,
    /// Conservative balanced construction reuse; false is a differential control.
    pub grouped_execution_reuse: bool,
    /// Differential control for classic font-construction receipts.
    pub font_construction_reuse: bool,
    /// Ordered state and transactional-write replay for supported segments.
    pub stateful_execution_reuse: bool,
    /// Scoped paragraph preparation with supported escaping assignments.
    pub stateful_paragraph_reuse: bool,
    /// Balanced construction inside a guarded alignment cell body.
    pub alignment_material_reuse: bool,
    /// Compact immutable dependency guards and favor larger material segments.
    pub material_region_admission: bool,
    /// Reuse across fully consumed immutable physical and owned pseudo-input.
    pub input_execution_reuse: bool,
}
impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            resource_root: None,
            recovery_policy: super::recovery::RecoveryPolicy::Strict,
            recovery_limit: 100,
            font_dir: None,
            encoding_file: None,
            trace_enabled: false,
            event_limit: 10_000,
            expansion_limit: None,
            ini: false,
            greedy_paragraphs: false,
            eight_bit_output: false,
            interaction_mode: None,
            program_name: "pdftex".into(),
            capacities: Vec::new(),
            output_directory: None,
            exact_repro: false,
            language_profile: LanguageProfile::Pdftex,
            tex_input_roots: None,
            configuration_paths: None,
            page_checkpoints: false,
            progressive_pages: false,
            exact_local_updates: false,
            exact_local_verify: false,
            exact_local_trace: false,
            paragraph_preview: false,
            paragraph_preview_timings: false,
            persistent_auxiliary_inputs: false,
            execution_reuse: true,
            grouped_execution_reuse: true,
            font_construction_reuse: true,
            stateful_execution_reuse: true,
            stateful_paragraph_reuse: true,
            alignment_material_reuse: true,
            material_region_admission: true,
            input_execution_reuse: true,
        }
    }
}
impl EngineConfig {
    pub fn validate(&self) -> Result<()> {
        if !(1..=4096).contains(&self.recovery_limit) {
            return Err(Error::value("Recovery limit must be between 1 and 4096"));
        }
        if self.event_limit == 0 {
            return Err(Error::value("--event-limit must be positive"));
        }
        if self.expansion_limit == Some(0) {
            return Err(Error::value("--expansion-limit must be positive"));
        }
        if self.interaction_mode.is_some_and(|mode| mode > 3) {
            return Err(Error::value("Interaction mode must be 0–3"));
        }
        Ok(())
    }
}
