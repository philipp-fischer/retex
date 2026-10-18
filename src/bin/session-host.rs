//! Reproducible native driver for the same resident session used by Wasm.
//! The input is a byte-oriented recorded script plus a packaged catalog. This
//! harness provisions disk assets; engine execution has no filesystem fallback.
use retex_rs2::{
    core::config::EngineConfig,
    engine::RunProgress,
    resources::{
        catalog::{CatalogDelta, ResourceCatalog, ResourceChange, ResourceId, ResourcePath},
        host::{FixedClock, Host},
        virtual_host::VirtualHost,
    },
    session::EditingSession,
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    rc::Rc,
};
#[derive(Deserialize)]
struct Entry {
    path: Vec<u8>,
    id: String,
    version: u64,
    offset: usize,
    length: usize,
}
#[derive(Deserialize)]
struct Script {
    config: EngineConfig,
    bundle: PathBuf,
    manifest: PathBuf,
    initialized_format: Option<String>,
    #[serde(default)]
    host_variables: BTreeMap<String, String>,
    actions: Vec<Action>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Action {
    CheckpointFocus {
        path: String,
        offset: usize,
    },
    SourceTransaction {
        transaction: retex_rs2::session::SourceTransaction,
    },
    Open {
        revision: u64,
        bytes: Vec<u8>,
    },
    Edit {
        base_revision: u64,
        revision: u64,
        start: usize,
        end: usize,
        bytes: Vec<u8>,
    },
    Cancel {
        revision: u64,
    },
    Resource {
        path: String,
        version: u64,
        bytes: Vec<u8>,
        #[serde(default)]
        modified_unix_ms: Option<i64>,
    },
    RemoveResource {
        path: String,
    },
    Advance {
        steps: usize,
    },
    Settle,
    BeginSettling {
        revision: u64,
        paths: Vec<String>,
        max_passes: usize,
    },
    Preview,
    UntilPage {
        page: usize,
        pass: usize,
    },
    PreviewPage {
        page: usize,
    },
    Publish {
        revision: u64,
    },
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let script = args.next().ok_or("Expected script.json")?;
    let output = PathBuf::from(args.next().ok_or("Expected output folder")?);
    if args.next().is_some() {
        return Err("Usage: session-host script.json output-directory".into());
    }
    let script: Script = serde_json::from_slice(&std::fs::read(script)?)?;
    if script.actions.len() > 10_000 {
        return Err("Action count exceeded".into());
    }
    let bytes = std::fs::read(script.bundle)?;
    let entries: Vec<Entry> = serde_json::from_slice(&std::fs::read(script.manifest)?)?;
    let mut changes = Vec::with_capacity(entries.len());
    for entry in entries {
        let end = entry
            .offset
            .checked_add(entry.length)
            .ok_or("Bundle offset overflow")?;
        changes.push(ResourceChange::Put {
            path: ResourcePath(entry.path),
            id: ResourceId(entry.id),
            version: entry.version,
            bytes: bytes
                .get(entry.offset..end)
                .ok_or("Bundle entry outside bytes")?
                .into(),
        });
    }
    let mut catalog = ResourceCatalog::new(Default::default());
    catalog.apply(CatalogDelta {
        base_epoch: 0,
        new_epoch: 1,
        policy_version: 0,
        changes,
    })?;
    let mut variables = BTreeMap::from([
        ("TEXMF_OUTPUT_DIRECTORY".into(), "/project".into()),
        ("TEXFONTMAPS".into(), "/project:/bundle/fonts/assets".into()),
    ]);
    variables.extend(
        script
            .host_variables
            .into_iter()
            .map(|(name, value)| (name, value.into())),
    );
    let host = Rc::new(VirtualHost::new(
        catalog,
        PathBuf::from("/project"),
        variables,
        FixedClock::new(1_600_000_000, 0, 0)?,
        1_000_000,
    )?);
    let mut session = EditingSession::new(script.config, host.clone())?;
    if let Some(path) = script.initialized_format {
        let state = retex_rs2::serialization::formats::decode_snapshot(
            &host.read(Path::new(&path))?,
            Some(256 * 1024 * 1024),
        )?;
        session.load_initialized(state, Path::new("/bundle/formats"))?;
    }
    std::fs::create_dir_all(&output)?;
    let mut records = Vec::new();
    let mut outlines = retex_rs2::fonts::outlines::OutlineCache::default();
    for (index, action) in script.actions.into_iter().enumerate() {
        let result = (|| -> retex_rs2::core::errors::Result<()> {
            match action {
                Action::CheckpointFocus { path, offset } => {
                    session.set_checkpoint_focus("probe", Path::new(&path), offset)?
                }
                Action::SourceTransaction { transaction } => {
                    session.apply_source_transaction(transaction)?
                }
                Action::Open { revision, bytes } => session.open_document(
                    "probe",
                    revision,
                    Path::new("/project/document.tex"),
                    bytes.into(),
                )?,
                Action::Edit {
                    base_revision,
                    revision,
                    start,
                    end,
                    bytes,
                } => session.apply_edit("probe", base_revision, start, end, &bytes, revision)?,
                Action::Cancel { revision } => session.cancel("probe", revision)?,
                Action::BeginSettling {
                    revision,
                    paths,
                    max_passes,
                } => session.begin_settling(
                    "probe",
                    revision,
                    &paths.into_iter().map(PathBuf::from).collect::<Vec<_>>(),
                    max_passes,
                )?,
                Action::Publish { revision } => session.commit_outputs("probe", revision)?,
                Action::Resource {
                    path,
                    version,
                    bytes,
                    modified_unix_ms,
                } => {
                    let epoch = host.epoch();
                    session.update_resources(CatalogDelta {
                        base_epoch: epoch,
                        new_epoch: epoch + 1,
                        policy_version: host.policy_version(),
                        changes: vec![ResourceChange::put(
                            ResourceId(path.clone()),
                            ResourcePath(path.into_bytes()),
                            version,
                            bytes.into(),
                            modified_unix_ms,
                        )?],
                    })?;
                }
                Action::RemoveResource { path } => {
                    let epoch = host.epoch();
                    session.update_resources(CatalogDelta {
                        base_epoch: epoch,
                        new_epoch: epoch + 1,
                        policy_version: host.policy_version(),
                        changes: vec![ResourceChange::Remove(ResourcePath(path.into_bytes()))],
                    })?;
                }
                Action::Advance { steps } => {
                    session.advance(steps)?;
                }
                Action::Preview => {
                    let scene = retex_rs2::pdf::preview::capture(session.engine()?, &mut outlines)?;
                    let revision = session.tag().unwrap().revision;
                    std::fs::write(
                        output.join(format!("revision-{revision}-preview.json")),
                        serde_json::to_vec(&scene).unwrap(),
                    )?;
                }
                Action::UntilPage { page, pass } => {
                    for _ in 0..1_000_000 {
                        if session.settling().map_or(1, |s| s.pass) == pass
                            && session.engine()?.completed_page_count() >= page
                        {
                            break;
                        }
                        if session.advance(128)? != RunProgress::Yielded {
                            return Err(retex_rs2::core::errors::Error::value(
                                "Requested intermediate page unavailable",
                            ));
                        }
                    }
                }
                Action::PreviewPage { page } => {
                    let scene = session.capture_page(page, &mut outlines)?;
                    let revision = session.tag().unwrap().revision;
                    let pass = session.settling().map_or(1, |s| s.pass);
                    std::fs::write(
                        output.join(format!("revision-{revision}-pass-{pass}-page-{page}.json")),
                        serde_json::to_vec(&scene).unwrap(),
                    )?;
                    let effects = host.staged_outputs(session.tag().unwrap())?;
                    let effects: Vec<_>=effects.iter().map(|(path,bytes)|serde_json::json!({"path":path.0,"bytes":bytes.as_ref()})).collect();
                    std::fs::write(
                        output.join(format!(
                            "revision-{revision}-pass-{pass}-page-{page}-outputs.json"
                        )),
                        serde_json::to_vec(&effects).map_err(std::io::Error::other)?,
                    )?;
                }
                Action::Settle => {
                    for _ in 0..1_000_000 {
                        if session.advance(128)? != RunProgress::Yielded {
                            break;
                        }
                    }
                    if session.engine()?.status() == "running" {
                        return Err(retex_rs2::core::errors::Error::value(
                            "Recorded execution did not settle",
                        ));
                    }
                    let engine = session.engine()?;
                    let snapshot = retex_rs2::diagnostics::snapshot::layout_snapshot(
                        &engine.layout,
                        &engine.builder,
                        &engine.fonts,
                        &engine.core.diagnostics,
                        engine.status(),
                        Some(&engine.core.output_pages),
                        engine.core.trace.events.len(),
                        engine.core.trace.dropped,
                    )?;
                    let revision = session.tag().unwrap().revision;
                    std::fs::write(
                        output.join(format!("revision-{revision}.json")),
                        serde_json::to_vec_pretty(&snapshot).unwrap(),
                    )?;
                    let outputs = host.staged_outputs(session.tag().unwrap())?;
                    let observations:Vec<_>=outputs.iter().map(|(path,bytes)|serde_json::json!({"path":path.0,"bytes":bytes.as_ref()})).collect();
                    std::fs::write(
                        output.join(format!("revision-{revision}-outputs.json")),
                        serde_json::to_vec(&observations).unwrap(),
                    )?;
                    let removals: Vec<_> = host
                        .staged_removals(session.tag().unwrap())?
                        .into_iter()
                        .map(|path| path.0)
                        .collect();
                    std::fs::write(
                        output.join(format!("revision-{revision}-removals.json")),
                        serde_json::to_vec(&removals).unwrap(),
                    )?;
                    if let Some(bytes) = session.export_pdf()? {
                        std::fs::write(output.join(format!("revision-{revision}.pdf")), bytes)?;
                    }
                }
            }
            Ok(())
        })();
        records.push(serde_json::json!({"action":index,"revision":session.tag().map(|t|t.revision),"reuse":session.reuse(),"settling":session.settling(),"error":result.err().map(|e|e.to_string())}));
    }
    std::fs::write(
        output.join("actions.json"),
        serde_json::to_vec_pretty(&records)?,
    )?;
    Ok(())
}
