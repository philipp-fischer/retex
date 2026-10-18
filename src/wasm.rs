//! Local browser job ABI over the shared typed Engine and explicit catalog host.
//! The shared resident editing session performs guarded rollback/replay.
use crate::{
    core::config::EngineConfig,
    engine::RunProgress,
    resources::{
        catalog::{CatalogDelta, ResourceCatalog, ResourceChange, ResourceId, ResourcePath},
        effects::DocumentRevision,
        host::FixedClock,
        virtual_host::VirtualHost,
    },
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    rc::Rc,
};
use wasm_bindgen::prelude::*;
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = performance, js_name = now)]
    fn performance_now() -> f64;
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn panic_message(message: &str);
}

#[derive(serde::Deserialize)]
struct BundleEntry {
    path: Vec<u8>,
    id: String,
    version: u32,
    offset: usize,
    length: usize,
    #[serde(default)]
    modified_unix_ms: Option<i64>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceUpdate {
    base_epoch: u64,
    new_epoch: u64,
    policy_version: u64,
    changes: Vec<ResourceUpdateEntry>,
}
#[derive(serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum ResourceUpdateEntry {
    Put {
        path: Vec<u8>,
        id: String,
        version: u64,
        bytes: Vec<u8>,
        #[serde(default)]
        modified_unix_ms: Option<i64>,
    },
    Remove {
        path: Vec<u8>,
    },
}

fn decode_update(update_json: &str) -> std::result::Result<CatalogDelta, JsValue> {
    if update_json.len() > 64 * 1024 * 1024 {
        return Err(failure("Resource update transport budget exceeded"));
    }
    let update: ResourceUpdate = serde_json::from_str(update_json).map_err(failure)?;
    let changes = update
        .changes
        .into_iter()
        .map(|entry| match entry {
            ResourceUpdateEntry::Put {
                path,
                id,
                version,
                bytes,
                modified_unix_ms,
            } => ResourceChange::put(
                ResourceId(id),
                ResourcePath(path),
                version,
                bytes.into(),
                modified_unix_ms,
            )
            .map_err(failure),
            ResourceUpdateEntry::Remove { path } => Ok(ResourceChange::Remove(ResourcePath(path))),
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(CatalogDelta {
        base_epoch: update.base_epoch,
        new_epoch: update.new_epoch,
        policy_version: update.policy_version,
        changes,
    })
}
fn failure(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}
#[wasm_bindgen]
pub struct BrowserJob {
    config: EngineConfig,
    initialized_pdf_output: Option<i64>,
    host: Rc<VirtualHost>,
    session: Option<crate::session::EditingSession>,
    tag: Option<DocumentRevision>,
    outlines: crate::fonts::outlines::OutlineCache,
    images: crate::pdf::preview_images::ImageCache,
    preview_pixels: Vec<u8>,
}
#[wasm_bindgen]
impl BrowserJob {
    #[cfg(feature = "latency-diagnostics")]
    pub fn diagnostic_config(&self, mode: &str) -> std::result::Result<(), JsValue> {
        if !matches!(
            mode,
            "off" | "local" | "coarse" | "detail" | "provenance-bypass" | "stages" | "stage-detail"
        ) {
            return Err(failure("Invalid diagnostic mode"));
        }
        crate::latency_diagnostics::configure(mode);
        Ok(())
    }
    #[cfg(any(feature = "latency-diagnostics", feature = "output-replay-control"))]
    pub fn diagnostic_checkpoint_limit(
        &mut self,
        offset: u32,
        anchor: u32,
    ) -> std::result::Result<String, JsValue> {
        let info = self
            .session
            .as_mut()
            .ok_or_else(|| failure("No investigation session"))?
            .pin_investigation_checkpoint(offset as usize, anchor as usize)
            .map_err(failure)?;
        crate::investigation_checkpoint::set(offset as usize);
        Ok(info.to_string())
    }
    #[cfg(feature = "latency-diagnostics")]
    pub fn diagnostic_clear_material(&mut self) -> std::result::Result<(), JsValue> {
        self.session
            .as_mut()
            .ok_or_else(|| failure("No investigation session"))?
            .clear_investigation_material()
            .map_err(failure)
    }
    #[cfg(feature = "latency-diagnostics")]
    pub fn output_diagnostic_complete(&self) -> bool {
        crate::output_diagnostics::complete()
    }
    #[cfg(feature = "latency-diagnostics")]
    pub fn diagnostic_snapshot(&self) -> String {
        crate::output_diagnostics::publication();
        let mut record = crate::latency_diagnostics::snapshot();
        record["meaning_cache"] = crate::language::meaning_memo::diagnostics();
        record["font_program_cache"] = crate::fonts::resources::diagnostics();
        record["font_map_cache"] = crate::fonts::map_preparation::diagnostics();
        record["preview_image_bytes"] = self.images.bytes().into();
        record["outline_bytes"] = self.outlines.bytes().into();
        record.to_string()
    }
    #[wasm_bindgen(constructor)]
    pub fn new(
        config_json: &str,
        clock_seconds: u32,
        utc_offset_seconds: i32,
    ) -> std::result::Result<BrowserJob, JsValue> {
        std::panic::set_hook(Box::new(|info| panic_message(&info.to_string())));
        let settings: serde_json::Value = serde_json::from_str(config_json).map_err(failure)?;
        crate::source_map::configure(
            settings
                .get("source_map_reuse")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            settings
                .get("source_map_profile")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        );
        let config: EngineConfig = serde_json::from_value(settings.clone()).map_err(failure)?;
        config.validate().map_err(failure)?;
        let initialized_pdf_output = settings
            .get("pdf_output")
            .map(|value| {
                value
                    .as_i64()
                    .filter(|value| matches!(value, 0 | 1))
                    .ok_or_else(|| failure("Browser PDF output default must be 0 or 1"))
            })
            .transpose()?;
        let mut variables = BTreeMap::from([(
            "TEXMF_OUTPUT_DIRECTORY".into(),
            config
                .output_directory
                .as_deref()
                .unwrap_or("/project")
                .into(),
        )]);
        if let Some(values) = settings.get("host_variables") {
            let values: BTreeMap<String, String> =
                serde_json::from_value(values.clone()).map_err(failure)?;
            for (name, value) in values {
                if name.len() > 256
                    || value.len() > 4096
                    || name.contains(['\0', '='])
                    || value.contains('\0')
                {
                    return Err(failure("Invalid browser host variable"));
                }
                variables.insert(name, value.into());
            }
            if variables.len() > 256 {
                return Err(failure("Browser host variable count exceeded"));
            }
        }
        let host = VirtualHost::new(
            ResourceCatalog::new(Default::default()),
            PathBuf::from("/project"),
            variables,
            FixedClock::new(clock_seconds as u64, 0, utc_offset_seconds).map_err(failure)?,
            1024 * 1024,
        )
        .map_err(failure)?;
        host.set_monotonic_clock(Rc::new(|| (performance_now() * 1_000_000.0) as u128));
        host.set_measurement_clock(Rc::new(|| (performance_now() * 1_000_000.0) as u128));
        Ok(Self {
            config,
            initialized_pdf_output,
            host: Rc::new(host),
            session: None,
            tag: None,
            outlines: Default::default(),
            images: Default::default(),
            preview_pixels: Vec::new(),
        })
    }
    pub fn set_clock(&self, seconds: u32, offset_seconds: i32) -> std::result::Result<(), JsValue> {
        self.host
            .set_clock(FixedClock::new(seconds as u64, 0, offset_seconds).map_err(failure)?);
        Ok(())
    }
    pub fn load_initialized(&mut self, path: &str) -> std::result::Result<(), JsValue> {
        use crate::resources::host::Host;
        if self.tag.is_some() {
            return Err(failure("Load initialized state before opening a document"));
        }
        let bytes = self.host.read(Path::new(path)).map_err(failure)?;
        let mut state =
            crate::serialization::formats::decode_snapshot(&bytes, Some(256 * 1024 * 1024))
                .map_err(failure)?;
        if let Some(value) = self.initialized_pdf_output {
            state.environment.values.insert(
                crate::language::records::ValueKey::parameter("pdfoutput"),
                crate::language::records::Quantity::Integer(value),
            );
        }
        let mut session =
            crate::session::EditingSession::new(self.config.clone(), self.host.clone())
                .map_err(failure)?;
        session
            .load_initialized(state, Path::new("/bundle/formats"))
            .map_err(failure)?;
        self.session = Some(session);
        Ok(())
    }
    /// Path/source are byte buffers. UTF-8 path transport is currently required
    /// by wasm32's OsString implementation; invalid paths fail explicitly.
    pub fn put_resource(
        &mut self,
        path: &[u8],
        id: &str,
        version: u32,
        bytes: &[u8],
    ) -> std::result::Result<(), JsValue> {
        let epoch = self.host.epoch();
        self.apply_delta(CatalogDelta {
            base_epoch: epoch,
            new_epoch: epoch
                .checked_add(1)
                .ok_or_else(|| failure("Catalog epoch overflow"))?,
            policy_version: self.host.policy_version(),
            changes: vec![ResourceChange::Put {
                id: ResourceId(id.into()),
                path: ResourcePath(path.into()),
                version: version as u64,
                bytes: bytes.into(),
            }],
        })
        .map_err(failure)
    }
    pub fn put_resources(
        &mut self,
        manifest_json: &str,
        bytes: &[u8],
    ) -> std::result::Result<(), JsValue> {
        if bytes.len() > 512 * 1024 * 1024 || manifest_json.len() > 8 * 1024 * 1024 {
            return Err(failure("Bundle byte/manifest budget exceeded"));
        }
        let entries: Vec<BundleEntry> = serde_json::from_str(manifest_json).map_err(failure)?;
        if entries.len() > 20_000 {
            return Err(failure("Bundle resource count budget exceeded"));
        }
        let mut changes = Vec::with_capacity(entries.len());
        for entry in entries {
            let end = entry
                .offset
                .checked_add(entry.length)
                .ok_or_else(|| failure("Bundle offset overflow"))?;
            let data = bytes
                .get(entry.offset..end)
                .ok_or_else(|| failure("Bundle resource outside supplied bytes"))?;
            changes.push(
                ResourceChange::put(
                    ResourceId(entry.id),
                    ResourcePath(entry.path),
                    entry.version as u64,
                    data.into(),
                    entry.modified_unix_ms,
                )
                .map_err(failure)?,
            );
        }
        let epoch = self.host.epoch();
        self.apply_delta(CatalogDelta {
            base_epoch: epoch,
            new_epoch: epoch
                .checked_add(1)
                .ok_or_else(|| failure("Catalog epoch overflow"))?,
            policy_version: self.host.policy_version(),
            changes,
        })
        .map_err(failure)
    }
    /// Explicit epochs reject late catalog replies. Validation is atomic and
    /// happens before cancelling the current document or discarding its effects.
    pub fn update_resources(&mut self, update_json: &str) -> std::result::Result<(), JsValue> {
        self.apply_delta(decode_update(update_json)?)
            .map_err(failure)
    }
    pub fn resolve_resource(
        &mut self,
        document: &str,
        revision: u32,
        request_id: f64,
        update_json: Option<String>,
    ) -> std::result::Result<String, JsValue> {
        if !request_id.is_finite()
            || request_id < 1.0
            || request_id.fract() != 0.0
            || request_id > 9007199254740991.0
        {
            return Err(failure("Invalid resource request identity"));
        }
        let delta = update_json.as_deref().map(decode_update).transpose()?;
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| failure("No browser session"))?;
        session
            .resolve_resource(document, revision as u64, request_id as u64, delta)
            .map_err(failure)?;
        let engine = session.engine().map_err(failure)?;
        Ok(serde_json::json!({"document":document,"revision":revision,"status":if update_json.is_some()||engine.status()=="running" { "yielded" } else { "failed" }, "errors":engine.core.diagnostics.errors,"warnings":engine.core.diagnostics.warnings,"reuse":session.reuse()}).to_string())
    }
    pub fn catalog_state(&self) -> String {
        serde_json::json!({"epoch":self.host.epoch(),"policy_version":self.host.policy_version(),"pending_bytes":self.host.pending_bytes()}).to_string()
    }
    pub fn resource_identity(&self, path: &[u8]) -> std::result::Result<String, JsValue> {
        let identity = self
            .host
            .resource_identity(&ResourcePath(path.into()))
            .map_err(failure)?;
        Ok(identity
            .map(|(id, version)| serde_json::json!({"id":id.0,"version":version}))
            .unwrap_or(serde_json::Value::Null)
            .to_string())
    }
    pub fn open_document(
        &mut self,
        document: &str,
        revision: u32,
        path: &str,
        bytes: &[u8],
    ) -> std::result::Result<(), JsValue> {
        self.open_project_document(document, document, revision, path, bytes)
    }
    pub fn open_project_document(
        &mut self,
        project: &str,
        document: &str,
        revision: u32,
        path: &str,
        bytes: &[u8],
    ) -> std::result::Result<(), JsValue> {
        if self.session.is_none() {
            self.session = Some(
                crate::session::EditingSession::new(self.config.clone(), self.host.clone())
                    .map_err(failure)?,
            );
        }
        self.session
            .as_mut()
            .unwrap()
            .open_project_document(
                project,
                document,
                revision as u64,
                Path::new(path),
                bytes.into(),
            )
            .map_err(failure)?;
        self.tag = Some(DocumentRevision {
            document: document.into(),
            revision: revision as u64,
        });
        Ok(())
    }
    pub fn set_checkpoint_focus(
        &mut self,
        document: &str,
        path: &str,
        offset: u32,
    ) -> std::result::Result<(), JsValue> {
        self.session
            .as_mut()
            .ok_or_else(|| failure("No editing session"))?
            .set_checkpoint_focus(document, Path::new(path), offset as usize)
            .map_err(failure)
    }
    pub fn source_protocol_version(&self) -> u32 {
        2
    }
    pub fn validate_source_transaction(
        &self,
        transaction: &str,
    ) -> std::result::Result<(), JsValue> {
        let tx = serde_json::from_str(transaction).map_err(failure)?;
        self.session
            .as_ref()
            .ok_or_else(|| failure("No editing session"))?
            .validate_source_transaction(tx)
            .map_err(failure)
    }
    pub fn apply_source_transaction(
        &mut self,
        transaction: &str,
    ) -> std::result::Result<(), JsValue> {
        let tx: crate::session::SourceTransaction =
            serde_json::from_str(transaction).map_err(failure)?;
        let tag = DocumentRevision {
            document: tx.document.clone(),
            revision: tx.revision,
        };
        self.session
            .as_mut()
            .ok_or_else(|| failure("No editing session"))?
            .apply_source_transaction(tx)
            .map_err(failure)?;
        self.tag = Some(tag);
        Ok(())
    }
    pub fn apply_edit(
        &mut self,
        document: &str,
        base_revision: u32,
        start: u32,
        end: u32,
        replacement: &[u8],
        revision: u32,
    ) -> std::result::Result<(), JsValue> {
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::reset();
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase("revision_open", "", None);
        self.session
            .as_mut()
            .ok_or_else(|| failure("No editing session"))?
            .apply_edit(
                document,
                base_revision as u64,
                start as usize,
                end as usize,
                replacement,
                revision as u64,
            )
            .map_err(failure)?;
        self.tag = Some(DocumentRevision {
            document: document.into(),
            revision: revision as u64,
        });
        Ok(())
    }
    pub fn start(
        &mut self,
        document: &str,
        revision: u32,
        path: &str,
    ) -> std::result::Result<(), JsValue> {
        use crate::resources::host::Host;
        let bytes = self.host.read(Path::new(path)).map_err(failure)?;
        self.open_document(document, revision, path, &bytes)
    }
    pub fn source_location_pages(&self, file: &str, line: u32, column: u32) -> String {
        serde_json::to_string(
            &self
                .session
                .as_ref()
                .map(|s| s.source_location_pages(file, line as usize, column as usize))
                .unwrap_or_default(),
        )
        .unwrap()
    }
    /// Retrieve bounded original terminal output once the attempt has completed.
    pub fn diagnostic_log(&self) -> String {
        self.session
            .as_ref()
            .and_then(|s| s.engine().ok())
            .map(|e| {
                let diagnostics = &e.core.diagnostics;
                let mut log = diagnostics.terminal_log.as_ref().clone();
                if diagnostics.terminal_log_truncated {
                    log.push_str("\n[Terminal log truncated at 128 KiB]\n");
                }
                if diagnostics.dropped > 0 {
                    log.push_str(&format!(
                        "\n[{} additional warnings omitted by the diagnostic limit]\n",
                        diagnostics.dropped
                    ));
                }
                log
            })
            .unwrap_or_default()
    }
    pub fn advance(&mut self, steps: u32) -> std::result::Result<String, JsValue> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| failure("No browser session"))?;
        let progress = match session.advance_interactive(steps as usize).map_err(failure)? {
            crate::session::SessionProgress::Running(progress) => progress,
            crate::session::SessionProgress::NeedResource(request) => return Ok(serde_json::json!({"document":request.document,"revision":request.revision,"status":"need_resource","request":request}).to_string()),
        };
        let engine = session.engine().map_err(failure)?;
        // Diagnostics include bounded expansion chains. Send the completed
        // collection once, rather than serializing it at every worker quantum.
        // Yielded packets keep their existing field shape; page provenance is
        // independently published by the retained preview protocol.
        let diagnostics = if progress == RunProgress::Yielded {
            &[][..]
        } else {
            engine.core.diagnostics.records.as_slice()
        };
        Ok(serde_json::json!({"document":self.tag.as_ref().unwrap().document,"revision":self.tag.as_ref().unwrap().revision,
            "status":match progress { RunProgress::Yielded=>"yielded",RunProgress::Complete=>"complete",RunProgress::Failed=>"failed" },
            "errors":engine.core.diagnostics.errors,"warnings":engine.core.diagnostics.warnings,"diagnostics":diagnostics,"recovery":if engine.recovered(){Some(serde_json::json!({"count":engine.core.input.recovery.count,"approximate":true,"outcome":"completed_with_errors"}))}else{None},"reuse":session.reuse(),"settling":session.settling(),"checkpoint_capture_ms":session.checkpoint_capture_ms(),"completed_pages":engine.completed_page_count(),"local_page":session.early_page(),"paragraph_preview":session.paragraph_preview_status()}).to_string())
    }
    pub fn acknowledge_paragraph_page(
        &self,
        document: &str,
        revision: u32,
        pass: u32,
        page: u32,
        hash: &str,
    ) -> bool {
        self.session.as_ref().is_some_and(|session| {
            session.acknowledge_paragraph_page(
                document,
                revision as u64,
                pass as usize,
                page as usize,
                hash,
            )
        })
    }
    /// Presentation only. Does not advance TeX or make export available.
    pub fn take_paragraph_preview(&mut self) -> std::result::Result<String, JsValue> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| failure("No browser session"))?;
        let fragment = session
            .take_paragraph_preview(&mut self.outlines, &mut self.images)
            .map_err(failure)?;
        serde_json::to_string(&fragment).map_err(failure)
    }
    /// Development oracle only: never called by production publication.
    pub fn page_observation(&self, page: u32) -> std::result::Result<String, JsValue> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| failure("No browser session"))?;
        let early = session
            .early_page_layout()
            .filter(|p| p.page == page as usize)
            .map(crate::local_page::geometry)
            .transpose()
            .map_err(failure)?;
        let engine = session.engine().map_err(failure)?;
        let ordinary = if engine.completed_page_count() >= page as usize {
            let (_, _, prepared) = engine
                .prepare_preview_page(page as usize)
                .map_err(failure)?;
            Some(crate::local_page::geometry(&prepared).map_err(failure)?)
        } else {
            None
        };
        Ok(serde_json::json!({"document":self.tag.as_ref().unwrap().document,"revision":self.tag.as_ref().unwrap().revision,"early":early,"ordinary":ordinary,"reuse":session.reuse(),
            "investigation":if engine.core.config.exact_local_trace { Some(serde_json::json!({"events":engine.core.local.trace,"dropped":engine.core.local.trace_dropped})) } else {None}}).to_string())
    }
    pub fn set_preview_target(&mut self, page: u32) -> std::result::Result<(), JsValue> {
        self.session
            .as_mut()
            .ok_or_else(|| failure("No browser session"))?
            .set_preview_target(page as usize)
            .map_err(failure)
    }
    pub fn preview_page(&mut self, page: u32) -> std::result::Result<String, JsValue> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| failure("No browser session"))?;
        let scene = session
            .capture_page_with_images(page as usize, &mut self.outlines, &mut self.images)
            .map_err(failure)?;
        Ok(serde_json::json!({"document":self.tag.as_ref().unwrap().document,"revision":self.tag.as_ref().unwrap().revision,
            "pass":session.settling().map_or(1, |s| s.pass),"page":page,"preview":scene}).to_string())
    }
    /// Binary pixels avoid constructing millions of serde JSON values per page.
    /// Read `take_preview_pixels` immediately after this metadata call.
    pub fn preview_page_binary(&mut self, page: u32) -> std::result::Result<String, JsValue> {
        self.preview_page_binary_known(page, "[]")
    }
    /// Omit only samples acknowledged as resident by the browser. Each page
    /// still carries its complete resource manifest and drawing placements.
    pub fn preview_page_binary_known(
        &mut self,
        page: u32,
        known: &str,
    ) -> std::result::Result<String, JsValue> {
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase(
            "preview_preparation",
            "",
            Some(crate::latency_diagnostics::Region {
                file: "@preview".into(),
                ..Default::default()
            }),
        );
        let known: Vec<String> = serde_json::from_str(known).map_err(failure)?;
        if known.len() > 256
            || known
                .iter()
                .any(|id| id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(failure("Invalid known preview resource identities"));
        }
        self.preview_pixels.clear();
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| failure("No browser session"))?;
        let mut preview = {
            #[cfg(feature = "latency-diagnostics")]
            let _capture = crate::latency_diagnostics::phase("preview_scene_capture", "", None);
            session
                .capture_page_with_images(page as usize, &mut self.outlines, &mut self.images)
                .map_err(failure)?
        };
        preview.image_keys = Some(preview.images.keys().cloned().collect());
        preview.images.retain(|id, _| !known.contains(id));
        let (pixels, bytes) = preview.pack_pixels();
        self.preview_pixels = bytes;
        #[derive(serde::Serialize)]
        struct Page<'a> {
            document: &'a str,
            revision: u64,
            pass: usize,
            page: u32,
            preview: crate::pdf::preview::RetainedPreview,
            pixels: BTreeMap<String, [usize; 4]>,
        }
        let tag = self.tag.as_ref().unwrap();
        #[cfg(feature = "latency-diagnostics")]
        let _serialize = crate::latency_diagnostics::phase("preview_serialization", "", None);
        serde_json::to_string(&Page {
            document: &tag.document,
            revision: tag.revision,
            pass: session.settling().map_or(1, |s| s.pass),
            page,
            preview,
            pixels,
        })
        .map_err(failure)
    }
    /// Explicitly acknowledged immutable source-map chunks resident in the renderer.
    pub fn source_map_resident(&self, ids: &str) -> std::result::Result<(), JsValue> {
        let ids: Vec<u64> = serde_json::from_str(ids).map_err(failure)?;
        if ids.len() > 2048 {
            return Err(failure("Source-map resident budget exceeded"));
        }
        crate::source_map::begin_publication(&ids);
        Ok(())
    }
    pub fn source_map_statistics(&self) -> String {
        serde_json::to_string(&crate::source_map::statistics()).unwrap()
    }
    pub fn take_preview_pixels(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.preview_pixels)
    }
    pub fn release_previous(&mut self) {
        if let Some(session) = &mut self.session {
            session.release_previous();
        }
    }
    pub fn restoration_timings(&self) -> std::result::Result<String, JsValue> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| failure("No browser session"))?;
        serde_json::to_string(session.restoration_timings()).map_err(failure)
    }
    pub fn reclaim_retired(&mut self, maximum: u32) -> usize {
        self.session
            .as_mut()
            .map_or(0, |session| session.reclaim_retired(maximum as usize))
    }
    pub fn page_sizes(&self) -> std::result::Result<String, JsValue> {
        let engine = self
            .session
            .as_ref()
            .ok_or_else(|| failure("No browser session"))?
            .engine()
            .map_err(failure)?;
        let sizes: Vec<_> = engine
            .layout
            .pages
            .iter()
            .take(engine.completed_page_count())
            .map(|page| {
                [
                    crate::core::units::sp_to_bp(page.width_sp)
                        * if engine.status() == "running" {
                            page.magnification as f64 / 1000.0
                        } else {
                            1.0
                        },
                    crate::core::units::sp_to_bp(page.height_sp)
                        * if engine.status() == "running" {
                            page.magnification as f64 / 1000.0
                        } else {
                            1.0
                        },
                ]
            })
            .collect();
        serde_json::to_string(&sizes).map_err(failure)
    }
    pub fn checkpoint_status(&self) -> std::result::Result<String, JsValue> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| failure("No browser session"))?;
        Ok(serde_json::json!({"document":self.tag.as_ref().unwrap().document,"revision":self.tag.as_ref().unwrap().revision,
            "checkpoints":session.checkpoint_storage(),"source_pages":session.source_pages(),
            "font_map_preparation_cache":crate::fonts::map_preparation::diagnostics()}).to_string())
    }
    pub fn begin_settling(
        &mut self,
        document: &str,
        revision: u32,
        paths_json: &str,
        max_passes: u32,
    ) -> std::result::Result<(), JsValue> {
        let paths: Vec<Vec<u8>> = serde_json::from_str(paths_json).map_err(failure)?;
        let paths = paths
            .into_iter()
            .map(|bytes| {
                crate::resources::virtual_host::physical_path(
                    &crate::resources::catalog::ResourcePath(bytes),
                )
            })
            .collect::<crate::core::errors::Result<Vec<_>>>()
            .map_err(failure)?;
        self.session
            .as_mut()
            .ok_or_else(|| failure("No browser session"))?
            .begin_settling(document, revision as u64, &paths, max_passes as usize)
            .map_err(failure)
    }
    pub fn layout(&self) -> std::result::Result<String, JsValue> {
        let engine = self
            .session
            .as_ref()
            .ok_or_else(|| failure("No browser session"))?
            .settled_engine()
            .map_err(failure)?;
        let snapshot = crate::diagnostics::snapshot::layout_snapshot(
            &engine.layout,
            &engine.builder,
            &engine.fonts,
            &engine.core.diagnostics,
            engine.status(),
            Some(&engine.core.output_pages),
            engine.core.trace.events.len(),
            engine.core.trace.dropped,
        )
        .map_err(failure)?;
        Ok(serde_json::json!({"document":self.tag.as_ref().unwrap().document,"revision":self.tag.as_ref().unwrap().revision,"complete":engine.status()=="complete","layout":snapshot}).to_string())
    }
    pub fn preview(&mut self) -> std::result::Result<String, JsValue> {
        let engine = self
            .session
            .as_ref()
            .ok_or_else(|| failure("No browser session"))?
            .settled_engine()
            .map_err(failure)?;
        let scene =
            crate::pdf::preview::capture_with_images(engine, &mut self.outlines, &mut self.images)
                .map_err(failure)?;
        serde_json::to_string(&scene).map_err(failure)
    }
    pub fn image_cache_bytes(&self) -> usize {
        self.images.bytes()
    }
    pub fn image_cache_hits(&self) -> u64 {
        self.images.hits()
    }
    pub fn outline_cache_bytes(&self) -> usize {
        self.outlines.bytes()
    }
    pub fn pdf_output_available(&self) -> std::result::Result<bool, JsValue> {
        self.session
            .as_ref()
            .ok_or_else(|| failure("No browser session"))?
            .pdf_output_available()
            .map_err(failure)
    }
    pub fn export_pdf(&mut self) -> std::result::Result<Vec<u8>, JsValue> {
        self.session
            .as_mut()
            .ok_or_else(|| failure("No browser session"))?
            .export_pdf()
            .map_err(failure)?
            .ok_or_else(|| failure("PDF unavailable: failed revision, no pages or draft mode"))
    }
    pub fn staged_output(&self, path: &str) -> std::result::Result<Vec<u8>, JsValue> {
        use crate::resources::host::Host;
        self.session
            .as_ref()
            .ok_or_else(|| failure("No browser session"))?
            .settled_engine()
            .map_err(failure)?;
        self.host.read(Path::new(path)).map_err(failure)
    }
    /// Byte paths permit callers to inspect the complete private file journal.
    pub fn staged_effect_paths(&self) -> std::result::Result<String, JsValue> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| failure("No browser session"))?;
        session.settled_engine().map_err(failure)?;
        let tag = session
            .tag()
            .ok_or_else(|| failure("No document revision"))?;
        let paths: Vec<_> = self
            .host
            .staged_output_paths(tag)
            .map_err(failure)?
            .into_iter()
            .map(|path| path.0)
            .collect();
        let removals: Vec<_> = self
            .host
            .staged_removals(tag)
            .map_err(failure)?
            .into_iter()
            .map(|path| path.0)
            .collect();
        Ok(serde_json::json!({"paths":paths,"removals":removals}).to_string())
    }
    /// Auxiliary publication is an explicit accepted generation. Downloaded PDFs
    /// are produced by export_pdf_bytes and never enter this transaction.
    pub fn commit_outputs(
        &mut self,
        document: &str,
        revision: u32,
    ) -> std::result::Result<(), JsValue> {
        self.session
            .as_mut()
            .ok_or_else(|| failure("No browser session"))?
            .commit_outputs(document, revision as u64)
            .map_err(failure)
    }
    pub fn cancel(&mut self) {
        if let (Some(session), Some(tag)) = (&mut self.session, &self.tag) {
            let _ = session.cancel(&tag.document, tag.revision);
        }
        self.host.discard_effects();
    }
}
impl Drop for BrowserJob {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl BrowserJob {
    fn apply_delta(&mut self, delta: CatalogDelta) -> crate::core::errors::Result<()> {
        if let Some(session) = &mut self.session {
            session.update_resources(delta)
        } else {
            self.host.apply(delta)
        }
    }
}

/// Bounded XZ decoding for the repository catalog; no engine state is involved.
#[wasm_bindgen]
pub fn decode_texlive_xz(bytes: &[u8], limit: u32) -> std::result::Result<Vec<u8>, JsValue> {
    crate::resources::archive::decode_xz(bytes, limit as usize).map_err(failure)
}
#[wasm_bindgen]
pub struct BrowserArchive {
    archive: crate::resources::archive::RuntimeArchive,
}
#[wasm_bindgen]
impl BrowserArchive {
    pub fn manifest(&self) -> String {
        serde_json::to_string(&self.archive.manifest).unwrap()
    }
    pub fn take_bytes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.archive.bytes)
    }
}
#[wasm_bindgen]
pub fn unpack_texlive_archive(
    bytes: &[u8],
    expected: &str,
) -> std::result::Result<BrowserArchive, JsValue> {
    if expected.len() > 4 * 1024 * 1024 {
        return Err(failure("TeX Live manifest transport limit exceeded"));
    }
    let expected: Vec<String> = serde_json::from_str(expected).map_err(failure)?;
    Ok(BrowserArchive {
        archive: crate::resources::archive::unpack_xz(bytes, &expected).map_err(failure)?,
    })
}
