//! Execute the same typed engine against a catalog with no native fallback.
use retex_rs2::{
    Engine,
    core::config::EngineConfig,
    engine::RunProgress,
    resources::{
        catalog::{CatalogDelta, ResourceCatalog, ResourceChange, ResourceId},
        effects::DocumentRevision,
        host::{FixedClock, Host},
        virtual_host::{VirtualHost, resource_path},
    },
};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    rc::Rc,
};

fn tag(revision: u64) -> DocumentRevision {
    DocumentRevision {
        document: "document".into(),
        revision,
    }
}
fn put(path: &Path, bytes: Vec<u8>) -> ResourceChange {
    ResourceChange::Put {
        id: ResourceId(path.display().to_string()),
        path: resource_path(path),
        version: 1,
        bytes: bytes.into(),
    }
}
fn host(source: &[u8]) -> Rc<VirtualHost> {
    let mut catalog = ResourceCatalog::new(Default::default());
    let mut changes = vec![
        put(Path::new("/project/document.tex"), source.to_vec()),
        put(Path::new("/project/part.tex"), b"Included\\par\n".to_vec()),
    ];
    // Preserve canonical package anchors so formats/font resources keep the
    // same identity as native jobs. Only development tests read disk to provision.
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
    for directory in [assets.join("assets"), assets.join("metrics")] {
        for item in std::fs::read_dir(directory).unwrap() {
            let path = item.unwrap().path();
            if path.is_file() {
                changes.push(put(&path, std::fs::read(&path).unwrap()));
            }
        }
    }
    catalog
        .apply(CatalogDelta {
            base_epoch: 0,
            new_epoch: 1,
            policy_version: 0,
            changes,
        })
        .unwrap();
    Rc::new(
        VirtualHost::new(
            catalog,
            PathBuf::from("/project"),
            BTreeMap::from([
                ("TEXMF_OUTPUT_DIRECTORY".into(), "/project".into()),
                ("openout_any".into(), "a".into()),
            ]),
            FixedClock::new(1_600_000_000, 0, 0).unwrap(),
            1_000_000,
        )
        .unwrap(),
    )
}
fn config() -> EngineConfig {
    EngineConfig {
        exact_repro: true,
        interaction_mode: Some(0),
        configuration_paths: Some(vec![]),
        tex_input_roots: Some(vec!["/project".into()]),
        output_directory: Some("/project".into()),
        ..Default::default()
    }
}

#[test]
fn virtual_format_roundtrip_atomically_replaces_both_file_and_manifest() {
    use retex_rs2::language::records::{Quantity, QuantityKind, ValueKey};
    let host = host(b"\\catcode123=1\\catcode125=2\\count7=42\\def\\saved{Stored}\\dump\n");
    let mut settings = config();
    settings.ini = true;
    host.begin_effects(tag(1), 10_000_000).unwrap();
    let mut engine = Engine::new_with_host(settings.clone(), host.clone()).unwrap();
    assert!(engine.start_input(Path::new("/project/document.tex")));
    assert!(engine.main_body());
    let path = Path::new("/project/minimal.rsfmt");
    engine.write_format(path).unwrap();
    let first = host.read(path).unwrap();
    assert!(host.committed(path).unwrap().is_none());
    host.commit_effects(&tag(1)).unwrap();
    let second = Path::new("/project/second.tex");
    host.apply(CatalogDelta {
        base_epoch: host.epoch(),
        new_epoch: host.epoch() + 1,
        policy_version: host.policy_version(),
        changes: vec![put(
            second,
            b"\\catcode123=1\\catcode125=2\\count7=43\\def\\saved{Changed}\\dump\n".to_vec(),
        )],
    })
    .unwrap();
    host.begin_effects(tag(2), 10_000_000).unwrap();
    let mut engine = Engine::new_with_host(settings.clone(), host.clone()).unwrap();
    assert!(engine.start_input(second));
    assert!(engine.main_body());
    engine.write_format(path).unwrap();
    assert_ne!(host.read(path).unwrap(), first);
    assert_eq!(host.committed(path).unwrap().unwrap().as_ref(), first);
    host.commit_effects(&tag(2)).unwrap();
    let mut restored = Engine::new_with_host(settings, host.clone()).unwrap();
    restored.load_format(path).unwrap();
    assert_eq!(
        restored
            .env
            .values
            .get(&ValueKey::register(QuantityKind::Count, 7)),
        Some(&Quantity::Integer(43))
    );
    for child in host.read_dir(Path::new("/project")).unwrap() {
        assert!(!child.to_string_lossy().ends_with(".tmp"), "{child:?}");
    }
}
#[test]
fn project_replacement_retains_bundled_identity_and_rejects_stale_updates_atomically() {
    let host = host(b"\\end");
    let path = resource_path(Path::new("/project/part.tex"));
    let (id, version) = host
        .resource_identity(&resource_path(Path::new("part.tex")))
        .unwrap()
        .unwrap();
    let old = host.read(Path::new("/project/part.tex")).unwrap();
    let epoch = host.epoch();
    let delta = |base_epoch| CatalogDelta {
        base_epoch,
        new_epoch: epoch + 1,
        policy_version: host.policy_version(),
        changes: vec![ResourceChange::Put {
            path: path.clone(),
            id: id.clone(),
            version: version + 1,
            bytes: b"Replacement".to_vec().into(),
        }],
    };
    assert!(host.apply(delta(epoch - 1)).is_err());
    assert_eq!(host.read(Path::new("/project/part.tex")).unwrap(), old);
    assert_eq!(
        host.resource_identity(&path).unwrap(),
        Some((id.clone(), version))
    );
    host.apply(delta(epoch)).unwrap();
    assert_eq!(
        host.resource_identity(&path).unwrap(),
        Some((id, version + 1))
    );
    assert_eq!(
        host.read(Path::new("/project/part.tex")).unwrap(),
        b"Replacement"
    );
    assert!(
        host.resource_identity(&resource_path(Path::new("absent.png")))
            .unwrap()
            .is_none()
    );
}
#[test]
fn catalog_engine_executes_includes_read_after_write_and_exports_pdf_without_disk() {
    let source = br"\pdfinfoomitdate=1 \pdftrailerid{} \immediate\openout1=notes.aux \immediate\write1{Saved data}\immediate\closeout1 \openin1=notes.aux \read1 to\saved \closein1 \saved\par \input part.tex \hbox{Final}\end";
    let host = host(source);
    host.begin_effects(tag(1), 1_000_000).unwrap();
    let mut engine = Engine::new_with_host(config(), host.clone()).unwrap();
    assert!(
        engine.start_input(Path::new("/project/document.tex")),
        "{:?}",
        engine.core.input.last_input_error
    );
    engine.begin_run().unwrap();
    while engine.advance_run(3).unwrap() == RunProgress::Yielded {}
    assert_eq!(
        engine.status(),
        "complete",
        "{:?}",
        engine.core.diagnostics.errors
    );
    assert_eq!(
        host.read(Path::new("/project/notes.aux")).unwrap(),
        b"Saved data\n"
    );
    assert!(
        host.committed(Path::new("/project/notes.aux"))
            .unwrap()
            .is_none()
    );
    engine
        .dump_layout(Path::new("/project/layout.json"))
        .unwrap();
    engine
        .write_pdf(Path::new("/project/document.pdf"))
        .unwrap();
    let pdf = host.read(Path::new("/project/document.pdf")).unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    let pending = host.pending_bytes();
    assert_eq!(engine.export_pdf_bytes().unwrap().unwrap(), pdf);
    assert_eq!(
        host.pending_bytes(),
        pending,
        "PDF download contaminated staged TeX outputs"
    );
    let snapshot: serde_json::Value =
        serde_json::from_slice(&host.read(Path::new("/project/layout.json")).unwrap()).unwrap();
    assert_eq!(snapshot["status"], "complete");
    assert!(!engine.core.output_pages.is_empty());
    host.commit_effects(&tag(1)).unwrap();
    assert_eq!(
        host.committed(Path::new("/project/notes.aux"))
            .unwrap()
            .unwrap()
            .as_ref(),
        b"Saved data\n"
    );
    assert!(engine.builder.nodes.values().all(Option::is_none));
}
#[test]
fn cancelling_a_yielded_engine_discards_outputs_and_old_writer_handles() {
    let host = host(br"\immediate\openout1=cancel.aux \immediate\write1{Obsolete} \def\forever{\forever}\forever");
    host.begin_effects(tag(1), 100).unwrap();
    let mut old_writer = host
        .open_output(Path::new("/project/writer.bin"), false)
        .unwrap();
    let mut engine = Engine::new_with_host(config(), host.clone()).unwrap();
    assert!(engine.start_input(Path::new("/project/document.tex")));
    engine.begin_run().unwrap();
    for _ in 0..100 {
        assert_eq!(engine.advance_run(4).unwrap(), RunProgress::Yielded);
    }
    assert_eq!(
        host.read(Path::new("/project/cancel.aux")).unwrap(),
        b"Obsolete\n"
    );
    drop(engine);
    host.discard_effects();
    assert!(
        host.committed(Path::new("/project/cancel.aux"))
            .unwrap()
            .is_none()
    );
    host.begin_effects(tag(2), 100).unwrap();
    assert!(old_writer.write_all(b"late").is_err());
    assert_eq!(host.pending_bytes(), 0);
    assert!(host.commit_effects(&tag(1)).is_err());
    assert!(
        host.committed(Path::new("/project/writer.bin"))
            .unwrap()
            .is_none()
    );
}
#[test]
fn virtual_output_limits_reject_growth_without_partial_writes() {
    let host = host(b"\\end");
    host.begin_effects(tag(1), 3).unwrap();
    let mut output = host
        .open_output(Path::new("/project/bounded.bin"), false)
        .unwrap();
    output.write_all(b"abc").unwrap();
    assert!(output.write_all(b"def").is_err());
    assert_eq!(host.pending_bytes(), 3);
    assert_eq!(
        host.read(Path::new("/project/bounded.bin")).unwrap(),
        b"abc"
    );
    host.discard_effects();
    let mut catalog = ResourceCatalog::new(Default::default());
    catalog
        .apply(CatalogDelta {
            base_epoch: 0,
            new_epoch: 1,
            policy_version: 0,
            changes: vec![put(Path::new("/project/document.tex"), b"\\end".to_vec())],
        })
        .unwrap();
    let missing_fonts = Rc::new(
        VirtualHost::new(
            catalog,
            PathBuf::from("/project"),
            BTreeMap::new(),
            FixedClock::new(1_600_000_000, 0, 0).unwrap(),
            100,
        )
        .unwrap(),
    );
    assert!(Engine::new_with_host(config(), missing_fonts).is_err());
}

fn settled(session: &mut retex_rs2::session::EditingSession) -> RunProgress {
    for _ in 0..100_000 {
        let progress = session.advance(16).unwrap();
        if progress != RunProgress::Yielded {
            return progress;
        }
    }
    panic!("Session did not settle within the bounded test budget");
}
#[test]
fn resident_pdf_availability_matches_native_publication_and_recovers_after_cancel() {
    use retex_rs2::session::EditingSession;
    for (source, available) in [
        (b"\\pdfoutput=1\\end\n".as_slice(), false),
        (b"\\pdfoutput=1 Visible\\par\\end\n".as_slice(), true),
        (
            b"\\pdfoutput=1\\pdfdraftmode=1 Visible\\par\\end\n".as_slice(),
            false,
        ),
    ] {
        let mut session = EditingSession::new(config(), host(source)).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.into(),
            )
            .unwrap();
        assert!(session.pdf_output_available().is_err());
        assert_eq!(settled(&mut session), RunProgress::Complete);
        assert_eq!(session.pdf_output_available().unwrap(), available);
        assert_eq!(session.export_pdf().unwrap().is_some(), available);
        session.cancel("document", 1).unwrap();
        assert!(session.pdf_output_available().is_err());
        let recovery = b"\\pdfoutput=1 Recovered\\par\\end\n";
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                recovery.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        assert!(session.pdf_output_available().unwrap());
        assert!(session.export_pdf().unwrap().is_some());
    }
}
fn observed(engine: &Engine) -> serde_json::Value {
    retex_rs2::diagnostics::snapshot::layout_snapshot(
        &engine.layout,
        &engine.builder,
        &engine.fonts,
        &engine.core.diagnostics,
        engine.status(),
        Some(&engine.core.output_pages),
        engine.core.trace.events.len(),
        engine.core.trace.dropped,
    )
    .unwrap()
}
fn assert_material_eq(incremental: &Engine, fresh: &Engine) {
    // Reuse allocates independent graphs and intentionally changes allocation
    // IDs/costs. Compare canonical graph topology and all semantic output fields.
    // Jobs that never imported material retain the original exact comparison.
    if incremental.execution_reuse_stats().contains_reused_graphs {
        assert_eq!(
            reusable_material_snapshot(observed(incremental)),
            reusable_material_snapshot(observed(fresh))
        );
    } else {
        assert_eq!(observed(incremental), observed(fresh));
    }
}
fn compare_fresh(session: &mut retex_rs2::session::EditingSession, source: &[u8]) {
    compare_fresh_resources(session, source, &[]);
}
fn compare_fresh_resources(
    session: &mut retex_rs2::session::EditingSession,
    source: &[u8],
    resources: &[(&str, Vec<u8>)],
) {
    compare_fresh_config(session, source, resources, config());
}
fn compare_fresh_config(
    session: &mut retex_rs2::session::EditingSession,
    source: &[u8],
    resources: &[(&str, Vec<u8>)],
    settings: EngineConfig,
) {
    let fresh_host = host(source);
    if !resources.is_empty() {
        fresh_host
            .apply(CatalogDelta {
                base_epoch: fresh_host.epoch(),
                new_epoch: fresh_host.epoch() + 1,
                policy_version: fresh_host.policy_version(),
                changes: resources
                    .iter()
                    .map(|(name, bytes)| {
                        let path = Path::new("/project").join(name);
                        let mut change = put(&path, bytes.clone());
                        if let Some((existing_id, existing_version)) =
                            fresh_host.resource_identity(&resource_path(&path)).unwrap()
                        {
                            if let ResourceChange::Put { id, version, .. } = &mut change {
                                *id = existing_id;
                                *version = existing_version + 1;
                            }
                        }
                        change
                    })
                    .collect(),
            })
            .unwrap();
    }
    fresh_host.begin_effects(tag(1), 1_000_000).unwrap();
    let mut fresh = Engine::new_with_host(settings, fresh_host.clone()).unwrap();
    assert!(fresh.start_input(Path::new("/project/document.tex")));
    fresh.main_body();
    assert_material_eq(session.engine().unwrap(), &fresh);
    assert_eq!(
        session.engine().unwrap().env.values,
        fresh.env.values,
        "Register/global state differs"
    );
    // CommandOperand's PartialEq deliberately uses Arc identity for macro operands.
    // Across independent jobs compare the complete serialized macro/token state.
    assert_eq!(
        session.engine().unwrap().env.meanings.len(),
        fresh.env.meanings.len()
    );
    for (key, meaning) in &fresh.env.meanings {
        assert_eq!(
            serde_json::to_value(session.engine().unwrap().env.meanings.get(key)).unwrap(),
            serde_json::to_value(Some(meaning)).unwrap(),
            "Macro state differs at {key}"
        );
    }
    assert_eq!(
        session.export_pdf().unwrap(),
        fresh.export_pdf_bytes().unwrap()
    );
    let current = session.tag().unwrap().clone();
    assert_eq!(
        session.host().staged_outputs(&current).unwrap(),
        fresh_host.staged_outputs(&tag(1)).unwrap()
    );
}
#[test]
fn resident_body_rollback_preserves_groups_globals_boxes_and_open_writers() {
    use retex_rs2::session::EditingSession;
    let source=b"\\pdfinfoomitdate=1 \\pdftrailerid{}\n\\count0=3 \\def\\word{First}\n\\immediate\\openout1=notes.aux\n{\\global\\advance\\count0 by7 \\setbox0=\\hbox{Saved}\n\\word \\copy0 \\the\\count0\\par\n\\global\\advance\\count0 by2 \\immediate\\write1{\\the\\count0}\nOriginal body\\par\n}\\immediate\\closeout1\\end\n";
    let mut session = EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, source);
    let start = source
        .windows(b"Original".len())
        .position(|s| s == b"Original")
        .unwrap();
    let mut edited = source.to_vec();
    edited.splice(start..start + 8, b"Edited".iter().copied());
    session
        .apply_edit("document", 1, start, start + 8, b"Edited", 2)
        .unwrap();
    assert!(
        session.reuse().restored_offset > 0,
        "No live prefix was reused: {:?}",
        session.reuse()
    );
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, &edited);
    assert!(session.reuse().checkpoint_count <= 8);
    // A preamble change rolls back beyond the first changed byte, including
    // suffix globals. Undo may reuse only a prefix shared with the new bytes.
    let preamble = edited.windows(6).position(|s| s == b"First}").unwrap();
    edited.splice(preamble..preamble + 5, b"Other".iter().copied());
    session
        .apply_edit("document", 2, preamble, preamble + 5, b"Other", 3)
        .unwrap();
    assert!(session.reuse().restored_offset <= preamble);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, &edited);
}
#[test]
fn stale_edits_cancel_and_incomplete_recovery_do_not_contaminate_session() {
    use retex_rs2::session::EditingSession;
    let source=b"\\pdfinfoomitdate=1 \\pdftrailerid{}\n\\def\\word{Prefix}\n\\word\\par\nValid body\\par\\end\n";
    let mut session = EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert!(session.apply_edit("document", 0, 0, 0, b"bad", 2).is_err());
    assert_eq!(session.tag().unwrap().revision, 1);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let start = source.windows(5).position(|s| s == b"Valid").unwrap();
    let broken = b"\\hbox{unfinished";
    session
        .apply_edit("document", 1, start, source.len(), broken, 2)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Failed);
    let broken_len = start + broken.len();
    session
        .apply_edit("document", 2, start, broken_len, &source[start..], 3)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, source);
    session
        .apply_edit(
            "document",
            3,
            start,
            source.len(),
            b"\\def\\loop{\\loop}\\loop",
            4,
        )
        .unwrap();
    assert_eq!(session.advance(100).unwrap(), RunProgress::Yielded);
    assert!(session.cancel("document", 3).is_err());
    session.cancel("document", 4).unwrap();
    assert_eq!(session.host().pending_bytes(), 0);
    assert!(session.advance(1).is_err());
    session
        .open_document(
            "document",
            5,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, source);
}
#[test]
fn resource_deltas_invalidate_live_prefix_and_invalid_deltas_preserve_active_revision() {
    use retex_rs2::session::EditingSession;
    let source = b"\\pdfinfoomitdate=1 \\pdftrailerid{}\n\\input part.tex\nBody\\par\\end\n";
    let mut session = EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert!(
        session
            .update_resources(CatalogDelta {
                base_epoch: 0,
                new_epoch: 1,
                policy_version: 0,
                changes: vec![]
            })
            .is_err()
    );
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let epoch = session.host().epoch();
    let mut resource = put(Path::new("/project/part.tex"), b"Replaced\\par\n".to_vec());
    if let ResourceChange::Put { version, .. } = &mut resource {
        *version = 2;
    }
    session
        .update_resources(CatalogDelta {
            base_epoch: epoch,
            new_epoch: epoch + 1,
            policy_version: 0,
            changes: vec![resource],
        })
        .unwrap();
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(session.reuse().restored_offset, 0);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let fresh_host = host(source);
    let epoch = fresh_host.epoch();
    let mut change = put(Path::new("/project/part.tex"), b"Replaced\\par\n".to_vec());
    if let ResourceChange::Put { version, .. } = &mut change {
        *version = 2;
    }
    fresh_host
        .apply(CatalogDelta {
            base_epoch: epoch,
            new_epoch: epoch + 1,
            policy_version: 0,
            changes: vec![change],
        })
        .unwrap();
    fresh_host.begin_effects(tag(1), 1_000_000).unwrap();
    let mut fresh = Engine::new_with_host(config(), fresh_host).unwrap();
    assert!(fresh.start_input(Path::new("/project/document.tex")));
    assert!(fresh.main_body());
    assert_material_eq(session.engine().unwrap(), &fresh);
}

#[test]
fn body_resource_changes_reuse_preamble_without_replaying_obsolete_input_or_effects() {
    use retex_rs2::session::EditingSession;
    let source = b"\\pdfinfoomitdate=1 \\pdftrailerid{}\n\\def\\word{Preamble} \\count0=7\n\\immediate\\openout1=notes.aux\n\\immediate\\write1{Prefix: \\word}\nFirst paragraph\\par\n\\input part.tex\n\\immediate\\write1{Body: \\the\\count0}\\immediate\\closeout1\n\\word \\the\\count0\\par\\end\n";
    let before_input = source.windows(6).position(|s| s == b"\\input").unwrap();
    let preamble = source.windows(5).position(|s| s == b"First").unwrap();
    let mut session = EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, source);
    for (revision, bytes) in [
        (2, b"\\advance\\count0 by5 Changed\\par\n".as_slice()),
        (3, b"\\advance\\count0 by2 Another\\par\n".as_slice()),
    ] {
        let mut change = put(Path::new("/project/part.tex"), bytes.to_vec());
        if let ResourceChange::Put { version, .. } = &mut change {
            *version = revision;
        }
        session
            .update_resources(CatalogDelta {
                base_epoch: session.host().epoch(),
                new_epoch: session.host().epoch() + 1,
                policy_version: session.host().policy_version(),
                changes: vec![change],
            })
            .unwrap();
        session
            .open_document(
                "document",
                revision,
                Path::new("/project/document.tex"),
                source.as_slice().into(),
            )
            .unwrap();
        let offset = session.reuse().restored_offset;
        assert!(
            offset >= preamble && offset <= before_input,
            "Unsafe or absent body-resource rollback: {offset}"
        );
        assert_eq!(settled(&mut session), RunProgress::Complete);
        compare_fresh_resources(&mut session, source, &[("part.tex", bytes.to_vec())]);
    }
    assert_eq!(session.reuse().initialized_restarts, 1);
    // A lookup-policy change is a full invalidation even with identical bytes.
    session
        .update_resources(CatalogDelta {
            base_epoch: session.host().epoch(),
            new_epoch: session.host().epoch() + 1,
            policy_version: session.host().policy_version() + 1,
            changes: vec![],
        })
        .unwrap();
    session
        .open_document(
            "document",
            4,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(session.reuse().restored_offset, 0);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh_resources(
        &mut session,
        source,
        &[("part.tex", b"\\advance\\count0 by2 Another\\par\n".to_vec())],
    );
}

#[test]
fn late_body_resource_supply_reuses_prefix_and_recovers_from_invalid_input() {
    use retex_rs2::session::{EditingSession, SessionProgress};
    let source = b"\\pdfinfoomitdate=1 \\pdftrailerid{}\n\\def\\word{Prefix}\\count0=4\n\\immediate\\openout1=notes.aux\n\\immediate\\write1{\\word}\nFirst paragraph\\par\n\\input late-body.tex\nLast paragraph \\the\\count0\\par\\immediate\\closeout1\\end\n";
    let boundary = source.windows(6).position(|s| s == b"\\input").unwrap();
    let preamble = source.windows(5).position(|s| s == b"First").unwrap();
    let mut session = EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    let request = loop {
        match session.advance_interactive(8).unwrap() {
            SessionProgress::NeedResource(request) => break request,
            SessionProgress::Running(progress) => assert_eq!(progress, RunProgress::Yielded),
        }
    };
    let malformed = b"\\hbox{unfinished";
    session
        .resolve_resource(
            "document",
            1,
            request.request_id,
            Some(CatalogDelta {
                base_epoch: session.host().epoch(),
                new_epoch: session.host().epoch() + 1,
                policy_version: session.host().policy_version(),
                changes: vec![put(Path::new("/project/late-body.tex"), malformed.to_vec())],
            }),
        )
        .unwrap();
    assert!(
        session.reuse().restored_offset >= preamble && session.reuse().restored_offset <= boundary,
        "Optional probe rollback {:?}, preamble {preamble}, boundary {boundary}",
        session.reuse()
    );
    assert_eq!(settled(&mut session), RunProgress::Failed);
    let fresh_host = host(source);
    fresh_host
        .apply(CatalogDelta {
            base_epoch: fresh_host.epoch(),
            new_epoch: fresh_host.epoch() + 1,
            policy_version: fresh_host.policy_version(),
            changes: vec![put(Path::new("/project/late-body.tex"), malformed.to_vec())],
        })
        .unwrap();
    fresh_host.begin_effects(tag(1), 1_000_000).unwrap();
    let mut fresh = Engine::new_with_host(config(), fresh_host.clone()).unwrap();
    assert!(fresh.start_input(Path::new("/project/document.tex")));
    assert!(!fresh.main_body());
    assert_material_eq(session.engine().unwrap(), &fresh);
    assert_eq!(session.engine().unwrap().env.values, fresh.env.values);
    assert_eq!(
        serde_json::to_value(&session.engine().unwrap().env.meanings).unwrap(),
        serde_json::to_value(&fresh.env.meanings).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&session.engine().unwrap().core.backend.document).unwrap(),
        serde_json::to_value(&fresh.core.backend.document).unwrap()
    );
    assert_eq!(
        session.host().staged_outputs(&tag(1)).unwrap(),
        fresh_host.staged_outputs(&tag(1)).unwrap()
    );
    assert!(session.export_pdf().unwrap().is_none());
    let repaired = b"\\advance\\count0 by3 Repaired\\par\n";
    let mut change = put(Path::new("/project/late-body.tex"), repaired.to_vec());
    if let ResourceChange::Put { version, .. } = &mut change {
        *version = 2;
    }
    session
        .update_resources(CatalogDelta {
            base_epoch: session.host().epoch(),
            new_epoch: session.host().epoch() + 1,
            policy_version: session.host().policy_version(),
            changes: vec![change],
        })
        .unwrap();
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert!(
        session.reuse().restored_offset >= preamble && session.reuse().restored_offset <= boundary,
        "Probe rollback {:?}, preamble {preamble}, boundary {boundary}",
        session.reuse()
    );
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh_resources(
        &mut session,
        source,
        &[("late-body.tex", repaired.to_vec())],
    );
    assert_eq!(session.reuse().initialized_restarts, 1);
}

#[test]
fn newly_present_optional_body_file_invalidates_negative_queries() {
    use retex_rs2::session::EditingSession;
    let source = b"\\pdfinfoomitdate=1 \\pdftrailerid{}\n\\immediate\\openout1=notes.aux\nFirst paragraph\\par\n\\edef\\size{\\pdffilesize{optional.dat}}\n\\immediate\\write1{Size: \\size}\\immediate\\closeout1\nSize: \\size\\par\\end\n";
    let probe = source.windows(5).position(|s| s == b"\\edef").unwrap();
    // A checkpoint can already hold this complete physical line while the
    // definition (and its file query) still waits in the token input.
    let boundary = probe + source[probe..].iter().position(|b| *b == b'\n').unwrap() + 1;
    let preamble = source.windows(5).position(|s| s == b"First").unwrap();
    let mut session = EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, source);
    session
        .update_resources(CatalogDelta {
            base_epoch: session.host().epoch(),
            new_epoch: session.host().epoch() + 1,
            policy_version: session.host().policy_version(),
            changes: vec![put(Path::new("/project/optional.dat"), b"12345".to_vec())],
        })
        .unwrap();
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert!(
        session.reuse().restored_offset >= preamble && session.reuse().restored_offset <= boundary,
        "Probe rollback {:?}, preamble {preamble}, boundary {boundary}",
        session.reuse()
    );
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh_resources(&mut session, source, &[("optional.dat", b"12345".to_vec())]);
    session
        .update_resources(CatalogDelta {
            base_epoch: session.host().epoch(),
            new_epoch: session.host().epoch() + 1,
            policy_version: session.host().policy_version(),
            changes: vec![ResourceChange::Remove(resource_path(Path::new(
                "/project/optional.dat",
            )))],
        })
        .unwrap();
    session
        .open_document(
            "document",
            3,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert!(
        session.reuse().restored_offset >= preamble && session.reuse().restored_offset <= boundary
    );
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, source);
    assert_eq!(session.reuse().initialized_restarts, 1);
}

#[test]
fn suspended_conditionals_catcodes_random_and_delayed_writes_replay_exactly() {
    use retex_rs2::session::EditingSession;
    let source=b"\\pdfinfoomitdate=1 \\pdftrailerid{}\r\n\\pdfsetrandomseed17 \\def\\word{Prefix}\r\n\\immediate\\openout1=notes.aux\r\n{\\catcode63=13 \\def?{Active}\\ifnum\\count0=0\r\n? \\word \\pdfuniformdeviate1000\\par\r\n\\write1{\\word: \\the\\count0}\r\nBody sentence\\par\r\n\\else Wrong\\fi}\r\n\\end\r\n";
    let mut session = EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, source);
    let start = source.windows(4).position(|s| s == b"Body").unwrap();
    let mut edited = source.to_vec();
    edited.splice(start..start + 4, b"Edited".iter().copied());
    session
        .apply_edit("document", 1, start, start + 4, b"Edited", 2)
        .unwrap();
    assert!(session.reuse().restored_offset > 0);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, &edited);
}
#[test]
fn bounded_checkpoint_retention_and_rejected_publication_keep_current_effects() {
    use retex_rs2::session::EditingSession;
    let source=b"\\pdfinfoomitdate=1 \\pdftrailerid{}\n\\def\\word{Prefix}\n\\immediate\\openout1=notes.aux\n\\immediate\\write1{Accepted bytes}\n\\word\\par\nBody\\par\n\\end\n";
    let mut session = EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let current = session.tag().unwrap().clone();
    let outputs = session.host().staged_outputs(&current).unwrap();
    assert!(session.host().commit_effects(&tag(0)).is_err());
    assert_eq!(session.host().staged_outputs(&current).unwrap(), outputs);
    for revision in 2..=101 {
        let mut bytes = source.to_vec();
        let start = bytes.windows(4).position(|s| s == b"Body").unwrap();
        bytes.splice(start..start + 4, format!("Body {revision}").bytes());
        session
            .open_document(
                "document",
                revision,
                Path::new("/project/document.tex"),
                bytes.clone().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        assert!(session.reuse().checkpoint_count <= 8);
        assert!(session.host().pending_bytes() < 100);
        assert!(session.host().epoch() >= revision);
        if revision == 101 {
            compare_fresh(&mut session, &bytes);
        }
    }
}

#[test]
fn clock_dependent_prefixes_invalidate_transitively_and_refresh_initialized_dates() {
    use retex_rs2::session::EditingSession;
    let source=b"\\pdfinfoomitdate=1 \\pdftrailerid{}\n\\edef\\stamp{\\pdfcreationdate}\n\\stamp\\par\n\\def\\word{Prefix}\n\\word\\par\nBody\\par\n\\end\n";
    let mut session = EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let mut edited = source.to_vec();
    let start = edited.windows(4).position(|s| s == b"Body").unwrap();
    edited.splice(start..start + 4, b"Edited".iter().copied());
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            edited.clone().into(),
        )
        .unwrap();
    assert!(session.reuse().restored_offset > 0);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, &edited);
    session
        .host()
        .set_clock(FixedClock::new(1_600_000_001, 0, 0).unwrap());
    session
        .open_document(
            "document",
            3,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let fresh_host = host(source);
    fresh_host.set_clock(FixedClock::new(1_600_000_001, 0, 0).unwrap());
    fresh_host.begin_effects(tag(1), 1_000_000).unwrap();
    let mut fresh = Engine::new_with_host(config(), fresh_host).unwrap();
    assert!(fresh.start_input(Path::new("/project/document.tex")));
    assert!(fresh.main_body());
    assert_material_eq(session.engine().unwrap(), &fresh);
    assert_eq!(
        serde_json::to_value(
            session
                .engine()
                .unwrap()
                .env
                .meanings
                .get("control-word:stamp")
        )
        .unwrap(),
        serde_json::to_value(fresh.env.meanings.get("control-word:stamp")).unwrap()
    );
}

#[test]
fn opened_reader_observes_a_rewrite_before_its_first_read() {
    // The original stream uses a buffered file handle, not an open-time byte copy.
    let source=b"\\pdfinfoomitdate=1 \\pdftrailerid{}\n\\immediate\\openout1=notes.aux \\immediate\\write1{Before}\\immediate\\closeout1\n\\openin1=notes.aux\n\\immediate\\openout1=notes.aux \\immediate\\write1{After}\\immediate\\closeout1\n\\read1 to\\saved \\closein1\n\\immediate\\openout2=observation.txt \\immediate\\write2{\\saved}\\immediate\\closeout2\n\\saved\\par\\end\n";
    let host = host(source);
    host.begin_effects(tag(1), 1_000_000).unwrap();
    let mut engine = Engine::new_with_host(config(), host.clone()).unwrap();
    assert!(engine.start_input(Path::new("/project/document.tex")));
    assert!(engine.main_body());
    assert_eq!(
        host.read(Path::new("/project/observation.txt")).unwrap(),
        b"After \n"
    );
}

#[test]
fn buffered_reader_retains_cached_bytes_refills_writes_and_rejects_cancellation() {
    use retex_rs2::core::characters::LanguageProfile;
    let host = host(b"\\end");
    host.begin_effects(tag(1), 30_000).unwrap();
    let path = Path::new("/project/input.txt");
    let mut writer = host.open_output(path, false).unwrap();
    writer.write_all(b"First\r\nCached\r\n").unwrap();
    // A refill crosses the 8192-byte read-ahead boundary.
    writer.write_all(&vec![b'x'; 8192 - 15]).unwrap();
    writer.write_all(b"Old tail\n").unwrap();
    let mut reader = host.open_physical(path).unwrap().unwrap();
    assert_eq!(
        reader.readline(None, LanguageProfile::Pdftex).unwrap(),
        Some("First\n".into())
    );
    assert_eq!(reader.byte_position(), Some(7));
    let mut replacement = host.open_output(path, false).unwrap();
    replacement.write_all(&vec![b'y'; 8192]).unwrap();
    replacement.write_all(b"New tail\n").unwrap();
    assert_eq!(
        reader.readline(None, LanguageProfile::Pdftex).unwrap(),
        Some("Cached\n".into())
    );
    assert_eq!(
        reader.readline(None, LanguageProfile::Pdftex).unwrap(),
        Some(format!("{}New tail\n", "x".repeat(8192 - 15)))
    );
    assert_eq!(
        reader.readline(None, LanguageProfile::Pdftex).unwrap(),
        None
    );
    let mut append = host.open_output(path, true).unwrap();
    append.write_all("Caf\u{e9}\r\n".as_bytes()).unwrap();
    assert_eq!(
        reader.readline(None, LanguageProfile::Unicode).unwrap(),
        Some("Caf\u{e9}\n".into())
    );
    // Cancellation must reject a read even if an old buffer is still present.
    let mut cached = host.open_physical(path).unwrap().unwrap();
    assert_eq!(
        cached.readline(Some(1), LanguageProfile::Pdftex).unwrap(),
        Some("y".into())
    );
    host.discard_effects();
    host.begin_effects(tag(2), 30_000).unwrap();
    assert!(cached.readline(None, LanguageProfile::Pdftex).is_err());
}

#[test]
fn resident_rollback_rebinds_open_readers_and_edited_main_read_ahead() {
    use retex_rs2::session::EditingSession;
    let source=b"\\pdfinfoomitdate=1 \\pdftrailerid{}\n\\immediate\\openout1=notes.aux \\immediate\\write1{Before}\\immediate\\closeout1\n\\openin1=notes.aux\n\\relax\n\\immediate\\openout1=notes.aux \\immediate\\write1{After}\\immediate\\closeout1\n\\read1 to\\saved \\closein1\n\\immediate\\openout2=observation.txt \\immediate\\write2{\\saved}\\immediate\\closeout2\n\\saved\\par\\end\n";
    let mut session = EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, source);
    let start = source.windows(5).position(|s| s == b"After").unwrap();
    let mut edited = source.to_vec();
    edited.splice(start..start + 5, b"Changed".iter().copied());
    session
        .apply_edit("document", 1, start, start + 5, b"Changed", 2)
        .unwrap();
    let opened = source.windows(7).position(|s| s == b"\\relax\n").unwrap();
    assert!(session.reuse().restored_offset > opened);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, &edited);
    assert_eq!(
        session
            .host()
            .read(Path::new("/project/observation.txt"))
            .unwrap(),
        b"Changed \n"
    );
}

#[test]
fn rejected_catalog_publication_preserves_all_speculative_files_and_handles() {
    use retex_rs2::resources::catalog::CatalogLimits;
    let mut catalog = ResourceCatalog::new(CatalogLimits {
        max_resource_bytes: 8,
        max_total_bytes: 10,
        ..Default::default()
    });
    catalog
        .apply(CatalogDelta {
            base_epoch: 0,
            new_epoch: 1,
            policy_version: 0,
            changes: vec![put(Path::new("/project/document.tex"), b"\\end".to_vec())],
        })
        .unwrap();
    let host = VirtualHost::new(
        catalog,
        "/project".into(),
        BTreeMap::new(),
        FixedClock::new(1_600_000_000, 0, 0).unwrap(),
        100,
    )
    .unwrap();
    host.begin_effects(tag(1), 20).unwrap();
    let mut writer = host
        .open_output(Path::new("/project/notes.aux"), false)
        .unwrap();
    writer.write_all(b"123456789").unwrap();
    let before = host.staged_outputs(&tag(1)).unwrap();
    assert!(host.commit_effects(&tag(1)).is_err());
    assert_eq!(host.epoch(), 1);
    assert_eq!(host.staged_outputs(&tag(1)).unwrap(), before);
    writer.write_all(b"X").unwrap();
    assert_eq!(
        host.read(Path::new("/project/notes.aux")).unwrap(),
        b"123456789X"
    );
    host.open_output(Path::new("/project/notes.aux"), false)
        .unwrap()
        .write_all(b"OK")
        .unwrap();
    host.commit_effects(&tag(1)).unwrap();
    assert_eq!(
        host.committed(Path::new("/project/notes.aux"))
            .unwrap()
            .unwrap()
            .as_ref(),
        b"OK"
    );
}

#[test]
fn nested_boxes_prefixes_and_assignment_expansion_yield_cancel_and_recover() {
    use retex_rs2::session::EditingSession;
    let bodies = [
        r"\hbox{\loop}",
        r"\vbox{\hbox{\loop}}",
        r"\setbox0=\hbox{\loop}",
        r"\global\setbox0=\hbox{\loop}",
        r"\shipout\vbox{\hbox{\loop}}",
        r"\insert2{\hbox{\loop}}",
        r"X\vadjust{\hbox{\loop}}",
        r"\hbox{\edef\value{\loop}}",
        r"\hbox{\count0=\loop}",
        r"\hbox{\global\loop}",
        r"$\loop$",
        r"$$\loop$$",
        r"$x^\loop$",
        r"$\mathchar\loop$",
        r"$\left\loop$",
        r"$x\above\loop$",
        r"$\radical\loop$",
        r"$\mathaccent\loop$",
        r"$\mskip\loop$",
        r"$\mkern\loop$",
        r"$x_\hbox{\loop}$",
        r"$\vcenter{\loop}$",
        r"$$x\eqno y$\loop$",
    ];
    for body in bodies {
        let source = format!(
            "\\pdfinfoomitdate=1 \\pdftrailerid{{}}\n\\def\\loop{{\\loop}}\n\\immediate\\openout1=cancel.aux \\immediate\\write1{{Speculative}}\n{body}\n"
        );
        let mut session = EditingSession::new(config(), host(source.as_bytes())).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        for _ in 0..100 {
            assert_eq!(session.advance(8).unwrap(), RunProgress::Yielded, "{body}");
        }
        assert!(
            !session.engine().unwrap().builder.context_stack.is_empty(),
            "Did not reach nested work: {body}"
        );
        assert!(session.host().pending_bytes() > 0);
        session.cancel("document", 1).unwrap();
        assert_eq!(session.host().pending_bytes(), 0);
        assert!(session.advance(1).is_err());
        let repaired =
            b"\\pdfinfoomitdate=1 \\pdftrailerid{}\n\\def\\loop{done}\n\\hbox{Recovered}\\end\n";
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                repaired.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        compare_fresh(&mut session, repaired);
    }
}

#[test]
fn required_resource_waits_are_revision_safe_and_optional_queries_remain_absent() {
    use retex_rs2::session::{EditingSession, SessionProgress};
    let source = b"\\pdfinfoomitdate=1 \\pdftrailerid{}\n\\immediate\\openout1=notes.aux \\immediate\\write1{Once}\n\\input part\n\\immediate\\closeout1\\end\n";
    let virtual_host = host(source);
    virtual_host
        .apply(CatalogDelta {
            base_epoch: virtual_host.epoch(),
            new_epoch: virtual_host.epoch() + 1,
            policy_version: virtual_host.policy_version(),
            changes: vec![ResourceChange::Remove(resource_path(Path::new(
                "/project/part.tex",
            )))],
        })
        .unwrap();
    let mut session = EditingSession::new(config(), virtual_host).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    let request = loop {
        match session.advance_interactive(8).unwrap() {
            SessionProgress::NeedResource(request) => break request,
            SessionProgress::Running(progress) => assert_eq!(progress, RunProgress::Yielded),
        }
    };
    assert_eq!(request.resource.name, b"part");
    assert_eq!(
        request.resource.directory.as_deref(),
        Some(b"/project".as_slice())
    );
    assert!(session.host().pending_bytes() > 0);
    assert!(session.settled_engine().is_err());
    assert!(session.export_pdf().is_err());
    assert!(session.commit_outputs("document", 1).is_err());
    for (document, revision, id) in [
        ("obsolete", 1, request.request_id),
        ("document", 2, request.request_id),
        ("document", 1, request.request_id + 1),
    ] {
        assert!(
            session
                .resolve_resource(document, revision, id, None)
                .is_err()
        );
    }
    let epoch = session.host().epoch();
    let delta = CatalogDelta {
        base_epoch: epoch,
        new_epoch: epoch + 1,
        policy_version: session.host().policy_version(),
        changes: vec![put(
            Path::new("/project/part.tex"),
            b"Included\\par\n".to_vec(),
        )],
    };
    let mut invalid = delta.clone();
    invalid.base_epoch -= 1;
    assert!(
        session
            .resolve_resource("document", 1, request.request_id, Some(invalid))
            .is_err()
    );
    let mut replace_source = delta.clone();
    replace_source
        .changes
        .push(ResourceChange::Remove(resource_path(Path::new(
            "/project/document.tex",
        ))));
    assert!(
        session
            .resolve_resource("document", 1, request.request_id, Some(replace_source))
            .is_err()
    );
    let mut alias_source = delta.clone();
    alias_source.changes.push(ResourceChange::Remove(
        retex_rs2::resources::catalog::ResourcePath(b"/project/directory/../document.tex".to_vec()),
    ));
    assert!(
        session
            .resolve_resource("document", 1, request.request_id, Some(alias_source))
            .is_err()
    );
    assert_eq!(
        session.needed_resource().unwrap().request_id,
        request.request_id
    );
    assert_eq!(session.host().epoch(), epoch);
    session
        .resolve_resource("document", 1, request.request_id, Some(delta))
        .unwrap();
    assert!(session.needed_resource().is_none());
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, source);
    assert!(
        session
            .resolve_resource("document", 1, request.request_id, None)
            .is_err()
    );

    let missing = b"Before\\par\n\\input absent\n";
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            missing.as_slice().into(),
        )
        .unwrap();
    let denied = loop {
        match session.advance_interactive(8).unwrap() {
            SessionProgress::NeedResource(request) => break request,
            SessionProgress::Running(progress) => assert_eq!(progress, RunProgress::Yielded),
        }
    };
    let retained = observed(session.engine().unwrap());
    session
        .resolve_resource("document", 2, denied.request_id, None)
        .unwrap();
    assert_eq!(observed(session.settled_engine().unwrap()), retained);
    assert!(session.export_pdf().unwrap().is_none());

    let optional = b"\\openin1=optional.tex \\ifeof1 Absent\\else Present\\fi\\closein1\\end\n";
    session
        .open_document(
            "document",
            3,
            Path::new("/project/document.tex"),
            optional.as_slice().into(),
        )
        .unwrap();
    loop {
        match session.advance_interactive(8).unwrap() {
            SessionProgress::NeedResource(_) => panic!("Optional absence must not request bytes"),
            SessionProgress::Running(RunProgress::Yielded) => {}
            SessionProgress::Running(progress) => {
                assert_eq!(progress, RunProgress::Complete);
                break;
            }
        }
    }
    compare_fresh(&mut session, optional);
    assert!(
        session
            .resolve_resource("document", 2, denied.request_id, None)
            .is_err()
    );
    session
        .open_document(
            "document",
            4,
            Path::new("/project/document.tex"),
            missing.as_slice().into(),
        )
        .unwrap();
    let cancelled = loop {
        match session.advance_interactive(8).unwrap() {
            SessionProgress::NeedResource(request) => break request,
            SessionProgress::Running(progress) => assert_eq!(progress, RunProgress::Yielded),
        }
    };
    session.cancel("document", 4).unwrap();
    assert_eq!(session.host().pending_bytes(), 0);
    assert!(session.needed_resource().is_none());
    assert!(
        session
            .resolve_resource("document", 4, cancelled.request_id, None)
            .is_err()
    );
    session
        .open_document(
            "document",
            5,
            Path::new("/project/document.tex"),
            optional.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, optional);
}

#[test]
fn long_math_rendering_yields_after_token_parsing_and_cancels_cleanly() {
    use retex_rs2::session::EditingSession;
    let mut source = String::from("\\pdfinfoomitdate=1 \\pdftrailerid{}\n");
    for (family, (selector, name)) in [
        ("A", "cmr10"),
        ("B", "cmmi10"),
        ("C", "cmsy10"),
        ("D", "cmex10"),
    ]
    .iter()
    .enumerate()
    {
        source += &format!("\\font\\font{selector}={name} ");
        for prefix in ["text", "script", "scriptscript"] {
            source += &format!("\\{prefix}font{family}=\\font{selector} ");
        }
    }
    source += "\n\\immediate\\openout1=cancel.aux \\immediate\\write1{Before render}\n$";
    source += &"x+x+".repeat(500);
    source += "x$\\par\\end\n";
    let mut session = EditingSession::new(config(), host(source.as_bytes())).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    let mut reached = false;
    for _ in 0..100_000 {
        assert_eq!(session.advance(8).unwrap(), RunProgress::Yielded);
        let engine = session.engine().unwrap();
        if engine.math.depth == 0
            && engine
                .env
                .groups
                .last()
                .is_some_and(|g| g.kind == "math_shift")
            && engine.builder.nodes.len() > 50
        {
            let tokens = engine.core.input.token_count;
            let allocated = engine.builder.node_ptr;
            assert_eq!(session.advance(8).unwrap(), RunProgress::Yielded);
            let engine = session.engine().unwrap();
            assert_eq!(
                engine.core.input.token_count, tokens,
                "Did not reach rendering"
            );
            assert!(
                engine.builder.node_ptr > allocated,
                "Rendering made no progress"
            );
            reached = true;
            break;
        }
    }
    assert!(reached, "Did not yield during math rendering");
    assert!(session.host().pending_bytes() > 0);
    session.cancel("document", 1).unwrap();
    assert_eq!(session.host().pending_bytes(), 0);
    let repaired = b"\\pdfinfoomitdate=1 \\pdftrailerid{} Recovered\\par\\end\n";
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            repaired.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, repaired);
}

#[test]
fn shifted_boxes_leaders_and_message_expansion_yield_and_recover() {
    use retex_rs2::session::EditingSession;
    for body in [
        r"X\raise\loop\hbox{X}",
        r"\moveleft\loop\hbox{X}",
        r"\leaders\hbox{\loop}\hskip1pt",
        r"X\leaders\hbox{X}\hskip\loop",
        r"\message{\loop}",
        r"\errmessage{\loop}",
    ] {
        let source = format!(
            "\\pdfinfoomitdate=1 \\pdftrailerid{{}}\n\\def\\loop{{\\loop}}\n\\immediate\\openout1=cancel.aux \\immediate\\write1{{Pending}}\n{body}\n"
        );
        let mut session = EditingSession::new(config(), host(source.as_bytes())).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        for _ in 0..100 {
            assert_eq!(session.advance(8).unwrap(), RunProgress::Yielded, "{body}");
        }
        assert!(session.host().pending_bytes() > 0, "{body}");
        session.cancel("document", 1).unwrap();
        assert_eq!(session.host().pending_bytes(), 0);
        let repaired = b"\\pdfinfoomitdate=1 \\pdftrailerid{} Recovered\\par\\end\n";
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                repaired.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        compare_fresh(&mut session, repaired);
    }
}

#[test]
fn alignment_cells_preambles_hooks_and_noalign_yield_and_recover() {
    use retex_rs2::session::EditingSession;
    for body in [
        r"\halign{#\cr\loop\cr}",
        r"\halign{\tabskip=\loop#\cr X\cr}",
        r"\halign{#\cr X\cr\noalign{\loop}}",
        r"\halign{\loop#\cr X\cr}",
        r"\halign{#\loop\cr X\cr}",
        r"\everycr={\loop}\halign{#\cr X\cr}",
        r"\halign{#&#\cr X\span\loop\cr}",
        r"\valign{#\cr\loop\cr}",
        r"$$\halign{#\cr\loop\cr}$$",
    ] {
        let source = format!(
            "\\pdfinfoomitdate=1 \\pdftrailerid{{}}\n\\def\\loop{{\\loop}}\n\\immediate\\openout1=cancel.aux \\immediate\\write1{{Pending}}\n{body}\n"
        );
        let mut session = EditingSession::new(config(), host(source.as_bytes())).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        for _ in 0..100 {
            assert_eq!(session.advance(8).unwrap(), RunProgress::Yielded, "{body}");
        }
        assert!(
            session
                .engine()
                .unwrap()
                .env
                .groups
                .iter()
                .any(|group| group.kind == "alignment"),
            "Did not enter alignment: {body}"
        );
        assert!(session.host().pending_bytes() > 0, "{body}");
        session.cancel("document", 1).unwrap();
        assert_eq!(session.host().pending_bytes(), 0);
        let repaired = b"\\pdfinfoomitdate=1 \\pdftrailerid{} Recovered\\par\\end\n";
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                repaired.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        compare_fresh(&mut session, repaired);
    }
}

#[test]
fn output_routines_yield_cancel_and_recover_at_page_boundaries() {
    use retex_rs2::session::EditingSession;
    for body in [
        r"\output={\loop}\vsize=1pt X\par\end",
        r"\output={\loop}\vsize=1pt X\par",
        r"\output={\loop}\vsize=1pt\hbox{X}\penalty-10000",
        r"\output={\loop}\vsize=1pt X\par Y",
        r"\output={\hbox{\loop}}\vsize=1pt X\par\end",
        r"\output={\halign{#\cr\loop\cr}}\vsize=1pt X\par\end",
        r"\output={\message{\loop}}\vsize=1pt X\par\end",
        r"\output={$\loop$}\vsize=1pt X\par\end",
        r"\output={\global\setbox0=\hbox{\loop}}\vsize=1pt X\par\end",
        r"\output={\loop}\vsize=1pt X\par\hbox{unfinished",
        r"\output={\loop}\vsize=1pt\hsize=10pt X X X X X X X X X X\vskip1pt",
        r"\output={\loop}\vsize=1pt\halign{#\cr X\cr Y\cr}",
        r"\output={\loop}\vsize=1pt $$\halign{#\cr\hbox{X}\cr\hbox{Y}\cr}$$",
        r"\output={\loop}\vsize=1pt\hbox{X}\moveleft0pt\hbox{X}",
        r#"\font\mi=cmmi10 \font\sy=cmsy10 \font\ex=cmex10 \textfont1=\mi \scriptfont1=\mi \scriptscriptfont1=\mi \textfont2=\sy \scriptfont2=\sy \scriptscriptfont2=\sy \textfont3=\ex \scriptfont3=\ex \scriptscriptfont3=\ex \mathcode120="0178 \output={\loop}\vsize=1pt $$x$$"#,
    ] {
        let source = format!(
            "\\pdfinfoomitdate=1 \\pdftrailerid{{}}\n\\def\\loop{{\\loop}}\n\\immediate\\openout1=cancel.aux \\immediate\\write1{{Pending}}\n{body}\n"
        );
        let mut session = EditingSession::new(config(), host(source.as_bytes())).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        for _ in 0..100 {
            assert_eq!(session.advance(8).unwrap(), RunProgress::Yielded, "{body}");
        }
        assert!(
            session.engine().unwrap().page_builder.active,
            "Did not reach output routine: {body}"
        );
        assert!(session.host().pending_bytes() > 0, "{body}");
        session.cancel("document", 1).unwrap();
        assert_eq!(session.host().pending_bytes(), 0);
        let repaired = b"\\pdfinfoomitdate=1 \\pdftrailerid{} Recovered\\par\\end\n";
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                repaired.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        compare_fresh(&mut session, repaired);
    }
}

#[test]
fn scalar_and_rule_operands_yield_cancel_and_recover() {
    use retex_rs2::session::EditingSession;
    for body in [
        r"\char\loop",
        r"\kern\loop",
        r"\penalty\loop",
        r"\hskip\loop",
        r"\hrule width\loop",
        r"\leaders\hrule width\loop\vskip1pt",
        r"X\vrule height\loop",
        r"\hrule height1pt width\loop",
    ] {
        let source = format!(
            "\\pdfinfoomitdate=1 \\pdftrailerid{{}}\n\\def\\loop{{\\loop}}\n\\immediate\\openout1=cancel.aux \\immediate\\write1{{Pending}}\n{body}\n"
        );
        let mut session = EditingSession::new(config(), host(source.as_bytes())).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        for _ in 0..100 {
            assert_eq!(session.advance(8).unwrap(), RunProgress::Yielded, "{body}");
        }
        assert!(
            session.engine().unwrap().core.input.token_count > 100,
            "Did not reach recursive operand: {body}"
        );
        assert!(session.host().pending_bytes() > 0, "{body}");
        session.cancel("document", 1).unwrap();
        assert_eq!(session.host().pending_bytes(), 0);
        let repaired = b"\\pdfinfoomitdate=1 \\pdftrailerid{} Recovered\\par\\end\n";
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                repaired.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        compare_fresh(&mut session, repaired);
    }
}

#[test]
fn discretionary_accent_and_unpack_yield_cancel_and_recover() {
    use retex_rs2::session::EditingSession;
    for body in [
        r"\discretionary{\loop}{}{}",
        r"\discretionary{A}{\loop}{}",
        r"\discretionary{A}{B}{\loop}",
        r"\accent\loop",
        r"\accent65\count0=\loop X",
        r"\accent65\char\loop",
        r"\unhcopy\loop",
        r"\setbox0=\vbox{\hbox{X}}\output={\loop}\vsize=1pt\hsize=10pt X X X X X X X X X X\unvcopy0",
    ] {
        let source = format!(
            "\\pdfinfoomitdate=1 \\pdftrailerid{{}}\n\\def\\loop{{\\loop}}\n\\immediate\\openout1=cancel.aux \\immediate\\write1{{Pending}}\n{body}\n"
        );
        let mut session = EditingSession::new(config(), host(source.as_bytes())).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        for _ in 0..100 {
            assert_eq!(session.advance(8).unwrap(), RunProgress::Yielded, "{body}");
        }
        assert!(
            session.engine().unwrap().core.input.token_count > 100,
            "Did not reach recursive operand: {body}"
        );
        if body.starts_with(r"\discretionary") {
            assert!(
                session
                    .engine()
                    .unwrap()
                    .env
                    .groups
                    .iter()
                    .any(|group| group.kind == "box"),
                "Did not enter discretionary alternative: {body}"
            );
            assert_eq!(
                session.engine().unwrap().env.cur_group,
                retex_rs2::core::enums::GroupKind::Disc
            );
        }
        if body.contains(r"\count0=") {
            assert!(
                !session.engine().unwrap().core.setbox_allowed,
                "Accent assignment guard not active: {body}"
            );
        }
        if body.contains(r"\output=") {
            assert!(
                session.engine().unwrap().page_builder.active,
                "Did not reach unpack output callback: {body}"
            );
        }
        assert!(session.host().pending_bytes() > 0, "{body}");
        session.cancel("document", 1).unwrap();
        assert_eq!(session.host().pending_bytes(), 0);
        let repaired = b"\\pdfinfoomitdate=1 \\pdftrailerid{} Recovered\\par\\end\n";
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                repaired.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        compare_fresh(&mut session, repaired);
    }
}

#[test]
fn font_and_filename_scanning_yield_cancel_private_effects_and_recover() {
    use retex_rs2::{
        language::records::{CommandOperand, MeaningCommand},
        session::EditingSession,
    };
    for body in [
        r"\input\loop",
        r"\input{\loop}",
        r"\font\r=\loop",
        r"\font\r=cmr10 at\loop",
        r"\font\r=cmr10 scaled\loop",
        r"\pdfcopyfont\r=\loop",
        r"\letterspacefont\r=\loop",
        r"\font\r=cmr10 \letterspacefont\s=\r\loop",
        r"\hyphenchar\loop",
        r"\font\r=cmr10 \hyphenchar\r=\loop",
        r"\font\r=cmr10 \pdfincludechars\r{\loop}",
        r"\font\r=cmr10 \lpcode\r65=\loop",
    ] {
        let source = format!(
            "\\pdfinfoomitdate=1 \\pdftrailerid{{}}\n\\def\\loop{{\\loop}}\n\\immediate\\openout1=cancel.aux \\immediate\\write1{{Pending}}\n{body}\n"
        );
        let mut session = EditingSession::new(config(), host(source.as_bytes())).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        for _ in 0..100 {
            assert_eq!(session.advance(8).unwrap(), RunProgress::Yielded, "{body}");
        }
        assert!(
            session.engine().unwrap().core.input.token_count > 100,
            "Did not reach recursive operand: {body}"
        );
        if matches!(body, r"\input\loop" | r"\input{\loop}" | r"\font\r=\loop") {
            assert!(
                session.engine().unwrap().core.input.scanning_filename,
                "Filename guard was not active: {body}"
            );
        }
        if matches!(
            body,
            r"\font\r=\loop"
                | r"\font\r=cmr10 at\loop"
                | r"\font\r=cmr10 scaled\loop"
                | r"\pdfcopyfont\r=\loop"
                | r"\letterspacefont\r=\loop"
        ) {
            let engine = session.engine().unwrap();
            let meaning = engine.env.meaning_key("control-symbol:r").unwrap();
            assert_eq!(
                meaning.command,
                MeaningCommand::Operation(retex_rs2::core::enums::Command::SetFont)
            );
            assert!(
                matches!(meaning.value, CommandOperand::Number(0)),
                "Destination was not bound to nullfont: {body}"
            );
        }
        assert!(session.host().pending_bytes() > 0, "{body}");
        session.cancel("document", 1).unwrap();
        assert_eq!(session.host().pending_bytes(), 0);
        let repaired=b"\\pdfinfoomitdate=1 \\pdftrailerid{} \\font\\r=cmr10 at10pt \\r Recovered\\par\\input part.tex \\end\n";
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                repaired.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        assert!(!session.engine().unwrap().core.input.scanning_filename);
        compare_fresh(&mut session, repaired);
    }
}

#[test]
fn required_font_resources_retry_atomically_and_denial_preserves_failure() {
    use retex_rs2::session::{EditingSession, SessionProgress};
    let source = br"\pdfinfoomitdate=1 \pdftrailerid{} \immediate\openout1=notes.aux \immediate\write1{Once} \pdfmapline{+late CMR10 <late.enc <late.pfb} \font\late=late at10pt \late A\par \immediate\closeout1\end";
    let mut session = EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/assets");
    let mut glyphs = vec!["/.notdef"; 256];
    glyphs[65] = "/A";
    let encoding = format!("/LateEncoding [ {} ] def\n", glyphs.join(" ")).into_bytes();
    let resources = [
        (
            "tfm",
            "late.tfm",
            std::fs::read(assets.join("cmr10.tfm")).unwrap(),
        ),
        ("enc files", "late.enc", encoding),
        (
            "type1 fonts",
            "late.pfb",
            std::fs::read(assets.join("cmr10.pfb")).unwrap(),
        ),
    ];
    let expected_resources = resources
        .iter()
        .map(|(_, name, bytes)| (*name, bytes.clone()))
        .collect::<Vec<_>>();
    for (kind, name, bytes) in resources {
        let request = loop {
            match session.advance_interactive(8).unwrap() {
                SessionProgress::NeedResource(request) => break request,
                SessionProgress::Running(progress) => {
                    assert_eq!(progress, RunProgress::Yielded, "{kind}")
                }
            }
        };
        assert_eq!(request.resource.kind, kind);
        assert_eq!(request.resource.name, name.as_bytes());
        assert_eq!(
            request.resource.directory.as_deref(),
            Some(b"/project".as_slice())
        );
        assert!(session.host().pending_bytes() > 0);
        assert!(session.export_pdf().is_err());
        assert!(session.commit_outputs("document", 1).is_err());
        assert!(
            session
                .resolve_resource("document", 2, request.request_id, None)
                .is_err()
        );
        let epoch = session.host().epoch();
        session
            .resolve_resource(
                "document",
                1,
                request.request_id,
                Some(CatalogDelta {
                    base_epoch: epoch,
                    new_epoch: epoch + 1,
                    policy_version: session.host().policy_version(),
                    changes: vec![put(&Path::new("/project").join(name), bytes)],
                }),
            )
            .unwrap();
        assert!(
            session
                .resolve_resource("document", 1, request.request_id, None)
                .is_err()
        );
    }
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh_resources(&mut session, source, &expected_resources);
    assert_eq!(
        session
            .host()
            .read(Path::new("/project/notes.aux"))
            .unwrap(),
        b"Once\n"
    );

    let denied_source = br"\font\missing=deniedfont at10pt \missing A\end";
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            denied_source.as_slice().into(),
        )
        .unwrap();
    let request = loop {
        match session.advance_interactive(8).unwrap() {
            SessionProgress::NeedResource(request) => break request,
            SessionProgress::Running(progress) => assert_eq!(progress, RunProgress::Yielded),
        }
    };
    let retained = observed(session.engine().unwrap());
    session
        .resolve_resource("document", 2, request.request_id, None)
        .unwrap();
    assert_eq!(observed(session.settled_engine().unwrap()), retained);
    assert!(session.export_pdf().unwrap().is_none());
    let cancelled_source = br"\immediate\openout1=cancel.aux \immediate\write1{Private}\font\missing=cancelledfont at10pt\end";
    session
        .open_document(
            "document",
            3,
            Path::new("/project/document.tex"),
            cancelled_source.as_slice().into(),
        )
        .unwrap();
    let cancelled_request = loop {
        match session.advance_interactive(8).unwrap() {
            SessionProgress::NeedResource(request) => break request,
            SessionProgress::Running(progress) => assert_eq!(progress, RunProgress::Yielded),
        }
    };
    assert!(session.host().pending_bytes() > 0);
    session.cancel("document", 3).unwrap();
    assert_eq!(session.host().pending_bytes(), 0);
    assert!(
        session
            .resolve_resource("document", 3, cancelled_request.request_id, None)
            .is_err()
    );
    let repaired = br"\pdfinfoomitdate=1 \pdftrailerid{} Repaired\par\end";
    session
        .open_document(
            "document",
            4,
            Path::new("/project/document.tex"),
            repaired.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, repaired);
}

#[test]
fn unicode_font_wait_supplies_original_bytes_and_matches_fresh_execution() {
    use retex_rs2::session::{EditingSession, SessionProgress};
    let source = "\\catcode123=1 \\catcode125=2 \\pdfoutput=1 \\pdfinfoomitdate=1 \\pdftrailerid{} \\font\\late=[late.ttf] at10pt \\late office Ω\\par\\end".as_bytes();
    let mut settings = config();
    settings.ini = true;
    settings.language_profile = retex_rs2::core::characters::LanguageProfile::Unicode;
    let mut session = EditingSession::new(settings.clone(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.into(),
        )
        .unwrap();
    let request = loop {
        match session.advance_interactive(8).unwrap() {
            SessionProgress::NeedResource(request) => break request,
            SessionProgress::Running(progress) => assert_eq!(progress, RunProgress::Yielded),
        }
    };
    assert_eq!(request.resource.kind, "unicode font");
    assert_eq!(request.resource.name, b"/project/late.ttf");
    let bytes = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/resources/DejaVuSans.ttf"),
    )
    .unwrap();
    session
        .resolve_resource(
            "document",
            1,
            request.request_id,
            Some(CatalogDelta {
                base_epoch: session.host().epoch(),
                new_epoch: session.host().epoch() + 1,
                policy_version: session.host().policy_version(),
                changes: vec![put(Path::new("/project/late.ttf"), bytes.clone())],
            }),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh_config(&mut session, source, &[("late.ttf", bytes)], settings);
}

#[test]
fn invalid_font_supply_reports_failure_and_recovers_without_request_loop() {
    use retex_rs2::session::{EditingSession, SessionProgress};
    let source = br"\font\bad=broken at10pt \bad A\end";
    let mut session = EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    let request = loop {
        match session.advance_interactive(8).unwrap() {
            SessionProgress::NeedResource(request) => break request,
            SessionProgress::Running(progress) => assert_eq!(progress, RunProgress::Yielded),
        }
    };
    let invalid = b"not a TFM".to_vec();
    session
        .resolve_resource(
            "document",
            1,
            request.request_id,
            Some(CatalogDelta {
                base_epoch: session.host().epoch(),
                new_epoch: session.host().epoch() + 1,
                policy_version: session.host().policy_version(),
                changes: vec![put(Path::new("/project/broken.tfm"), invalid.clone())],
            }),
        )
        .unwrap();
    loop {
        match session.advance_interactive(8).unwrap() {
            SessionProgress::NeedResource(_) => {
                panic!("Invalid font bytes must fail rather than repeat a missing-resource request")
            }
            SessionProgress::Running(RunProgress::Yielded) => {}
            SessionProgress::Running(progress) => {
                assert_eq!(progress, RunProgress::Failed);
                break;
            }
        }
    }
    assert!(
        session
            .engine()
            .unwrap()
            .core
            .diagnostics
            .errors
            .iter()
            .any(|message| message.contains("invalid TFM"))
    );
    compare_fresh_resources(&mut session, source, &[("broken.tfm", invalid)]);
    let repaired = br"\pdfinfoomitdate=1 \pdftrailerid{} Repaired\par\end";
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            repaired.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, repaired);
}

#[test]
fn initialized_font_lookup_requests_a_directory_used_by_reference_search() {
    use retex_rs2::session::{EditingSession, SessionProgress};
    let source = br"\catcode123=1 \catcode125=2 \pdfoutput=1 \pdfinfoomitdate=1 \pdftrailerid{} \pdfmapline{+late CMR10 <late.enc <late.pfb} \font\late=late at10pt \late A\par\end";
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/assets");
    let mut settings = config();
    settings.ini = true;
    settings.font_dir = Some(assets.to_string_lossy().into_owned());
    let mut session = EditingSession::new(settings.clone(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    let mut glyphs = vec!["/.notdef"; 256];
    glyphs[65] = "/A";
    let resources = [
        (
            "tfm",
            "late.tfm",
            std::fs::read(assets.join("cmr10.tfm")).unwrap(),
        ),
        (
            "enc files",
            "late.enc",
            format!("/LateEncoding [ {} ] def\n", glyphs.join(" ")).into_bytes(),
        ),
        (
            "type1 fonts",
            "late.pfb",
            std::fs::read(assets.join("cmr10.pfb")).unwrap(),
        ),
    ];
    let mut expected = Vec::new();
    for (kind, name, bytes) in resources {
        let request = loop {
            match session.advance_interactive(8).unwrap() {
                SessionProgress::NeedResource(request) => break request,
                SessionProgress::Running(progress) => assert_eq!(progress, RunProgress::Yielded),
            }
        };
        assert_eq!(request.resource.kind, kind);
        assert_eq!(request.resource.name, name.as_bytes());
        assert_eq!(request.resource.directory, Some(resource_path(&assets).0));
        let path = assets.join(name);
        expected.push((path.to_string_lossy().into_owned(), bytes.clone()));
        session
            .resolve_resource(
                "document",
                1,
                request.request_id,
                Some(CatalogDelta {
                    base_epoch: session.host().epoch(),
                    new_epoch: session.host().epoch() + 1,
                    policy_version: session.host().policy_version(),
                    changes: vec![put(&path, bytes)],
                }),
            )
            .unwrap();
    }
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let expected = expected
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.clone()))
        .collect::<Vec<_>>();
    compare_fresh_config(&mut session, source, &expected, settings);
}

#[test]
fn internal_quantities_and_the_yield_discard_effects_and_recover() {
    use retex_rs2::session::EditingSession;
    for body in [
        r"\edef\saved{\the\loop}",
        r"\count0=\hyphenchar\loop",
        r"\edef\saved{\the\hyphenchar\loop}",
        r"\count0=\lpcode\loop65",
        r"\edef\saved{\the\lpcode\loop65}",
        r"\font\r=cmr10 \count0=\lpcode\r\loop",
        r"\kern\fontdimen\loop\font",
        r"\edef\saved{\the\fontdimen7\loop}",
        r"\kern\wd\loop",
        r"\edef\saved{\the\wd\loop}",
        r"\edef\saved{\the\textfont\loop}",
        r"\count0=\interlinepenalties\loop",
        r"\kern\gluestretch\loop",
        r"\kern\fontcharwd\loop65",
        r"\kern\fontcharwd\font\loop",
        r"\kern\parshapeindent\loop",
        r"\count0=\numexpr1+\loop",
        r"\count0=\numexpr1 \loop",
        r"\count0=\numexpr(1+(2+\loop))",
        r"\edef\saved{\unexpanded\loop}",
    ] {
        let source = format!(
            "\\pdfinfoomitdate=1 \\pdftrailerid{{}}\n\\def\\loop{{\\loop}}\n\\immediate\\openout1=cancel.aux \\immediate\\write1{{Pending}}\n{body}\n"
        );
        let mut session = EditingSession::new(config(), host(source.as_bytes())).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        for _ in 0..100 {
            assert_eq!(session.advance(8).unwrap(), RunProgress::Yielded, "{body}");
        }
        assert!(
            session.engine().unwrap().core.input.token_count > 100,
            "Did not reach recursive operand: {body}"
        );
        assert!(session.host().pending_bytes() > 0, "{body}");
        session.cancel("document", 1).unwrap();
        assert_eq!(session.host().pending_bytes(), 0);
        let repaired = br"\pdfinfoomitdate=1 \pdftrailerid{} \font\r=cmr10 \count0=\numexpr(7*3/2)+4\relax \dimen0=\fontcharwd\r65 \r Recovered \the\count0: \the\dimen0\par\input part.tex \end";
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                repaired.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        compare_fresh(&mut session, repaired);
    }
}

#[test]
fn stream_special_mark_operands_yield_cancel_effects_and_recover() {
    use retex_rs2::session::EditingSession;
    for body in [
        r"\immediate\loop",
        r"\immediate\openout1=notes.aux \immediate\write1{\loop}",
        r"\openin1=\loop",
        r"\openout1=\loop",
        r"\openin\loop",
        r"\write\loop",
        r"\read\loop to\saved",
        r"\read1 \loop",
        r"\special{\loop}",
        r"\pdfliteral\loop",
        r"\pdfmapline{\loop}",
        r"\mark{\loop}",
        r"\marks\loop{X}",
    ] {
        let source = format!(
            "\\pdfinfoomitdate=1 \\pdftrailerid{{}}\n\\def\\loop{{\\loop}}\n\\immediate\\openout1=cancel.aux \\immediate\\write1{{Pending}}\n{body}\n"
        );
        let mut session = EditingSession::new(config(), host(source.as_bytes())).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        for _ in 0..100 {
            assert_eq!(session.advance(8).unwrap(), RunProgress::Yielded, "{body}");
        }
        assert!(
            session.engine().unwrap().core.input.token_count > 100,
            "Did not reach recursive operand: {body}"
        );
        if body.contains(r"\immediate\write1{") {
            assert_eq!(
                session.engine().unwrap().builder.context.mode,
                retex_rs2::core::enums::ListMode::Disabled
            );
        }
        assert!(session.host().pending_bytes() > 0, "{body}");
        session.cancel("document", 1).unwrap();
        assert_eq!(session.host().pending_bytes(), 0);
        let repaired = br"\pdfinfoomitdate=1 \pdftrailerid{}\immediate\openout1=notes.aux \immediate\write1{Recovered}\immediate\closeout1\openin1=notes.aux \read1 to\saved \closein1\saved\par\end";
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                repaired.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        compare_fresh(&mut session, repaired);
    }
}

#[test]
fn missing_input_directory_follows_native_and_source_relative_lookup() {
    use retex_rs2::session::{EditingSession, SessionProgress};
    let source=br"\catcode123=1 \catcode125=2 \pdfoutput=1 \pdfinfoomitdate=1 \pdftrailerid{} \font\r=cmr10 \r \immediate\openout1=notes.aux \immediate\write1{Before}\input nested/parent.tex \immediate\write1{After}\immediate\closeout1\end";
    for native in [false, true] {
        let mut settings = config();
        settings.ini = native;
        let parent = put(
            Path::new("/project/nested/parent.tex"),
            br"\input child.tex".to_vec(),
        );
        let catalog = host(source);
        let epoch = catalog.epoch();
        catalog
            .apply(CatalogDelta {
                base_epoch: epoch,
                new_epoch: epoch + 1,
                policy_version: catalog.policy_version(),
                changes: vec![parent.clone()],
            })
            .unwrap();
        let mut session = EditingSession::new(settings.clone(), catalog).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_slice().into(),
            )
            .unwrap();
        let mut request = None;
        for _ in 0..2000 {
            match session.advance_interactive(8).unwrap() {
                SessionProgress::NeedResource(value) => {
                    request = Some(value);
                    break;
                }
                SessionProgress::Running(progress) => assert_eq!(progress, RunProgress::Yielded),
            }
        }
        let request = request.expect("Nested input did not request its missing resource");
        assert_eq!(request.resource.kind, "tex");
        assert_eq!(request.resource.name, b"child.tex");
        assert_eq!(
            request.resource.directory.as_deref(),
            if native {
                None
            } else {
                Some(b"/project/nested".as_slice())
            }
        );
        let directory = if native {
            "/project"
        } else {
            "/project/nested"
        };
        let child = put(
            &Path::new(directory).join("child.tex"),
            b"Supplied\\par\n".to_vec(),
        );
        let epoch = session.host().epoch();
        session
            .resolve_resource(
                "document",
                1,
                request.request_id,
                Some(CatalogDelta {
                    base_epoch: epoch,
                    new_epoch: epoch + 1,
                    policy_version: session.host().policy_version(),
                    changes: vec![child.clone()],
                }),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        assert_eq!(
            session
                .host()
                .read(Path::new("/project/notes.aux"))
                .unwrap(),
            b"Before\nAfter\n"
        );
        compare_fresh_config(
            &mut session,
            source,
            &[
                ("nested/parent.tex", br"\input child.tex".to_vec()),
                (
                    if native {
                        "child.tex"
                    } else {
                        "nested/child.tex"
                    },
                    b"Supplied\\par\n".to_vec(),
                ),
            ],
            settings,
        );
    }
}

#[test]
fn shipment_effects_yield_cancel_private_files_and_recover() {
    use retex_rs2::session::EditingSession;
    for body in [
        r"\openout1=notes.aux \write1{\loop}X\par\end",
        r"\shipout\hbox{X\write1{\loop}}",
        r"\output={\shipout\box255}\vsize=1pt X\write1{\loop}\par\end",
        r"\special shipout{\loop}X\par\end",
        r"\shipout\hbox{X\special shipout{\loop}}",
        r"X\leaders\hbox to10pt{X\special shipout{\loop}}\hskip50pt Y\par\end",
    ] {
        let source = format!(
            "\\pdfinfoomitdate=1 \\pdftrailerid{{}}\n\\def\\loop{{\\loop}}\n\\immediate\\openout1=cancel.aux \\immediate\\write1{{Pending}}\n{body}\n"
        );
        let mut session = EditingSession::new(config(), host(source.as_bytes())).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        for _ in 0..100 {
            assert_eq!(session.advance(8).unwrap(), RunProgress::Yielded, "{body}");
        }
        assert!(
            session.engine().unwrap().core.input.token_count > 100,
            "Did not reach recursive operand: {body}"
        );
        {
            assert_eq!(
                session.engine().unwrap().builder.context.mode,
                retex_rs2::core::enums::ListMode::Disabled
            );
        }
        assert!(session.host().pending_bytes() > 0, "{body}");
        session.cancel("document", 1).unwrap();
        assert_eq!(session.host().pending_bytes(), 0);
        let repaired = br"\pdfinfoomitdate=1 \pdftrailerid{}\immediate\openout1=notes.aux \immediate\write1{Recovered}\immediate\closeout1\openin1=notes.aux \read1 to\saved \closein1\saved\par\end";
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                repaired.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        compare_fresh(&mut session, repaired);
    }
}

#[test]
fn form_capture_operands_and_nested_resources_yield_cancel_and_recover() {
    use retex_rs2::session::EditingSession;
    for body in [
        r"\setbox0=\hbox{X\write1{\loop}}\immediate\pdfxform0",
        r"\setbox0=\hbox{X\write1{\loop}}\pdfxform0 X\pdfrefxform\pdflastxform\par\end",
        r"\setbox0=\hbox{X\special shipout{\loop}}\immediate\pdfxform0",
        r"\setbox0=\hbox{X\special shipout{\loop}}\pdfxform0 X\pdfrefxform\pdflastxform\par\end",
        r"\setbox0=\hbox{X\write1{\loop}}\pdfxform0\edef\first{\the\pdflastxform}\setbox0=\hbox{\pdfrefxform\first}\pdfxform0 X\pdfrefxform\pdflastxform\par\end",
        r"\setbox0=\hbox{X}\pdfxform attr{\loop}0",
        r"\setbox0=\hbox{X}\pdfxform resources{\loop}0",
        r"\setbox0=\hbox{X}\pdfxform resources{}\loop",
        r"\pdfrefxform\loop",
    ] {
        let source = format!(
            "\\pdfinfoomitdate=1 \\pdftrailerid{{}}\n\\def\\loop{{\\loop}}\n\\immediate\\openout1=cancel.aux \\immediate\\write1{{Pending}}\n{body}\n"
        );
        let mut session = EditingSession::new(config(), host(source.as_bytes())).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        for _ in 0..100 {
            assert_eq!(session.advance(8).unwrap(), RunProgress::Yielded, "{body}");
        }
        assert!(
            session.engine().unwrap().core.input.token_count > 100,
            "Did not reach recursive operand: {body}"
        );
        if body.contains(r"\write1{") || body.contains(r"\special shipout{") {
            assert_eq!(
                session.engine().unwrap().builder.context.mode,
                retex_rs2::core::enums::ListMode::Disabled
            );
        }
        assert!(session.host().pending_bytes() > 0, "{body}");
        session.cancel("document", 1).unwrap();
        assert_eq!(session.host().pending_bytes(), 0);
        let repaired = br"\pdfinfoomitdate=1 \pdftrailerid{}\immediate\openout1=notes.aux \immediate\write1{Recovered}\immediate\closeout1\openin1=notes.aux \read1 to\saved \closein1\saved\par\end";
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                repaired.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        compare_fresh(&mut session, repaired);
    }
}

#[test]
fn pdf_object_paint_metadata_and_font_operands_yield_cancel_and_recover() {
    use retex_rs2::session::EditingSession;
    for body in [
        r"\pdfobj{\loop}",
        r"\immediate\pdfobj{\loop}",
        r"\pdfobj stream attr{\loop}{X}",
        r"\immediate\pdfobj stream file{\loop}",
        r"\pdfobj useobjnum\loop",
        r"\pdfobj\loop",
        r"\pdfobj reserveobjnum\loop",
        r"\pdfrefobj\loop",
        r"\pdfsetmatrix{\loop}",
        r"\pdfcolorstack\loop",
        r"\pdfcolorstack0\loop",
        r"\pdfcolorstack0 push{\loop}",
        r"\pdfinfo{\loop}",
        r"\pdfnames{\loop}",
        r"\pdftrailer{\loop}",
        r"\pdftrailerid{\loop}",
        r"\pdfspacefont{\loop}",
        r"\pdffontexpand\loop",
        r"\pdffontexpand\font\loop",
        r"\pdffontexpand\font20 10 5 autoexpand\loop",
        r"\pdfsnapy\loop",
        r"\pdfsnapycomp\loop",
    ] {
        let source = format!(
            "\\pdfinfoomitdate=1 \\pdftrailerid{{}}\n\\def\\loop{{\\loop}}\n\\immediate\\openout1=cancel.aux \\immediate\\write1{{Pending}}\n{body}\n"
        );
        let mut session = EditingSession::new(config(), host(source.as_bytes())).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        for _ in 0..100 {
            assert_eq!(session.advance(8).unwrap(), RunProgress::Yielded, "{body}");
        }
        assert!(
            session.engine().unwrap().core.input.token_count > 100,
            "Did not reach recursive operand: {body}"
        );
        assert!(session.host().pending_bytes() > 0, "{body}");
        session.cancel("document", 1).unwrap();
        assert_eq!(session.host().pending_bytes(), 0);
        let repaired = br"\pdfinfoomitdate=1 \pdftrailerid{}\immediate\openout1=notes.aux \immediate\write1{Recovered}\immediate\closeout1\openin1=notes.aux \read1 to\saved \closein1\saved\par\end";
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                repaired.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        compare_fresh(&mut session, repaired);
    }
}

#[test]
fn navigation_and_image_operands_yield_cancel_and_recover() {
    use retex_rs2::session::EditingSession;
    for body in [
        r"\pdfannot{\loop}",
        r"\pdfannot width\loop",
        r"\pdfannot useobjnum\loop",
        r"\pdfannot reserveobjnum\loop",
        r"\pdfdest name{\loop}fit",
        r"\pdfdest num\loop",
        r"\pdfdest name{target}\loop",
        r"\pdfdest name{target}xyz zoom\loop",
        r"\pdfdest name{target}fitr width\loop",
        r"\pdfdest struct\loop",
        r"\hbox{\pdfstartlink goto name{\loop}X}",
        r"\hbox{\pdfstartlink attr{\loop}goto name{target}X}",
        r"\hbox{\pdfstartlink goto file{\loop}name{target}X}",
        r"\hbox{\pdfstartlink goto struct name{\loop}name{target}X}",
        r"\hbox{\pdfstartlink goto page\loop}",
        r"\hbox{\pdfstartlink goto page1{\loop}}",
        r"\hbox{\pdfstartlink goto file{other.pdf}name{target}newwindow\loop}",
        r"\pdfoutline goto name{target}{\loop}",
        r"\pdfoutline goto name{target}count\loop",
        r"\pdfthread attr{\loop}name{target}",
        r"\pdfstartthread name{\loop}",
        r"\pdfcatalog{\loop}",
        r"\pdfcatalog{}openaction user{\loop}",
        r"\pdfximage width\loop",
        r"\pdfximage attr{\loop}{image.png}",
        r"\pdfximage named{\loop}{image.pdf}",
        r"\pdfximage page\loop",
        r"\pdfximage colorspace\loop",
        r"\pdfximage{\loop}",
        r"\immediate\pdfximage{\loop}",
        r"\pdfrefximage\loop",
    ] {
        let source = format!(
            "\\pdfinfoomitdate=1 \\pdftrailerid{{}}\n\\def\\loop{{\\loop}}\n\\immediate\\openout1=cancel.aux \\immediate\\write1{{Pending}}\n{body}\n"
        );
        let mut session = EditingSession::new(config(), host(source.as_bytes())).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        for _ in 0..100 {
            assert_eq!(session.advance(8).unwrap(), RunProgress::Yielded, "{body}");
        }
        assert!(
            session.engine().unwrap().core.input.token_count > 100,
            "Did not reach recursive operand: {body}"
        );
        assert!(session.host().pending_bytes() > 0, "{body}");
        session.cancel("document", 1).unwrap();
        assert_eq!(session.host().pending_bytes(), 0);
        let repaired = br"\pdfinfoomitdate=1 \pdftrailerid{}\immediate\openout1=notes.aux \immediate\write1{Recovered}\immediate\closeout1\openin1=notes.aux \read1 to\saved \closein1\saved\par\end";
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                repaired.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        compare_fresh(&mut session, repaired);
    }
}

#[test]
fn required_image_requests_retry_transactionally_and_reject_late_replies() {
    use retex_rs2::session::{EditingSession, SessionProgress};
    let source = br"\pdfinfoomitdate=1 \pdftrailerid{}\immediate\openout1=notes.aux \immediate\write1{Once}\pdfximage width20pt{late.png}\shipout\hbox{\pdfrefximage\pdflastximage}\immediate\closeout1\end";
    let bytes = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/resources/badge.png"),
    )
    .unwrap();
    let mut session = EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    let request = loop {
        match session.advance_interactive(8).unwrap() {
            SessionProgress::NeedResource(request) => break request,
            SessionProgress::Running(progress) => assert_eq!(progress, RunProgress::Yielded),
        }
    };
    assert_eq!(request.resource.kind, "image");
    assert_eq!(request.resource.name, b"late.png");
    assert_eq!(
        request.resource.directory.as_deref(),
        Some(b"/project".as_slice())
    );
    assert!(session.host().pending_bytes() > 0);
    assert!(session.export_pdf().is_err());
    assert!(session.commit_outputs("document", 1).is_err());
    assert!(
        session
            .resolve_resource("document", 2, request.request_id, None)
            .is_err()
    );
    let epoch = session.host().epoch();
    session
        .resolve_resource(
            "document",
            1,
            request.request_id,
            Some(CatalogDelta {
                base_epoch: epoch,
                new_epoch: epoch + 1,
                policy_version: session.host().policy_version(),
                changes: vec![put(Path::new("/project/late.png"), bytes.clone())],
            }),
        )
        .unwrap();
    assert!(
        session
            .resolve_resource("document", 1, request.request_id, None)
            .is_err()
    );
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh_resources(&mut session, source, &[("late.png", bytes)]);
    assert_eq!(
        session
            .host()
            .read(Path::new("/project/notes.aux"))
            .unwrap(),
        b"Once\n"
    );
}

const SETTLING_COUNTER: &[u8] = br"\pdfinfoomitdate=1 \pdftrailerid{}
\count7=0 \openin1=notes.aux \ifeof1\else\read1 to\saved \count7=\saved\relax\fi\closein1
\ifnum\count7<2 \advance\count7 by1 \fi
\immediate\openout1=notes.aux \immediate\write1{\the\count7}\immediate\closeout1
\the\count7\par\end
";

#[test]
fn same_buffered_line_reuse_retains_executed_nested_input_state() {
    use retex_rs2::{
        language::records::{Quantity, QuantityKind, ValueKey},
        session::EditingSession,
    };
    let source=b"\\pdfinfoomitdate=1\\pdftrailerid{}\n\\count8=0\\input preamble.tex \\relax\nOriginal body. Count: \\the\\count7\\par\\end\n";
    let preamble = b"\\count7=39\\def\\loaded{Parsed}\\endinput\n";
    let mut session = EditingSession::new(config(), host(source)).unwrap();
    session
        .update_resources(CatalogDelta {
            base_epoch: session.host().epoch(),
            new_epoch: session.host().epoch() + 1,
            policy_version: session.host().policy_version(),
            changes: vec![put(Path::new("/project/preamble.tex"), preamble.to_vec())],
        })
        .unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let start = source
        .windows(8)
        .position(|word| word == b"Original")
        .unwrap();
    session
        .apply_edit("document", 1, start, start + 8, b"Changed!", 2)
        .unwrap();
    // Inspect immediately: an offset past the buffered input line alone does
    // not prove the included file's commands have already executed.
    assert!(
        session
            .engine()
            .unwrap()
            .core
            .input
            .source_resources
            .contains_key("/project/preamble.tex")
    );
    assert_eq!(
        session
            .engine()
            .unwrap()
            .env
            .values
            .get(&ValueKey::register(QuantityKind::Count, 7)),
        Some(&Quantity::Integer(39))
    );
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let mut changed = source.to_vec();
    changed.splice(start..start + 8, b"Changed!".iter().copied());
    compare_fresh_resources(
        &mut session,
        &changed,
        &[("preamble.tex", preamble.to_vec())],
    );
    let replaced = b"\\count7=57\\def\\loaded{Replaced}\\endinput\n";
    session
        .update_resources(CatalogDelta {
            base_epoch: session.host().epoch(),
            new_epoch: session.host().epoch() + 1,
            policy_version: session.host().policy_version(),
            changes: vec![ResourceChange::Put {
                id: ResourceId("/project/preamble.tex".into()),
                path: resource_path(Path::new("/project/preamble.tex")),
                version: 2,
                bytes: replaced.as_slice().into(),
            }],
        })
        .unwrap();
    session
        .open_document(
            "document",
            3,
            Path::new("/project/document.tex"),
            changed.as_slice().into(),
        )
        .unwrap();
    assert!(
        !session
            .engine()
            .unwrap()
            .core
            .input
            .source_resources
            .contains_key("/project/preamble.tex")
    );
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(
        session
            .engine()
            .unwrap()
            .env
            .values
            .get(&ValueKey::register(QuantityKind::Count, 7)),
        Some(&Quantity::Integer(57))
    );
    compare_fresh_resources(
        &mut session,
        &changed,
        &[("preamble.tex", replaced.to_vec())],
    );
}

#[test]
fn settling_edit_reuses_first_pass_preamble_and_its_private_effects() {
    use retex_rs2::{
        language::records::{Quantity, QuantityKind, ValueKey},
        session::EditingSession,
    };
    let source = SETTLING_COUNTER.to_vec();
    let start = source
        .windows(11)
        .rposition(|word| word == br"\the\count7")
        .unwrap();
    // Insert an unchanged ordinary paragraph before the edited body. This
    // permits a complete buffered-prefix checkpoint at the preamble boundary.
    let mut source = source;
    source.splice(
        start..start,
        b"First paragraph.\\par\nOriginal body. ".iter().copied(),
    );
    let mut session = EditingSession::new(config(), host(&source)).unwrap();
    let path = PathBuf::from("/project/notes.aux");
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    session
        .begin_settling("document", 1, std::slice::from_ref(&path), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.settling().unwrap().pass, 3);
    let start = source
        .windows(8)
        .position(|word| word == b"Original")
        .unwrap();
    session
        .apply_edit("document", 1, start, start + 8, b"Changed!", 2)
        .unwrap();
    // The retained prefix belongs to fresh pass one, not the previous stable
    // pass: its state and speculative file bytes must both still contain 1.
    assert_eq!(
        session
            .engine()
            .unwrap()
            .env
            .values
            .get(&ValueKey::register(QuantityKind::Count, 7)),
        Some(&Quantity::Integer(1))
    );
    assert_eq!(session.host().read(&path).unwrap(), b"1\n");
    assert!(session.host().committed(&path).unwrap().is_none());
    session
        .begin_settling("document", 2, std::slice::from_ref(&path), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.settling().unwrap().pass, 3);
    assert_eq!(session.host().read(&path).unwrap(), b"2\n");
    let mut changed = source;
    changed.splice(start..start + 8, b"Changed!".iter().copied());
    compare_fresh_resources(
        &mut session,
        &changed,
        &[("/project/notes.aux", b"2\n".to_vec())],
    );
}

#[test]
fn later_pass_preamble_reuse_requires_identical_private_input_bytes() {
    use retex_rs2::{
        language::records::{Quantity, QuantityKind, ValueKey},
        session::EditingSession,
    };
    let source = br"\pdfinfoomitdate=1\pdftrailerid{}
\count7=3 \openin1=notes.aux \ifeof1\else\read1 to\saved \count7=\saved\relax \advance\count7 by10 \fi\closein1
\immediate\openout1=notes.aux
First paragraph.\par
Original body. Count: \the\count7\par
\immediate\write1{7}\immediate\closeout1\end
";
    let mut session = EditingSession::new(config(), host(source)).unwrap();
    let path = PathBuf::from("/project/notes.aux");
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    session
        .begin_settling("document", 1, std::slice::from_ref(&path), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.settling().unwrap().pass, 2);
    let start = source
        .windows(8)
        .position(|word| word == b"Original")
        .unwrap();
    session
        .apply_edit("document", 1, start, start + 8, b"Changed!", 2)
        .unwrap();
    session
        .begin_settling("document", 2, std::slice::from_ref(&path), 4)
        .unwrap();
    for _ in 0..100_000 {
        assert_eq!(session.advance(1).unwrap(), RunProgress::Yielded);
        if session.settling().unwrap().pass == 2 {
            break;
        }
    }
    assert_eq!(session.settling().unwrap().pass, 2);
    // Inspect at pass entry, before further execution: identical input 7 lets
    // the already-executed preamble retain its value 17 and open output state.
    assert_eq!(
        session
            .engine()
            .unwrap()
            .env
            .values
            .get(&ValueKey::register(QuantityKind::Count, 7)),
        Some(&Quantity::Integer(17))
    );
    assert_eq!(session.host().read(&path).unwrap(), b"");
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let mut changed = source.to_vec();
    changed.splice(start..start + 8, b"Changed!".iter().copied());
    compare_fresh_resources(
        &mut session,
        &changed,
        &[("/project/notes.aux", b"7\n".to_vec())],
    );
    let literal = changed.windows(3).position(|word| word == b"{7}").unwrap() + 1;
    session
        .apply_edit("document", 2, literal, literal + 1, b"8", 3)
        .unwrap();
    session
        .begin_settling("document", 3, std::slice::from_ref(&path), 4)
        .unwrap();
    for _ in 0..100_000 {
        assert_eq!(session.advance(1).unwrap(), RunProgress::Yielded);
        if session.settling().unwrap().pass == 2 {
            break;
        }
    }
    assert_eq!(session.settling().unwrap().pass, 2);
    assert_ne!(
        session
            .engine()
            .unwrap()
            .env
            .values
            .get(&ValueKey::register(QuantityKind::Count, 7)),
        Some(&Quantity::Integer(17))
    );
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(
        session
            .engine()
            .unwrap()
            .env
            .values
            .get(&ValueKey::register(QuantityKind::Count, 7)),
        Some(&Quantity::Integer(18))
    );
    changed[literal] = b'8';
    compare_fresh_resources(
        &mut session,
        &changed,
        &[("/project/notes.aux", b"8\n".to_vec())],
    );
}

#[test]
fn resident_auxiliary_passes_settle_privately_and_publish_once() {
    use retex_rs2::session::EditingSession;
    let mut session = EditingSession::new(config(), host(SETTLING_COUNTER)).unwrap();
    let path = PathBuf::from("/project/notes.aux");
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            SETTLING_COUNTER.into(),
        )
        .unwrap();
    // Rejected configuration leaves the accepted execution and its journal intact.
    assert!(
        session
            .begin_settling("document", 0, std::slice::from_ref(&path), 4)
            .is_err()
    );
    assert!(
        session
            .begin_settling("document", 1, &[path.clone(), path.clone()], 4)
            .is_err()
    );
    session
        .begin_settling("document", 1, std::slice::from_ref(&path), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let observation = session.settling().unwrap();
    assert_eq!(
        (observation.pass, observation.status.as_str()),
        (3, "stable")
    );
    assert_eq!(observation.generations.len(), 3);
    assert_eq!(session.reuse().initialized_restarts, 1);
    assert_eq!(session.reuse().pass_checkpoint_reuses, 2);
    assert_eq!(
        observation.generations[0].changed_paths,
        vec![b"/project/notes.aux".to_vec()]
    );
    assert!(observation.generations[2].changed_paths.is_empty());
    assert!(session.host().committed(&path).unwrap().is_none());
    assert_eq!(session.host().read(&path).unwrap(), b"2\n");
    compare_fresh_resources(
        &mut session,
        SETTLING_COUNTER,
        &[("/project/notes.aux", b"2\n".to_vec())],
    );
    session.commit_outputs("document", 1).unwrap();
    assert_eq!(
        session.host().committed(&path).unwrap().unwrap().as_ref(),
        b"2\n"
    );
    assert!(session.commit_outputs("document", 1).is_err());
}

#[test]
fn settling_limits_and_cancelled_passes_do_not_contaminate_recovery() {
    use retex_rs2::session::EditingSession;
    let mut session = EditingSession::new(config(), host(SETTLING_COUNTER)).unwrap();
    let path = PathBuf::from("/project/notes.aux");
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            SETTLING_COUNTER.into(),
        )
        .unwrap();
    session
        .begin_settling("document", 1, std::slice::from_ref(&path), 1)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.settling().unwrap().status, "limit");
    assert_eq!(session.host().read(&path).unwrap(), b"1\n");
    session.cancel("document", 1).unwrap();
    assert!(session.host().committed(&path).unwrap().is_none());
    assert!(session.export_pdf().is_err());
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            SETTLING_COUNTER.into(),
        )
        .unwrap();
    session
        .begin_settling("document", 2, std::slice::from_ref(&path), 4)
        .unwrap();
    for _ in 0..100_000 {
        assert_eq!(session.advance(1).unwrap(), RunProgress::Yielded);
        if session.settling().unwrap().pass == 2 {
            break;
        }
    }
    assert_eq!(session.settling().unwrap().pass, 2);
    assert_eq!(session.host().read(&path).unwrap(), b"1\n");
    session.cancel("document", 2).unwrap();
    assert!(session.host().committed(&path).unwrap().is_none());
    session
        .open_document(
            "document",
            3,
            Path::new("/project/document.tex"),
            SETTLING_COUNTER.into(),
        )
        .unwrap();
    // A catalog-only prefix can survive cancellation; the auxiliary read must
    // still execute against absent bytes rather than an unaccepted earlier pass.
    let query_end = SETTLING_COUNTER
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .nth(1)
        .unwrap()
        .0
        + 1;
    assert!(session.reuse().restored_offset > 0 && session.reuse().restored_offset <= query_end);
    session
        .begin_settling("document", 3, std::slice::from_ref(&path), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.settling().unwrap().pass, 3);
    compare_fresh_resources(
        &mut session,
        SETTLING_COUNTER,
        &[("/project/notes.aux", b"2\n".to_vec())],
    );
}

#[test]
fn changing_auxiliary_generations_stop_at_the_requested_bound() {
    use retex_rs2::session::EditingSession;
    let source = String::from_utf8(SETTLING_COUNTER.to_vec())
        .unwrap()
        .replace(
            "\\ifnum\\count7<2 \\advance\\count7 by1 \\fi",
            "\\advance\\count7 by1",
        );
    let mut session = EditingSession::new(config(), host(source.as_bytes())).unwrap();
    let path = PathBuf::from("/project/notes.aux");
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    session.advance(1).unwrap();
    assert!(
        session
            .begin_settling("document", 1, std::slice::from_ref(&path), 4)
            .is_err()
    );
    session.cancel("document", 1).unwrap();
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    session
        .begin_settling("document", 2, std::slice::from_ref(&path), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let observation = session.settling().unwrap();
    assert_eq!(
        (observation.pass, observation.status.as_str()),
        (4, "limit")
    );
    assert!(
        observation
            .generations
            .iter()
            .all(|pass| pass.changed_paths == vec![b"/project/notes.aux".to_vec()])
    );
    assert_eq!(session.host().read(&path).unwrap(), b"4\n");
    assert!(session.host().committed(&path).unwrap().is_none());
    compare_fresh_resources(
        &mut session,
        source.as_bytes(),
        &[("/project/notes.aux", b"3\n".to_vec())],
    );
}

#[test]
fn settling_resource_supply_restarts_the_private_chain_with_new_resources() {
    use retex_rs2::session::{EditingSession, SessionProgress};
    let source = String::from_utf8(SETTLING_COUNTER.to_vec())
        .unwrap()
        .replace(
            "\\ifnum\\count7<2",
            "\\ifnum\\count7>0 \\input late.tex \\fi\n\\ifnum\\count7<2",
        );
    let mut session = EditingSession::new(config(), host(source.as_bytes())).unwrap();
    let path = PathBuf::from("/project/notes.aux");
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    session
        .begin_settling("document", 1, std::slice::from_ref(&path), 4)
        .unwrap();
    let request = loop {
        match session.advance_interactive(16).unwrap() {
            SessionProgress::NeedResource(request) => break request,
            SessionProgress::Running(progress) => assert_eq!(progress, RunProgress::Yielded),
        }
    };
    assert_eq!(session.settling().unwrap().pass, 2);
    assert_eq!(session.host().read(&path).unwrap(), b"1\n");
    let epoch = session.host().epoch();
    session
        .resolve_resource(
            "document",
            1,
            request.request_id,
            Some(CatalogDelta {
                base_epoch: epoch,
                new_epoch: epoch + 1,
                policy_version: 0,
                changes: vec![put(Path::new("/project/late.tex"), b"\\relax\n".to_vec())],
            }),
        )
        .unwrap();
    assert_eq!(session.settling().unwrap().pass, 1);
    assert!(session.host().committed(&path).unwrap().is_none());
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.settling().unwrap().pass, 3);
    compare_fresh_resources(
        &mut session,
        source.as_bytes(),
        &[
            ("/project/notes.aux", b"2\n".to_vec()),
            ("/project/late.tex", b"\\relax\n".to_vec()),
        ],
    );
}

#[test]
fn page_rollback_retains_executed_layout_and_invalidates_changed_state() {
    use retex_rs2::session::EditingSession;
    let mut source = String::from(
        "\\pdfinfoomitdate=1\\pdftrailerid{}\n\\font\\body=cmr10 \\body\n\\hsize=140pt\\vsize=100pt\n\\output={\\global\\advance\\count7 by1\\shipout\\box255}\n\\immediate\\openout1=notes.aux\n",
    );
    for index in 0..18 {
        source.push_str(&format!("Paragraph {index:02}. "));
        source.push_str(&"Exact paragraph and output state must survive an edit. ".repeat(3));
        source.push_str("\\par\n");
    }
    source.push_str("\\immediate\\write1{\\the\\count7}\\immediate\\closeout1\\end\n");
    let mut source = source.into_bytes();
    let mut session = EditingSession::new(
        EngineConfig {
            page_checkpoints: true,
            ..config()
        },
        host(&source),
    )
    .unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    let paths = [PathBuf::from("/project/notes.aux")];
    session.begin_settling("document", 1, &paths, 4).unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert!(session.engine().unwrap().layout.pages.len() > 4);
    assert!(session.reuse().page_checkpoint_captures > 0);
    assert!(session.reuse().page_boundaries.len() <= 8);
    let edit = source
        .windows(12)
        .position(|word| word == b"Paragraph 14")
        .unwrap();
    let reuses = session.reuse().page_checkpoint_reuses;
    session
        .apply_edit("document", 1, edit + 12, edit + 12, b"x", 2)
        .unwrap();
    source.insert(edit + 12, b'x');
    // Restoration includes actual page layout before any new engine step.
    assert!(session.reuse().restored_pages > 0);
    assert_eq!(
        session.engine().unwrap().layout.pages.len(),
        session.reuse().restored_pages
    );
    assert!(session.reuse().page_checkpoint_reuses > reuses);
    session.begin_settling("document", 2, &paths, 4).unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let auxiliary = session.host().read(&paths[0]).unwrap();
    compare_fresh_resources(&mut session, &source, &[("notes.aux", auxiliary)]);
    assert!(session.reuse().checkpoint_count <= 8);
    // A changed output routine cannot inherit pages executed by the old one.
    let output = source
        .windows(7)
        .position(|word| word == b"\\output")
        .unwrap();
    let change = output
        + source[output..]
            .windows(3)
            .position(|word| word == b"by1")
            .unwrap()
        + 2;
    session
        .apply_edit("document", 2, change, change + 1, b"2", 3)
        .unwrap();
    source[change] = b'2';
    assert_eq!(session.reuse().restored_pages, 0);
    session.begin_settling("document", 3, &paths, 4).unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let auxiliary = session.host().read(&paths[0]).unwrap();
    compare_fresh_resources(&mut session, &source, &[("notes.aux", auxiliary)]);
    // A resource used before the page boundary rejects that entire prefix.
    let font = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/assets/cmr10.tfm");
    let (id, version) = session
        .host()
        .resource_identity(&resource_path(&font))
        .unwrap()
        .unwrap();
    session
        .update_resources(CatalogDelta {
            base_epoch: session.host().epoch(),
            new_epoch: session.host().epoch() + 1,
            policy_version: session.host().policy_version(),
            changes: vec![ResourceChange::Put {
                path: resource_path(&font),
                id,
                version: version + 1,
                bytes: std::fs::read(&font).unwrap().into(),
            }],
        })
        .unwrap();
    session
        .open_document(
            "document",
            4,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(session.reuse().restored_pages, 0);
    session.begin_settling("document", 4, &paths, 4).unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let auxiliary = session.host().read(&paths[0]).unwrap();
    compare_fresh_resources(&mut session, &source, &[("notes.aux", auxiliary)]);
}

#[test]
fn progressive_time_observation_replays_from_fresh_execution() {
    use retex_rs2::session::EditingSession;
    let mut source = String::from(
        "\\pdfinfoomitdate=1\\pdftrailerid{}\n\\font\\body=cmr10 \\body\\hsize=140pt\\vsize=100pt\n\\output={\\shipout\\box255}\n\\immediate\\openout1=notes.aux\n",
    );
    for index in 0..16 {
        source.push_str(&format!(
            "Paragraph {index}. Exact continuation and clock dependencies remain observable.\\par\n"
        ));
    }
    source.push_str(
        "\\immediate\\write1{\\the\\pdfelapsedtime}\\immediate\\closeout1\nLater body.\\par\\end\n",
    );
    let mut settings = config();
    settings.progressive_pages = true;
    let catalog = host(source.as_bytes());
    catalog.set_monotonic_clock(std::rc::Rc::new(|| 1_000_000));
    let mut session = EditingSession::new(settings.clone(), catalog).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let restarts = session.reuse().initialized_restarts;
    let start = source.find("Later body.").unwrap();
    session
        .apply_edit("document", 1, start, start, b"Edited ", 2)
        .unwrap();
    assert!(session.reuse().restored_pages > 0);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.reuse().initialized_restarts, restarts + 1);
    let mut edited = source.into_bytes();
    edited.splice(start..start, b"Edited ".iter().copied());
    compare_fresh_config(&mut session, &edited, &[], settings);
}

#[test]
fn progressive_pages_are_isolated_and_ready_before_settlement() {
    use retex_rs2::{fonts::outlines::OutlineCache, session::EditingSession};
    let mut source = String::from(
        "\\pdfinfoomitdate=1\\pdftrailerid{}\n\\font\\body=cmr10 \\body\\hsize=140pt\\vsize=100pt\n\\output={\\shipout\\box255}\n\\immediate\\openout1=notes.aux\n",
    );
    for index in 0..30 {
        source.push_str(&format!("Paragraph {index:02}. "));
        source.push_str(&"Exact paragraph state must survive local editing. ".repeat(3));
        source.push_str("\\par\n");
    }
    source.push_str("\\immediate\\write1{Done}\\immediate\\closeout1\\end\n");
    let mut settings = config();
    settings.progressive_pages = true;
    let mut session = EditingSession::new(settings, host(source.as_bytes())).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    let mut outlines = OutlineCache::default();
    loop {
        assert_eq!(session.advance(128).unwrap(), RunProgress::Yielded);
        if session.engine().unwrap().completed_page_count() > 0 {
            break;
        }
    }
    assert_eq!(session.engine().unwrap().status(), "running");
    assert!(session.export_pdf().is_err());
    let values = session.engine().unwrap().env.values.clone();
    let resources = bincode::serialize(&session.engine().unwrap().core.backend.resources).unwrap();
    let pending = session
        .host()
        .staged_outputs(session.tag().unwrap())
        .unwrap();
    let page = session.capture_page(1, &mut outlines).unwrap();
    assert!(!page.complete);
    assert_eq!(page.pages.len(), 1);
    assert_eq!(page.pages[0].page, 1);
    assert_eq!(session.engine().unwrap().env.values, values);
    assert_eq!(
        bincode::serialize(&session.engine().unwrap().core.backend.resources).unwrap(),
        resources
    );
    assert_eq!(
        session
            .host()
            .staged_outputs(session.tag().unwrap())
            .unwrap(),
        pending
    );
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert!(session.engine().unwrap().completed_page_count() > 8);
    let coverage = session.page_checkpoint_coverage();
    assert_eq!(
        coverage.len(),
        session.engine().unwrap().completed_page_count() + 1
    );
    assert_eq!(coverage[0].page, 0);
    assert!(
        coverage.iter().all(|slot| slot.after_page == slot.page),
        "Missing per-page continuation: {coverage:?}"
    );
    assert!(session.reuse().checkpoint_count > 8);
    compare_fresh(&mut session, source.as_bytes());
    let needle = "Paragraph 16";
    let start = source.find(needle).unwrap();
    session
        .apply_edit(
            "document",
            1,
            start + needle.len(),
            start + needle.len(),
            b"x",
            2,
        )
        .unwrap();
    assert!(session.reuse().restored_pages > 0);
    let mut edited = source.into_bytes();
    edited.insert(start + needle.len(), b'x');
    assert_eq!(session.checkpoint_storage()["retired_engines"], 1);
    let pending = session
        .host()
        .staged_outputs(session.tag().unwrap())
        .unwrap();
    session.release_previous();
    assert_eq!(session.checkpoint_storage()["retired_engines"], 0);
    assert_eq!(
        session
            .host()
            .staged_outputs(session.tag().unwrap())
            .unwrap(),
        pending
    );
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, &edited);
}

#[test]
fn progressive_retirement_is_bounded_and_cannot_change_new_effects() {
    use retex_rs2::session::EditingSession;
    let mut source = String::from(
        "\\pdfinfoomitdate=1\\pdftrailerid{}\n\\count7=0\n\\immediate\\openout1=notes.txt\n",
    );
    for _ in 0..180 {
        source.push_str("\\global\\advance\\count7 by1\n");
    }
    source.push_str(
        "Body \\the\\count7\\par\n\\immediate\\write1{\\the\\count7}\\immediate\\closeout1\\end\n",
    );
    let mut settings = config();
    settings.progressive_pages = true;
    let mut session = EditingSession::new(settings, host(source.as_bytes())).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    session
        .begin_settling("document", 1, &[PathBuf::from("/project/notes.txt")], 3)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let start = source.find("Body").unwrap() + 4;
    session
        .apply_edit("document", 1, start, start, b"x", 2)
        .unwrap();
    let states = session.checkpoint_storage()["retired_checkpoint_states"]
        .as_u64()
        .unwrap();
    assert!(states > 0 && states <= 128);
    let effects = session
        .host()
        .staged_outputs(session.tag().unwrap())
        .unwrap();
    assert!(session.host().staged_outputs(&tag(1)).is_err());
    while session.reclaim_retired(1) != 0 {
        assert_eq!(
            session
                .host()
                .staged_outputs(session.tag().unwrap())
                .unwrap(),
            effects
        );
    }
    assert_eq!(session.checkpoint_storage()["retired_checkpoint_states"], 0);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let edited = source.replacen("Body", "Bodyx", 1);
    compare_fresh(&mut session, edited.as_bytes());
}

#[test]
fn body_checkpoints_restore_near_edit_and_keep_preceding_backspace_boundary() {
    use retex_rs2::session::EditingSession;
    let mut source = String::from(
        "\\pdfinfoomitdate=1\\pdftrailerid{}\n\\font\\body=cmr10 \\body\\hsize=300pt\\vsize=3000pt\n\\count7=0\n",
    );
    for index in 0..45 {
        source.push_str(&format!("\\global\\advance\\count7 by1 Paragraph {index:02} has precise state and current text.\\par\n"));
    }
    source.push_str("\\the\\count7\\par\\end\n");
    let mut settings = config();
    settings.progressive_pages = true;
    let mut session = EditingSession::new(settings, host(source.as_bytes())).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let start = source.find("Paragraph 35").unwrap() + "Paragraph 35".len();
    session
        .apply_edit("document", 1, start, start, b"x", 2)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let edited = source.replacen("Paragraph 35", "Paragraph 35x", 1);
    compare_fresh(&mut session, edited.as_bytes());
    assert!(
        session.checkpoint_storage()["body_states"]
            .as_u64()
            .unwrap()
            <= 64
    );
    let previous_start = edited.find("Paragraph 34").unwrap();
    session
        .apply_edit("document", 2, start, start + 1, b"", 3)
        .unwrap();
    assert!(session.reuse().restored_offset >= previous_start);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, source.as_bytes());
    let line_start = source[..source.find("Paragraph 35").unwrap()]
        .rfind('\n')
        .unwrap()
        + 1;
    session
        .apply_edit("document", 3, line_start - 1, line_start, b"", 4)
        .unwrap();
    assert!(session.reuse().restored_offset < line_start);
    let backspaced = format!("{}{}", &source[..line_start - 1], &source[line_start..]);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, backspaced.as_bytes());
    assert!(session.reuse().body_checkpoint_reuses > 0);
}

#[test]
fn body_line_checkpoints_preserve_unfinished_paragraphs_and_buffered_prefixes() {
    use retex_rs2::session::EditingSession;
    let mut source = String::from(
        "\\pdfinfoomitdate=1\\pdftrailerid{}\n\\font\\body=cmr10 \\body\\hsize=300pt\\vsize=3000pt\n\\count7=0\n",
    );
    for index in 0..40 {
        source.push_str(&format!("\\global\\advance\\count7 by1 Line{index:02} contains text that belongs to one unfinished paragraph.\n"));
    }
    source.push_str("\\par\n\\the\\count7\\par\\end\n");
    let mut settings = config();
    settings.progressive_pages = true;
    let mut session = EditingSession::new(settings, host(source.as_bytes())).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let position = source.find("Line20").unwrap() + 6;
    session
        .apply_edit("document", 1, position, position, b"x", 2)
        .unwrap();
    assert!(session.reuse().restored_offset >= source.find("Line18").unwrap());
    assert!(session.reuse().restored_offset <= source[..position].rfind('\n').unwrap() + 1);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let edited = source.replacen("Line20", "Line20x", 1);
    compare_fresh(&mut session, edited.as_bytes());
    let line_start = edited[..edited.find("Line20").unwrap()]
        .rfind('\n')
        .unwrap()
        + 1;
    session
        .apply_edit("document", 2, line_start - 1, line_start, b"", 3)
        .unwrap();
    assert!(session.reuse().restored_offset < line_start);
    let joined = format!("{}{}", &edited[..line_start - 1], &edited[line_start..]);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, joined.as_bytes());
    assert!(
        session.checkpoint_storage()["body_states"]
            .as_u64()
            .unwrap()
            <= 64
    );
}

#[test]
fn growing_buffered_lines_cannot_evict_valid_preceding_body_checkpoints() {
    use retex_rs2::session::EditingSession;
    let mut source = String::from(
        "\\pdfinfoomitdate=1\\pdftrailerid{}\n\\font\\body=cmr10 \\body\\hsize=300pt\\vsize=3000pt\n",
    );
    for index in 0..8 {
        source.push_str(&format!(
            "Line{index:02} has words in the same paragraph.\n"
        ));
    }
    source.push_str("\\par\n\\end\n");
    let mut settings = config();
    settings.progressive_pages = true;
    let mut session = EditingSession::new(settings, host(source.as_bytes())).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let mut position = source.find("Line04").unwrap();
    position += source[position..].find('\n').unwrap();
    let preceding = source.find("Line03").unwrap();
    for index in 0..40 {
        source.insert(position, 'x');
        position += 1;
        session
            .open_document(
                "document",
                index + 2,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert!(
            session.reuse().restored_offset >= preceding,
            "Invalid old buffered prefixes displaced the restart on edit {index}"
        );
        assert!(session.reuse().restored_offset < position);
        assert_eq!(settled(&mut session), RunProgress::Complete);
    }
    compare_fresh(&mut session, source.as_bytes());
    assert!(
        session.checkpoint_storage()["body_states"]
            .as_u64()
            .unwrap()
            <= 64
    );
}

#[test]
fn superseded_first_pass_can_restore_body_prefix_with_private_writes() {
    use retex_rs2::session::EditingSession;
    let mut source = String::from(
        "\\pdfinfoomitdate=1\\pdftrailerid{}\n\\font\\body=cmr10 \\body\\hsize=160pt\\vsize=80pt\\output={\\shipout\\box255}\n\\immediate\\openout1=notes.txt\\immediate\\write1{Prefix}\n",
    );
    for index in 0..16 {
        source.push_str(&format!(
            "Paragraph {index:02}. Current text needs precise execution and state.\\par\n"
        ));
    }
    source.push_str("\\immediate\\closeout1\\count7=0\\def\\work{\\advance\\count7 by1\\ifnum\\count7<1000\\expandafter\\work\\fi}\\work\\end\n");
    let mut settings = config();
    settings.progressive_pages = true;
    let mut session = EditingSession::new(settings, host(source.as_bytes())).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    session
        .begin_settling("document", 1, &[PathBuf::from("/project/notes.txt")], 3)
        .unwrap();
    while session.engine().unwrap().completed_page_count() == 0 {
        assert_eq!(session.advance(128).unwrap(), RunProgress::Yielded);
    }
    assert_eq!(session.engine().unwrap().status(), "running");
    let start = source.find("Paragraph 02").unwrap() + "Paragraph 02".len();
    session
        .apply_edit("document", 1, start, start, b"x", 2)
        .unwrap();
    assert!(session.reuse().body_checkpoint_reuses > 0);
    session
        .begin_settling("document", 2, &[PathBuf::from("/project/notes.txt")], 3)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(
        &mut session,
        source
            .replacen("Paragraph 02", "Paragraph 02x", 1)
            .as_bytes(),
    );
}

#[test]
fn progressive_checkpoints_preserve_included_physical_frames_and_spanning_paragraphs() {
    use retex_rs2::session::EditingSession;
    let source=b"\\pdfinfoomitdate=1\\pdftrailerid{}\n\\font\\body=cmr10 \\body\\hsize=120pt\\vsize=100pt\\output={\\shipout\\box255}\n\\input chapter.tex\nLast main paragraph.\\par\\end\n";
    let mut chapter = String::new();
    for index in 0..10 {
        chapter.push_str(&format!("Included {index}. "));
        chapter.push_str(
            &"A paragraph can span several output pages while retaining pending lines. ".repeat(14),
        );
        chapter.push_str("\\par\n");
    }
    let resources = vec![("chapter.tex", chapter.clone().into_bytes())];
    let h = host(source);
    h.apply(CatalogDelta {
        base_epoch: h.epoch(),
        new_epoch: h.epoch() + 1,
        policy_version: h.policy_version(),
        changes: vec![put(
            Path::new("/project/chapter.tex"),
            chapter.clone().into_bytes(),
        )],
    })
    .unwrap();
    let mut settings = config();
    settings.progressive_pages = true;
    let mut session = EditingSession::new(settings, h).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    while session.engine().unwrap().completed_page_count() == 0 {
        assert_eq!(session.advance(128).unwrap(), RunProgress::Yielded);
    }
    assert_eq!(session.engine().unwrap().core.input.in_open, 2);
    assert_eq!(session.page_checkpoint_coverage()[1].after_page, 1);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh_resources(&mut session, source, &resources);
    assert!(session.source_pages().get("/project/chapter.tex").is_some());
    let start = source.windows(4).position(|word| word == b"Last").unwrap();
    session
        .apply_edit("document", 1, start, start + 4, b"Changed", 2)
        .unwrap();
    assert!(session.reuse().restored_pages > 8);
    let mut edited = source.to_vec();
    edited.splice(start..start + 4, b"Changed".iter().copied());
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh_resources(&mut session, &edited, &resources);
    let changed = chapter.replace("Included 0.", "Changed included text.");
    session
        .update_resources(CatalogDelta {
            base_epoch: session.host().epoch(),
            new_epoch: session.host().epoch() + 1,
            policy_version: session.host().policy_version(),
            changes: vec![ResourceChange::Put {
                path: resource_path(Path::new("/project/chapter.tex")),
                id: ResourceId("/project/chapter.tex".into()),
                version: 2,
                bytes: changed.as_bytes().into(),
            }],
        })
        .unwrap();
    session
        .open_document(
            "document",
            3,
            Path::new("/project/document.tex"),
            edited.as_slice().into(),
        )
        .unwrap();
    assert_eq!(session.reuse().restored_pages, 0);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh_resources(
        &mut session,
        &edited,
        &[("chapter.tex", changed.into_bytes())],
    );
}

fn reusable_material_snapshot(mut value: serde_json::Value) -> serde_json::Value {
    fn ids(value: &serde_json::Value, map: &mut BTreeMap<i64, i64>) {
        match value {
            serde_json::Value::Object(fields) => {
                if let Some(id) = fields.get("node_id").and_then(|v| v.as_i64()) {
                    if id > 0 && !map.contains_key(&id) {
                        map.insert(id, map.len() as i64 + 1);
                    }
                }
                for value in fields.values() {
                    ids(value, map);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    ids(value, map);
                }
            }
            _ => {}
        }
    }
    fn material(value: &mut serde_json::Value, map: &mut BTreeMap<i64, i64>) {
        match value {
            serde_json::Value::Object(fields) => {
                for name in ["allocated_nodes", "peak_nodes", "reclaimed_nodes"] {
                    fields.remove(name);
                }
                for (name, value) in fields.iter_mut() {
                    if [
                        "node_id",
                        "root_node",
                        "link",
                        "child",
                        "pre_break",
                        "post_break",
                        "leader_ptr",
                    ]
                    .contains(&name.as_str())
                    {
                        if let Some(id) = value.as_i64().filter(|id| *id > 0) {
                            let next = map.len() as i64 + 1;
                            *value = serde_json::json!(*map.entry(id).or_insert(next));
                        }
                    } else {
                        material(value, map);
                    }
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    material(value, map);
                }
            }
            _ => {}
        }
    }
    let mut map = BTreeMap::new();
    ids(&value, &mut map);
    material(&mut value, &mut map);
    value
}

fn compare_reuse_fresh(
    session: &mut retex_rs2::session::EditingSession,
    source: &[u8],
    settings: EngineConfig,
) {
    let fresh_host = host(source);
    fresh_host.begin_effects(tag(1), 1_000_000).unwrap();
    let mut fresh = Engine::new_with_host(settings, fresh_host.clone()).unwrap();
    assert!(fresh.start_input(Path::new("/project/document.tex")));
    assert!(fresh.main_body());
    let engine = session.engine().unwrap();
    let actual = reusable_material_snapshot(observed(engine));
    let expected = reusable_material_snapshot(observed(&fresh));
    if actual != expected {
        std::fs::write(
            "/tmp/reuse-actual.json",
            serde_json::to_vec(&actual).unwrap(),
        )
        .unwrap();
        std::fs::write(
            "/tmp/reuse-expected.json",
            serde_json::to_vec(&expected).unwrap(),
        )
        .unwrap();
        panic!("Reuse output differs; /tmp/reuse-{{actual,expected}}.json");
    }
    assert_eq!(engine.env.values, fresh.env.values);
    assert_eq!(
        serde_json::to_value(&engine.env.meanings).unwrap(),
        serde_json::to_value(&fresh.env.meanings).unwrap()
    );
    assert_eq!(engine.core.input.token_count, fresh.core.input.token_count);
    assert_eq!(
        session.export_pdf().unwrap(),
        fresh.export_pdf_bytes().unwrap()
    );
    assert_eq!(
        session
            .host()
            .staged_outputs(session.tag().unwrap())
            .unwrap(),
        fresh_host.staged_outputs(&tag(1)).unwrap()
    );
}

#[test]
fn paragraph_execution_reuse_skips_pure_macros_and_rebases_sources() {
    use retex_rs2::session::EditingSession;
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\n\\font\\body=cmr10 \\body\\hsize=160pt\\vsize=500pt\n\\def\\word#1{#1 material}\\def\\unused{Original}\n\\tracingstats=1\n";
    let suffix = "\n\nUnchanged introduction uses \\word{reusable} and more ordinary words.\nIt has a second physical line for source attribution.\n\nLast paragraph has ordinary text.\n\n\\end\n";
    let original = format!("{prefix}First edited paragraph.{suffix}");
    let mut settings = config();
    settings.progressive_pages = true;
    let mut session = EditingSession::new(settings.clone(), host(original.as_bytes())).unwrap();
    for (i, first) in [
        "First edited paragraph.",
        "First edited paragraph grows.",
        "First edited paragraph grows longer.",
        "First edited\nparagraph grows longer.",
        "First edited paragraph.",
    ]
    .iter()
    .enumerate()
    {
        let source = format!("{prefix}{first}{suffix}");
        session
            .open_document(
                "document",
                i as u64 + 1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        let stats = session.engine().unwrap().execution_reuse_stats();
        // The final historical source can restore beyond this paragraph.
        if (1..4).contains(&i) {
            assert!(stats.hits >= 1, "{stats:?}");
            assert!(stats.avoided_macros >= 1, "{stats:?}");
        }
        let mut uncached = settings.clone();
        uncached.execution_reuse = false;
        compare_reuse_fresh(&mut session, source.as_bytes(), uncached);
    }
}

#[test]
fn paragraph_reuse_invalidates_relevant_definitions_and_settings() {
    use retex_rs2::session::EditingSession;
    let variants = [
        ("Original", "Other", "160pt", "0", "First"),
        ("Original", "Unrelated", "160pt", "0", "First grows"),
        ("Changed", "Unrelated", "160pt", "0", "First grows"),
        ("Changed", "Unrelated", "120pt", "0", "First grows"),
        ("Changed", "Unrelated", "120pt", "1", "First grows"),
    ];
    let mut settings = config();
    settings.progressive_pages = true;
    let mut session = EditingSession::new(settings.clone(), host(b"\\end")).unwrap();
    for (i, (word, unused, width, language, first)) in variants.iter().enumerate() {
        let source = format!(
            "\\pdfinfoomitdate=1\\pdftrailerid{{}}\n\\font\\body=cmr10 \\body\\hsize={width}\\vsize=500pt\\language={language}\n\\def\\word{{{word}}}\\def\\unused{{{unused}}}\n{first} edited paragraph.\n\nUnchanged paragraph uses \\word and several other words.\n\n\\end\n"
        );
        session
            .open_document(
                "document",
                i as u64 + 1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        let stats = session.engine().unwrap().execution_reuse_stats();
        if i == 1 {
            assert!(
                stats.avoided_macros > 0,
                "Unrelated redefinition must not invalidate pure macro reuse: {stats:?}"
            );
        }
        if i >= 2 {
            assert_eq!(
                stats.avoided_macros, 0,
                "Relevant dependency changed: {stats:?}"
            );
        }
        let mut uncached = settings.clone();
        uncached.execution_reuse = false;
        compare_reuse_fresh(&mut session, source.as_bytes(), uncached);
    }
}

#[test]
fn paragraph_reuse_rejects_ordered_effects_groups_and_page_dependent_reads() {
    use retex_rs2::session::EditingSession;
    let mut settings = config();
    settings.progressive_pages = true;
    for operation in [
        "\\global\\advance\\count7 by1",
        "{\\count7=8 local}",
        "\\the\\count7",
        "\\immediate\\write1{Ordered}",
        "\\mark{Marked}",
        "\\hbox{Nested}",
        "\\pdfliteral{0 g}",
    ] {
        let mut session = EditingSession::new(settings.clone(), host(b"\\end")).unwrap();
        for (revision, word) in [(1, "First"), (2, "First grows")] {
            let source = format!(
                "\\pdfinfoomitdate=1\\pdftrailerid{{}}\n\\font\\body=cmr10 \\body\\hsize=160pt\\vsize=500pt\\count7=0\n\\immediate\\openout1=notes.aux\n{word} edited paragraph.\n\nUnchanged text {operation} continues here.\n\n\\immediate\\closeout1\\end\n"
            );
            session
                .open_document(
                    "document",
                    revision,
                    Path::new("/project/document.tex"),
                    source.as_bytes().into(),
                )
                .unwrap();
            assert_eq!(settled(&mut session), RunProgress::Complete, "{operation}");
            let mut uncached = settings.clone();
            uncached.execution_reuse = false;
            compare_reuse_fresh(&mut session, source.as_bytes(), uncached);
            assert_eq!(
                session.engine().unwrap().execution_reuse_stats().hits,
                0,
                "{operation}"
            );
        }
    }
}

#[test]
fn paragraph_reuse_retains_pending_macro_tokens_and_runs_ordered_effects() {
    use retex_rs2::session::EditingSession;
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\n\\font\\body=cmr10 \\body\\hsize=200pt\\vsize=500pt\n\\let\\realpar=\\par\\count9=0\\immediate\\openout1=notes.aux\n\\def\\par{\\realpar\\global\\advance\\count9 by1\\immediate\\write1{End: \\the\\count9}}\n\\def\\word{reusable material}\n";
    let suffix = "\n\nUnchanged introduction uses \\word and ordinary text.\n\nLast text paragraph.\n\n\\immediate\\closeout1\\end\n";
    let mut settings = config();
    settings.progressive_pages = true;
    let mut session = EditingSession::new(settings.clone(), host(b"\\end")).unwrap();
    for (i, first) in [
        "First edited paragraph.",
        "First edited paragraph grows.",
        "First edited\nparagraph grows longer.",
    ]
    .iter()
    .enumerate()
    {
        let source = format!("{prefix}{first}{suffix}");
        session
            .open_document(
                "document",
                i as u64 + 1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        let stats = session.engine().unwrap().execution_reuse_stats();
        if i > 0 {
            assert!(stats.hits >= 1 && stats.avoided_macros >= 1, "{stats:?}");
        }
        let mut uncached = settings.clone();
        uncached.execution_reuse = false;
        compare_reuse_fresh(&mut session, source.as_bytes(), uncached);
    }
}

#[test]
fn complete_layout_reuse_keeps_end_hooks_and_changed_page_positions() {
    use retex_rs2::session::EditingSession;
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\n\\font\\body=cmr10 \\body\\hsize=160pt\\vsize=500pt\n\\let\\realpar=\\par\\count9=0\\immediate\\openout1=notes.aux\n\\def\\par{\\global\\advance\\count9 by1\\realpar\\immediate\\write1{End: \\the\\count9}}\n\\def\\word{reusable material}\n";
    let suffix = "\n\nUnchanged introduction uses \\word and ordinary text. Words occupy several lines at the same width.\n\nLast ordinary text paragraph also remains unchanged.\n\n\\immediate\\closeout1\\end\n";
    let mut settings = config();
    settings.progressive_pages = true;
    let mut session = EditingSession::new(settings.clone(), host(b"\\end")).unwrap();
    for (i, first) in [
        "First edited paragraph.",
        "First edited paragraph grows.",
        "First edited paragraph grows enough to wrap onto multiple lines at this width.",
        "First edited paragraph grows enough to wrap onto several lines with some more words here.",
    ]
    .iter()
    .enumerate()
    {
        let source = format!("{prefix}{first}{suffix}");
        session
            .open_document(
                "document",
                i as u64 + 1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        let stats = session.engine().unwrap().execution_reuse_stats();
        if i >= 2 {
            assert!(stats.layout_hits >= 1, "{stats:?}");
        }
        let mut uncached = settings.clone();
        uncached.execution_reuse = false;
        compare_reuse_fresh(&mut session, source.as_bytes(), uncached);
    }
}

#[test]
fn execution_reuse_cancelled_hits_roll_back_and_recover() {
    use retex_rs2::session::EditingSession;
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\n\\def\\word#1{#1 prepared material}\n";
    let suffix = "\n\nUnchanged introduction uses \\word{reusable} ordinary text.\n\n\\hbox{Unchanged constructed figure uses \\word{reusable} ordinary text.}\n\\immediate\\openout1=proof.txt\\immediate\\write1{Current}\\immediate\\closeout1\n\\par\\end\n";
    let original = format!("{prefix}First.{suffix}");
    let mut settings = config();
    settings.progressive_pages = true;
    let mut session = EditingSession::new(settings.clone(), host(original.as_bytes())).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            original.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let changed = format!("{prefix}First grows.{suffix}");
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            changed.as_bytes().into(),
        )
        .unwrap();
    let mut reached_hit = false;
    for _ in 0..100_000 {
        assert_eq!(session.advance(1).unwrap(), RunProgress::Yielded);
        if session.engine().unwrap().execution_reuse_stats().hits > 0 {
            reached_hit = true;
            break;
        }
    }
    assert!(reached_hit);
    session.cancel("document", 2).unwrap();
    assert_eq!(session.host().pending_bytes(), 0);
    let repaired = format!("{prefix}First grows\nagain.{suffix}");
    session
        .open_document(
            "document",
            3,
            Path::new("/project/document.tex"),
            repaired.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let mut uncached = settings;
    uncached.execution_reuse = false;
    compare_reuse_fresh(&mut session, repaired.as_bytes(), uncached);
}

#[test]
fn execution_reuse_font_resource_replacement_validates_metrics_after_hits() {
    use retex_rs2::session::EditingSession;
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\hsize=160pt\n\\def\\word{unchanged material}\n";
    let suffix = "\n\nUnchanged paragraph uses \\word with several ordinary words.\n\n\\hbox{Unchanged figure uses \\word with ordinary words.}\\par\\end\n";
    let mut settings = config();
    settings.progressive_pages = true;
    let original = format!("{prefix}First.{suffix}");
    let mut session = EditingSession::new(settings.clone(), host(original.as_bytes())).unwrap();
    for (revision, first) in [(1, "First."), (2, "First grows.")] {
        let source = format!("{prefix}{first}{suffix}");
        session
            .open_document(
                "document",
                revision,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
    }
    assert!(session.engine().unwrap().execution_reuse_stats().hits > 0);
    let font_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/assets/cmr10.tfm");
    let replacement = std::fs::read(font_path.with_file_name("cmr12.tfm")).unwrap();
    let mut change = put(&font_path, replacement.clone());
    if let ResourceChange::Put { version, .. } = &mut change {
        *version = 2;
    }
    let epoch = session.host().epoch();
    session
        .update_resources(CatalogDelta {
            base_epoch: epoch,
            new_epoch: epoch + 1,
            policy_version: 0,
            changes: vec![change],
        })
        .unwrap();
    let source = format!("{prefix}First grows again.{suffix}");
    session
        .open_document(
            "document",
            3,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh_resources(
        &mut session,
        source.as_bytes(),
        &[(font_path.to_str().unwrap(), replacement)],
    );
}

#[test]
fn grouped_execution_reuses_balanced_assignments_conditionals_fonts_and_pending_tokens() {
    use retex_rs2::session::EditingSession;
    let words = (0..24).map(|_| "\\word{saved} ").collect::<String>();
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\n\\font\\body=cmr10 \\font\\alt=cmti10 \\body\\hsize=160pt\\vsize=500pt\n\\count8=3\\count9=0\\def\\word#1{#1}\\def\\unused{Original}\n";
    let suffix = format!(
        "\n\nUnchanged heading \\begingroup\\count7=10 \\def\\tmp{{scoped}}\\afterassignment\\tmp\\count7=11 \\ifnum\\count8=3 Same\\else Different\\fi \\alt\\hbox{{{words}}}\\global\\advance\\count9 by1\\aftergroup Z\\endgroup tail.\\par\n\\immediate\\openout1=proof.txt\\immediate\\write1{{\\the\\count9}}\\immediate\\closeout1\\end\n"
    );
    let mut settings = config();
    settings.progressive_pages = true;
    settings.grouped_execution_reuse = true;
    // Exercise this group boundary independently of whole-paragraph admission.
    settings.stateful_paragraph_reuse = false;
    let original = format!("{prefix}First.{suffix}");
    let mut session = EditingSession::new(settings.clone(), host(original.as_bytes())).unwrap();
    for (i, first) in [
        "First.",
        "First grows.",
        "First grows again.",
        "First grows\non a new line.",
    ]
    .iter()
    .enumerate()
    {
        let source = format!("{prefix}{first}{suffix}");
        session
            .open_document(
                "document",
                i as u64 + 1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(
            settled(&mut session),
            RunProgress::Complete,
            "{:?}",
            session.engine().unwrap().core.diagnostics.errors
        );
        let stats = session.engine().unwrap().execution_reuse_stats();
        if i > 0 {
            assert!(stats.group_hits > 0, "{stats:?}");
            assert!(stats.avoided_macros >= 24, "{stats:?}");
            assert!(stats.group_effects_replayed > 0, "{stats:?}");
        }
        let mut uncached = settings.clone();
        uncached.execution_reuse = false;
        compare_reuse_fresh(&mut session, source.as_bytes(), uncached);
    }
}

#[test]
fn grouped_execution_reuses_image_construction_with_fresh_pdf_ownership() {
    use retex_rs2::session::EditingSession;
    for (file, fixture) in [
        ("figure.jpg", "preview-RGB.jpg"),
        ("figure.pdf", "preview-curves.pdf"),
    ] {
        let bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(fixture),
        )
        .unwrap();
        let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\hsize=160pt\\vsize=500pt\\count9=0\n";
        let suffix = format!(
            "\n\nConstructed figure \\begingroup\\pdfximage width20pt{{{file}}}\\setbox0=\\hbox{{\\pdfsave\\pdfsetmatrix{{1 0 0 1}}\\pdfrefximage\\pdflastximage\\pdfrestore}}\\wd0=25pt\\copy0\\setbox1=\\hbox{{\\unhcopy0}}\\unhbox1\\kern2pt\\box0\\global\\advance\\count9 by1\\endgroup tail.\\par\n\\immediate\\openout1=proof.txt\\immediate\\write1{{\\the\\count9}}\\immediate\\closeout1\\end\n"
        );
        let original = format!("{prefix}First.{suffix}");
        let h = host(original.as_bytes());
        h.apply(CatalogDelta {
            base_epoch: h.epoch(),
            new_epoch: h.epoch() + 1,
            policy_version: 0,
            changes: vec![put(&Path::new("/project").join(file), bytes.clone())],
        })
        .unwrap();
        let mut settings = config();
        settings.progressive_pages = true;
        settings.grouped_execution_reuse = true;
        // Exercise this group boundary independently of whole-paragraph admission.
        settings.stateful_paragraph_reuse = false;
        let mut session = EditingSession::new(settings.clone(), h).unwrap();
        for (i, first) in [
            "First.",
            "First grows.",
            "First grows again.",
            "First grows\non a new line.",
        ]
        .iter()
        .enumerate()
        {
            let source = format!("{prefix}{first}{suffix}");
            session
                .open_document(
                    "document",
                    i as u64 + 1,
                    Path::new("/project/document.tex"),
                    source.as_bytes().into(),
                )
                .unwrap();
            assert_eq!(
                settled(&mut session),
                RunProgress::Complete,
                "{:?}",
                session.engine().unwrap().core.diagnostics.errors
            );
            let stats = session.engine().unwrap().execution_reuse_stats();
            if i > 0 {
                assert!(stats.group_hits > 0, "{file}: {stats:?}");
            }
            let mut reference = settings.clone();
            reference.execution_reuse = false;
            compare_fresh_config(
                &mut session,
                source.as_bytes(),
                &[(file, bytes.clone())],
                reference,
            );
        }
    }
}

#[test]
fn grouped_execution_tracks_reads_before_writes_and_noop_globals() {
    use retex_rs2::session::EditingSession;
    let words = "\\word{saved} ".repeat(24);
    let prefix =
        "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\def\\word#1{#1}\\count9=0\n";
    // The old count7 and tmp meaning are assignment bookkeeping, not semantic
    // inputs. The nested same-value global must still cancel outer restoration.
    let suffix = format!(
        "\n\nStable \\begingroup\\count7=7 \\def\\tmp{{new}}\\tmp \\begingroup\\global\\count7=7 \\endgroup\\hbox{{{words}}}\\global\\advance\\count9 by1\\endgroup\\the\\count7\\par\\immediate\\openout1=proof.txt\\immediate\\write1{{\\the\\count7/\\the\\count9}}\\immediate\\closeout1\\end\n"
    );
    let original = format!("{prefix}\\count7=1\\def\\tmp{{old}}First.{suffix}");
    let mut settings = config();
    settings.progressive_pages = true;
    settings.grouped_execution_reuse = true;
    // Exercise this group boundary independently of whole-paragraph admission.
    settings.stateful_paragraph_reuse = false;
    let mut session = EditingSession::new(settings.clone(), host(original.as_bytes())).unwrap();
    for i in 0..4 {
        let source = format!(
            "{prefix}\\count7={}\\def\\tmp{{old{i}}}First{}.{suffix}",
            i + 1,
            " grows".repeat(i)
        );
        session
            .open_document(
                "document",
                i as u64 + 1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        let stats = session.engine().unwrap().execution_reuse_stats();
        if i > 0 {
            assert!(stats.group_hits > 0, "{stats:?}");
        }
        let mut reference = settings.clone();
        reference.execution_reuse = false;
        compare_reuse_fresh(&mut session, source.as_bytes(), reference);
    }
}

#[test]
fn grouped_execution_restored_bindings_become_dependencies_again() {
    use retex_rs2::session::EditingSession;
    let words = "\\word{saved} ".repeat(24);
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\def\\word#1{#1}\n";
    let suffix = format!(
        "\n\nStable \\iftrue\\begingroup\\begingroup\\count7=77\\def\\tmp{{temporary}}\\endgroup\\the\\count7\\tmp\\hbox{{{words}}}\\endgroup\\fi tail.\\par\\end\n"
    );
    let original = format!("{prefix}\\count7=1\\def\\tmp{{one}}First.{suffix}");
    let mut settings = config();
    settings.progressive_pages = true;
    settings.grouped_execution_reuse = true;
    // Exercise this group boundary independently of whole-paragraph admission.
    settings.stateful_paragraph_reuse = false;
    let mut session = EditingSession::new(settings.clone(), host(original.as_bytes())).unwrap();
    for (i, (number, meaning)) in [(1, "one"), (1, "one"), (2, "two"), (2, "two")]
        .iter()
        .enumerate()
    {
        let source = format!(
            "{prefix}\\count7={number}\\def\\tmp{{{meaning}}}First{}.{suffix}",
            " grows".repeat(i)
        );
        session
            .open_document(
                "document",
                i as u64 + 1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        let stats = session.engine().unwrap().execution_reuse_stats();
        if i == 1 || i == 3 {
            assert!(stats.group_hits > 0, "{stats:?}");
        }
        let mut reference = settings.clone();
        reference.execution_reuse = false;
        compare_reuse_fresh(&mut session, source.as_bytes(), reference);
    }
}

#[test]
fn grouped_execution_isolates_pending_prefix_glyphs_and_deferred_effects() {
    use retex_rs2::session::EditingSession;
    let words = "\\word{saved} ".repeat(24);
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\font\\alt=cmti10 \\body\\def\\word#1{#1}\\def\\later{old}\\immediate\\openout1=proof.txt\n";
    let suffix = format!(
        "\n\nStable of\\begingroup\\alt fi\\hbox{{{words}}}\\write1{{Deferred:\\later}}\\pdfliteral shipout{{q Q}}\\endgroup tail.\\par\\def\\later{{new}}\\end\n"
    );
    let original = format!("{prefix}First.{suffix}");
    let mut settings = config();
    settings.progressive_pages = true;
    settings.grouped_execution_reuse = true;
    // Exercise this group boundary independently of whole-paragraph admission.
    settings.stateful_paragraph_reuse = false;
    let mut session = EditingSession::new(settings.clone(), host(original.as_bytes())).unwrap();
    for i in 0..4 {
        let source = format!("{prefix}First{}.{suffix}", " grows".repeat(i));
        session
            .open_document(
                "document",
                i as u64 + 1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(
            settled(&mut session),
            RunProgress::Complete,
            "{:?}",
            session.engine().unwrap().core.diagnostics.errors
        );
        let stats = session.engine().unwrap().execution_reuse_stats();
        if i > 0 {
            assert!(stats.group_hits > 0, "{stats:?}");
        }
        let mut reference = settings.clone();
        reference.execution_reuse = false;
        compare_reuse_fresh(&mut session, source.as_bytes(), reference);
    }
}

#[test]
fn grouped_execution_resource_replacement_and_cancelled_image_hits_recover() {
    use retex_rs2::session::EditingSession;
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\hsize=160pt\\vsize=500pt\\immediate\\openout1=proof.txt\n";
    let suffix = "\n\nFigure \\begingroup\\pdfximage width20pt{figure.jpg}\\setbox0=\\hbox{\\pdfrefximage\\pdflastximage}\\copy0\\box0\\write1{Deferred}\\endgroup tail.\\par\\end\n";
    let original = format!("{prefix}First.{suffix}");
    let initial =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/preview-RGB.jpg"))
            .unwrap();
    let replacement =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/preview-L.jpg"))
            .unwrap();
    let h = host(original.as_bytes());
    h.apply(CatalogDelta {
        base_epoch: h.epoch(),
        new_epoch: h.epoch() + 1,
        policy_version: 0,
        changes: vec![put(Path::new("/project/figure.jpg"), initial.clone())],
    })
    .unwrap();
    let mut settings = config();
    settings.grouped_execution_reuse = true;
    // Exercise this group boundary independently of whole-paragraph admission.
    settings.stateful_paragraph_reuse = false;
    settings.progressive_pages = true;
    let mut session = EditingSession::new(settings.clone(), h).unwrap();
    for i in 0..2 {
        let source = format!("{prefix}First{}.{suffix}", " grows".repeat(i));
        session
            .open_document(
                "document",
                i as u64 + 1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        if i > 0 {
            assert!(session.engine().unwrap().execution_reuse_stats().group_hits > 0);
        }
    }
    let source = format!("{prefix}First grows again.{suffix}");
    session
        .open_document(
            "document",
            3,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    let mut hit = false;
    for _ in 0..100_000 {
        assert_eq!(session.advance(1).unwrap(), RunProgress::Yielded);
        if session.engine().unwrap().execution_reuse_stats().group_hits > 0 {
            hit = true;
            break;
        }
    }
    assert!(hit);
    session.cancel("document", 3).unwrap();
    assert_eq!(session.host().pending_bytes(), 0);
    let mut change = put(Path::new("/project/figure.jpg"), replacement.clone());
    if let ResourceChange::Put { version, .. } = &mut change {
        *version = 2;
    }
    let epoch = session.host().epoch();
    session
        .update_resources(CatalogDelta {
            base_epoch: epoch,
            new_epoch: epoch + 1,
            policy_version: 0,
            changes: vec![change],
        })
        .unwrap();
    let mut reference = settings.clone();
    reference.execution_reuse = false;
    for revision in 4..6 {
        let source = format!(
            "{prefix}First{}.{suffix}",
            " repaired".repeat(revision as usize)
        );
        session
            .open_document(
                "document",
                revision,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        compare_fresh_config(
            &mut session,
            source.as_bytes(),
            &[("figure.jpg", replacement.clone())],
            reference.clone(),
        );
        if revision == 5 {
            assert!(session.engine().unwrap().execution_reuse_stats().group_hits > 0);
        }
    }
}

#[test]
fn grouped_execution_rejects_relevant_inputs_but_ignores_unrelated_definitions() {
    use retex_rs2::session::EditingSession;
    let words = "\\word{saved} ".repeat(24);
    let prefix =
        "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\font\\alt=cmti10 \\body\n";
    let suffix = format!(
        "\n\nStable \\begingroup\\ifnum\\count8=3 Same\\else Other\\fi\\dimen1=\\dimexpr\\dimen0+1pt\\relax\\kern\\dimen1\\toks0={{tokens}}\\the\\toks0\\hbox{{{words}}}\\endgroup tail.\\par\\end\n"
    );
    let mut settings = config();
    settings.grouped_execution_reuse = true;
    // Exercise this group boundary independently of whole-paragraph admission.
    settings.stateful_paragraph_reuse = false;
    settings.progressive_pages = true;
    let initial = format!(
        "{prefix}\\def\\word#1{{#1}}\\def\\unused{{Original}}\\count8=3\\dimen0=2pt First.{suffix}"
    );
    let mut session = EditingSession::new(settings.clone(), host(initial.as_bytes())).unwrap();
    for (i, (condition, dimension, body, unused, font)) in [
        (3, 2, "#1", "Original", "body"),
        (3, 2, "#1", "Changed", "body"),
        (4, 2, "#1", "Changed", "body"),
        (4, 2, "#1", "Changed", "body"),
        (4, 3, "#1", "Changed", "body"),
        (4, 3, "#1 changed", "Changed", "body"),
        (4, 3, "#1 changed", "Changed", "alt"),
        (4, 3, "#1 changed", "Changed", "alt"),
    ]
    .iter()
    .enumerate()
    {
        let source = format!(
            "{prefix}\\def\\word#1{{{body}}}\\def\\unused{{{unused}}}\\count8={condition}\\dimen0={dimension}pt \\{font} First{}.{suffix}",
            " grows".repeat(i)
        );
        session
            .open_document(
                "document",
                i as u64 + 1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        let stats = session.engine().unwrap().execution_reuse_stats();
        if [1, 3, 7].contains(&i) {
            assert!(stats.group_hits > 0, "{stats:?}");
        }
        let mut reference = settings.clone();
        reference.execution_reuse = false;
        compare_reuse_fresh(&mut session, source.as_bytes(), reference);
    }
}

#[test]
fn grouped_execution_eviction_changes_speed_only() {
    use retex_rs2::session::EditingSession;
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\def\\word#1{#1}\\hsize=160pt\\vsize=500pt\n";
    let mut settings = config();
    settings.progressive_pages = true;
    settings.grouped_execution_reuse = true;
    // Exercise this group boundary independently of whole-paragraph admission.
    settings.stateful_paragraph_reuse = false;
    let mut session = EditingSession::new(settings.clone(), host(b"\\end")).unwrap();
    for revision in 1..=42 {
        let family = if revision <= 40 {
            (revision - 1) / 2
        } else {
            0
        };
        let words = format!("\\word{{family {family}}} ").repeat(24);
        let source = format!(
            "{prefix}First revision {revision}.\n\nStable \\begingroup\\hbox{{{words}}}\\endgroup tail.\\par\\end\n"
        );
        session
            .open_document(
                "document",
                revision,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        let stats = session.engine().unwrap().execution_reuse_stats();
        assert!(stats.group_entries <= 16, "{stats:?}");
        assert!(
            stats.retained_bytes <= 80 * 1024 * 1024 + 128 * 1024,
            "{stats:?}"
        );
        if revision % 2 == 0 {
            assert!(stats.group_hits > 0, "{revision}: {stats:?}");
        }
        if revision % 10 == 0 || revision >= 41 {
            let mut reference = settings.clone();
            reference.execution_reuse = false;
            compare_reuse_fresh(&mut session, source.as_bytes(), reference);
        }
    }
}

#[test]
fn grouped_execution_cancelled_recorders_do_not_block_recovery_recording() {
    use retex_rs2::session::EditingSession;
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\def\\word#1{#1}\n";
    let suffix = format!(
        "\n\nStable \\begingroup\\hbox{{{}}}\\endgroup tail.\\par\\end\n",
        "\\word{saved} ".repeat(24)
    );
    let original = format!("{prefix}First.{suffix}");
    let mut settings = config();
    settings.progressive_pages = true;
    settings.grouped_execution_reuse = true;
    // Exercise this group boundary independently of whole-paragraph admission.
    settings.stateful_paragraph_reuse = false;
    let mut session = EditingSession::new(settings.clone(), host(original.as_bytes())).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            original.as_bytes().into(),
        )
        .unwrap();
    let mut recorded = false;
    for _ in 0..100_000 {
        assert_eq!(session.advance(1).unwrap(), RunProgress::Yielded);
        if session
            .engine()
            .unwrap()
            .execution_reuse_stats()
            .group_recording
        {
            recorded = true;
            break;
        }
    }
    assert!(recorded);
    session.cancel("document", 1).unwrap();
    assert_eq!(session.host().pending_bytes(), 0);
    // Deliberately retain the old engine: no page acknowledgement/release call.
    for revision in 2..4 {
        let source = format!(
            "{prefix}First{}.{suffix}",
            " recovered".repeat(revision as usize)
        );
        session
            .open_document(
                "document",
                revision,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        let stats = session.engine().unwrap().execution_reuse_stats();
        assert!(stats.group_entries > 0, "{stats:?}");
        if revision == 3 {
            assert!(stats.group_hits > 0, "{stats:?}");
        }
        let mut reference = settings.clone();
        reference.execution_reuse = false;
        compare_reuse_fresh(&mut session, source.as_bytes(), reference);
    }
}

#[test]
fn grouped_execution_constructs_fresh_classic_fonts_and_restores_local_aliases() {
    use retex_rs2::session::EditingSession;
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\hsize=160pt\\vsize=500pt\\count9=0\\def\\word#1{#1}\n";
    let words = "\\word{saved} ".repeat(24);
    let suffix = format!(
        "\n\nStable \\begingroup\\font\\temporary=cmti10 at9pt\\hyphenchar\\temporary=45\\skewchar\\temporary=-1\\fontdimen2\\temporary=3pt\\edef\\identity{{\\the\\temporary}}\\identity\\global\\edef\\description{{\\fontname\\temporary/\\the\\hyphenchar\\temporary/\\the\\fontdimen2\\temporary/\\the\\font}}\\hbox{{{words}}}\\global\\advance\\count9 by1\\endgroup tail.\\par\\immediate\\openout1=proof.txt\\immediate\\write1{{\\the\\count9/\\description}}\\immediate\\closeout1\\end\n"
    );
    let original = format!("{prefix}First.{suffix}");
    let mut settings = config();
    settings.progressive_pages = true;
    settings.grouped_execution_reuse = true;
    // Exercise this group boundary independently of whole-paragraph admission.
    settings.stateful_paragraph_reuse = false;
    settings.font_construction_reuse = true;
    let mut session = EditingSession::new(settings.clone(), host(original.as_bytes())).unwrap();
    for i in 0..4 {
        let shift = if i == 2 { "\n" } else { "" };
        let source = format!("{prefix}{shift}First{}.{suffix}", " grows".repeat(i));
        session
            .open_document(
                "document",
                i as u64 + 1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(
            settled(&mut session),
            RunProgress::Complete,
            "{:?}",
            session.engine().unwrap().core.diagnostics.errors
        );
        let stats = session.engine().unwrap().execution_reuse_stats();
        if i > 0 {
            assert!(stats.group_hits > 0, "{stats:?}");
        }
        let mut reference = settings.clone();
        reference.execution_reuse = false;
        compare_reuse_fresh(&mut session, source.as_bytes(), reference);
    }
}

#[test]
fn grouped_font_construction_validates_resource_replacement_and_lookup_precedence() {
    use retex_rs2::session::EditingSession;
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/assets");
    let initial = std::fs::read(assets.join("cmti10.tfm")).unwrap();
    let replacement = std::fs::read(assets.join("cmr12.tfm")).unwrap();
    let program = std::fs::read(assets.join("cmti10.pfb")).unwrap();
    let words = "\\word{saved} ".repeat(24);
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\hsize=160pt\\vsize=500pt\\def\\word#1{#1}\n";
    let suffix = format!(
        "\n\nStable \\begingroup\\font\\temporary=selected at9pt\\temporary\\hbox{{{words}}}\\endgroup tail.\\par\\end\n"
    );
    let original = format!("{prefix}First.{suffix}");
    let h = host(original.as_bytes());
    h.apply(CatalogDelta {
        base_epoch: h.epoch(),
        new_epoch: h.epoch() + 1,
        policy_version: 0,
        changes: vec![
            put(Path::new("/project/selected.tfm"), initial.clone()),
            put(Path::new("/project/selected.pfb"), program.clone()),
        ],
    })
    .unwrap();
    let mut settings = config();
    settings.progressive_pages = true;
    settings.grouped_execution_reuse = true;
    // Exercise this group boundary independently of whole-paragraph admission.
    settings.stateful_paragraph_reuse = false;
    settings.font_construction_reuse = true;
    let mut session = EditingSession::new(settings.clone(), h).unwrap();
    for revision in 1..=6 {
        if revision == 3 || revision == 5 {
            let path = if revision == 3 {
                PathBuf::from("/project/selected.tfm")
            } else {
                assets.join("selected.tfm")
            };
            let mut change = put(
                &path,
                if revision == 3 {
                    replacement.clone()
                } else {
                    initial.clone()
                },
            );
            if revision == 3 {
                if let ResourceChange::Put { version, .. } = &mut change {
                    *version = 2;
                }
            }
            let epoch = session.host().epoch();
            session
                .update_resources(CatalogDelta {
                    base_epoch: epoch,
                    new_epoch: epoch + 1,
                    policy_version: 0,
                    changes: vec![change],
                })
                .unwrap();
        }
        let source = format!(
            "{prefix}First{}.{suffix}",
            " grows".repeat(revision as usize)
        );
        session
            .open_document(
                "document",
                revision,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(
            settled(&mut session),
            RunProgress::Complete,
            "{:?}",
            session.engine().unwrap().core.diagnostics.errors
        );
        let stats = session.engine().unwrap().execution_reuse_stats();
        if revision % 2 == 0 {
            assert!(stats.group_hits > 0, "{stats:?}");
        }
        let mut resources = vec![
            (
                "selected.tfm".to_string(),
                if revision < 3 {
                    initial.clone()
                } else {
                    replacement.clone()
                },
            ),
            ("selected.pfb".to_string(), program.clone()),
        ];
        if revision >= 5 {
            resources.push((
                assets.join("selected.tfm").to_str().unwrap().to_string(),
                initial.clone(),
            ));
        }
        let files = resources
            .iter()
            .map(|(p, b)| (p.as_str(), b.clone()))
            .collect::<Vec<_>>();
        let mut reference = settings.clone();
        reference.execution_reuse = false;
        compare_fresh_config(&mut session, source.as_bytes(), &files, reference);
    }
}

#[test]
fn grouped_font_construction_rejects_mutation_of_preexisting_font_slots() {
    use retex_rs2::session::EditingSession;
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\def\\word#1{#1}\n";
    let words = "\\word{saved} ".repeat(24);
    let suffix = format!(
        "\n\nStable \\begingroup\\fontdimen2\\body=4pt\\hyphenchar\\body=43\\hbox{{{words}}}\\endgroup tail.\\par\\end\n"
    );
    let original = format!("{prefix}First.{suffix}");
    let mut settings = config();
    settings.progressive_pages = true;
    settings.grouped_execution_reuse = true;
    // Exercise this group boundary independently of whole-paragraph admission.
    settings.stateful_paragraph_reuse = false;
    settings.font_construction_reuse = true;
    let mut session = EditingSession::new(settings.clone(), host(original.as_bytes())).unwrap();
    for revision in 1..4 {
        let source = format!(
            "{prefix}First{}.{suffix}",
            " grows".repeat(revision as usize)
        );
        session
            .open_document(
                "document",
                revision,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        assert_eq!(
            session.engine().unwrap().execution_reuse_stats().group_hits,
            0
        );
        let mut reference = settings.clone();
        reference.execution_reuse = false;
        compare_reuse_fresh(&mut session, source.as_bytes(), reference);
    }
}

#[test]
fn grouped_execution_replays_existing_stream_writes_and_global_changes_in_order() {
    use retex_rs2::session::EditingSession;
    let words = "\\word{saved} ".repeat(24);
    let suffix = format!(
        "\n\nStable \\begingroup\\global\\advance\\count9 by1\\immediate\\write1{{First:\\the\\count9}}\\hbox{{{words}}}\\global\\advance\\count9 by1\\immediate\\write1{{Second:\\the\\count9}}\\endgroup tail.\\par\\immediate\\write1{{After:\\the\\count9}}\\immediate\\closeout1\\end\n"
    );
    let source = |i, number| {
        format!(
            "\\pdfinfoomitdate=1\\pdftrailerid{{}}\\font\\body=cmr10 \\body\\hsize=180pt\\vsize=500pt\\def\\word#1{{#1}}\n\\count9={number}\\count8={i}\n\\immediate\\openout1=proof.txt\\immediate\\write1{{Before}}\nFirst gy{}.{suffix}",
            " grows".repeat(i)
        )
    };
    let mut settings = config();
    settings.progressive_pages = true;
    settings.grouped_execution_reuse = true;
    // Exercise this group boundary independently of whole-paragraph admission.
    settings.stateful_paragraph_reuse = false;
    settings.stateful_execution_reuse = true;
    let mut session = EditingSession::new(settings.clone(), host(source(0, 0).as_bytes())).unwrap();
    for (i, number) in [0, 0, 5, 5].iter().enumerate() {
        let input = source(i, *number);
        session
            .open_document(
                "document",
                i as u64 + 1,
                Path::new("/project/document.tex"),
                input.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(
            settled(&mut session),
            RunProgress::Complete,
            "{:?}",
            session.engine().unwrap().core.diagnostics.errors
        );
        let stats = session.engine().unwrap().execution_reuse_stats();
        if i == 1 || i == 3 {
            assert!(stats.group_hits > 0, "{stats:?}");
        }
        if i == 2 {
            assert_eq!(stats.group_hits, 0, "{stats:?}");
        }
        let mut reference = settings.clone();
        reference.execution_reuse = false;
        compare_reuse_fresh(&mut session, input.as_bytes(), reference);
    }
}

#[test]
fn grouped_execution_does_not_publish_entries_from_error_cleanup() {
    use retex_rs2::session::EditingSession;
    let prefix = "\\font\\body=cmr10 \\body\\hsize=180pt\\vsize=500pt\\def\\word#1{#1}\n";
    let words = "\\word{saved} ".repeat(24);
    let suffix = format!("\n\nStable \\hbox{{\\begingroup {words}}}\\par\\end\n");
    let original = format!("{prefix}First gy.{suffix}");
    let mut settings = config();
    settings.progressive_pages = true;
    settings.grouped_execution_reuse = true;
    // Exercise this group boundary independently of whole-paragraph admission.
    settings.stateful_paragraph_reuse = false;
    settings.stateful_execution_reuse = true;
    let mut session = EditingSession::new(settings.clone(), host(original.as_bytes())).unwrap();
    for i in 0..3 {
        let source = format!("{prefix}First gy{}.{suffix}", " grows".repeat(i));
        session
            .open_document(
                "document",
                i as u64 + 1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Failed);
        let stats = session.engine().unwrap().execution_reuse_stats();
        assert_eq!(stats.group_entries, 0, "{stats:?}");
        assert_eq!(stats.group_hits, 0, "{stats:?}");
        let mut reference = settings.clone();
        reference.execution_reuse = false;
        // The general helper intentionally requires successful input. Here the
        // independent reference must reproduce the failure and partial scene.
        let fresh_host = host(source.as_bytes());
        fresh_host.begin_effects(tag(1), 1_000_000).unwrap();
        let mut fresh = Engine::new_with_host(reference, fresh_host).unwrap();
        assert!(fresh.start_input(Path::new("/project/document.tex")));
        assert!(!fresh.main_body());
        let actual = session.engine().unwrap();
        assert_eq!(
            reusable_material_snapshot(observed(actual)),
            reusable_material_snapshot(observed(&fresh))
        );
        assert_eq!(actual.env.values, fresh.env.values);
        assert_eq!(
            serde_json::to_value(&actual.env.meanings).unwrap(),
            serde_json::to_value(&fresh.env.meanings).unwrap()
        );
        assert_eq!(actual.core.input.token_count, fresh.core.input.token_count);
        assert!(session.export_pdf().unwrap().is_none());
    }
}

#[test]
fn grouped_execution_preserves_incoming_conditional_frames_and_diagnostics() {
    use retex_rs2::session::EditingSession;
    let prefix = "\\font\\body=cmr10 \\body\\hsize=180pt\\vsize=500pt\\def\\word#1{#1}\n";
    let words = "\\word{saved} ".repeat(24);
    for replace in [false, true] {
        let middle = if replace { "\\fi\n\\iftrue " } else { "" };
        let suffix = format!(
            "\n\n\\iftrue Stable \\begingroup{middle}\\hbox{{{words}}}\\endgroup tail.\\par\\end\n"
        );
        let mut settings = config();
        settings.progressive_pages = true;
        settings.grouped_execution_reuse = true;
        // Exercise this group boundary independently of whole-paragraph admission.
        settings.stateful_paragraph_reuse = false;
        let original = format!("{prefix}First gy.{suffix}");
        let mut session = EditingSession::new(settings.clone(), host(original.as_bytes())).unwrap();
        for i in 0..3 {
            let input = format!("{prefix}First gy{}.{suffix}", " grows".repeat(i));
            session
                .open_document(
                    "document",
                    i as u64 + 1,
                    Path::new("/project/document.tex"),
                    input.as_bytes().into(),
                )
                .unwrap();
            assert_eq!(settled(&mut session), RunProgress::Complete);
            let stats = session.engine().unwrap().execution_reuse_stats();
            if replace {
                assert_eq!(stats.group_entries, 0, "{stats:?}");
                assert_eq!(stats.group_hits, 0, "{stats:?}");
            } else if i > 0 {
                assert!(stats.group_hits > 0, "{stats:?}");
            }
            let mut reference = settings.clone();
            reference.execution_reuse = false;
            compare_reuse_fresh(&mut session, input.as_bytes(), reference);
        }
    }
}

#[test]
fn stateful_paragraph_reuse_replays_root_locals_and_preserves_outer_restoration() {
    use retex_rs2::session::EditingSession;
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\hsize=180pt\\vsize=500pt\\def\\word#1{#1}\n";
    let words = "\\word{saved} ".repeat(24);
    let source = |i, relevant, unrelated| {
        format!(
            "{prefix}\\count9={relevant}\\count7={unrelated}\\immediate\\openout2=proof.txt\n\\begingroup First gy{}.\\par\n\nStable \\advance\\count9 by1\\def\\label{{Local}}\\ifnum\\count9>2 Yes\\else No\\fi \\begingroup\\count8=3\\global\\advance\\count10 by1\\endgroup {words}\\accent19 e\\write2{{Delayed:\\the\\count9}}\\skip0=\\lastskip\\unskip\\kern2pt\\dimen0=\\lastkern\\unkern\\penalty7\\count0=\\lastpenalty\\unpenalty\\label.\\par\nInside: \\the\\count9/\\the\\count10/\\label.\\par\\endgroup Outside: \\the\\count9/\\the\\count10.\\par\\closeout2\\end\n",
            format!(
                "{}{}",
                " grows".repeat(i),
                if i >= 8 { "\nshifted" } else { "" }
            )
        )
    };
    let mut settings = config();
    settings.progressive_pages = true;
    settings.grouped_execution_reuse = true;
    settings.stateful_execution_reuse = true;
    settings.stateful_paragraph_reuse = true;
    let mut session =
        EditingSession::new(settings.clone(), host(source(0, 0, 0).as_bytes())).unwrap();
    for (i, (relevant, unrelated)) in [
        (0, 0),
        (0, 0),
        (0, 0),
        (0, 0),
        (5, 0),
        (5, 9),
        (5, 9),
        (5, 9),
        (5, 9),
        (5, 9),
    ]
    .into_iter()
    .enumerate()
    {
        let input = source(i, relevant, unrelated);
        session
            .open_document(
                "document",
                i as u64 + 1,
                Path::new("/project/document.tex"),
                input.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(
            settled(&mut session),
            RunProgress::Complete,
            "{:?}",
            session.engine().unwrap().core.diagnostics.errors
        );
        let stats = session.engine().unwrap().execution_reuse_stats();
        if i == 2 || i == 3 || i == 6 || i == 7 || i == 8 || i == 9 {
            assert!(
                stats
                    .recent_regions
                    .iter()
                    .any(|r| r.prepared && r.grouped && r.macros >= 24),
                "{stats:?}"
            );
        }
        if i == 3 || i == 7 || i == 9 {
            assert!(stats.layout_hits > 0, "{stats:?}");
        }
        let mut reference = settings.clone();
        reference.execution_reuse = false;
        compare_reuse_fresh(&mut session, input.as_bytes(), reference);
    }
}

#[test]
fn alignment_material_reuse_keeps_cell_packing_and_effects_outside() {
    use retex_rs2::session::EditingSession;
    for (file, fixture) in [
        ("figure.jpg", "preview-RGB.jpg"),
        ("figure.pdf", "preview-curves.pdf"),
    ] {
        let mut bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(fixture),
        )
        .unwrap();
        let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\sy=cmsy10 \\font\\ex=cmex10 \\textfont2=\\sy\\scriptfont2=\\sy\\scriptscriptfont2=\\sy\\textfont3=\\ex\\scriptfont3=\\ex\\scriptscriptfont3=\\ex\\font\\body=cmr10 \\font\\alt=cmti10 \\body\\hsize=180pt\\vsize=500pt\\def\\word#1{#1}\\count9=0\\count8=3\\immediate\\openout1=proof.txt\n";
        let construction = format!(
            "\\begingroup\\ifnum\\count8=3\\alt\\else\\body\\fi\\count7=10\\advance\\count7 by2\\hbox{{{}}}\\pdfximage width20pt{{{file}}}\\pdfsave\\pdfsetmatrix{{1 0 0 1}}\\pdfrefximage\\pdflastximage\\pdfrestore\\global\\advance\\count9 by1\\immediate\\write1{{Now:\\the\\count9}}\\write1{{Later:\\the\\count9}}\\aftergroup\\relax\\endgroup",
            "\\word{saved} ".repeat(24)
        );
        let original = format!(
            "{prefix}First.\\par\n\\halign{{#\\hfil&\\hskip3pt#\\hfil\\cr {construction}&Other\\cr}}\\par\\end\n"
        );
        let h = host(original.as_bytes());
        h.apply(CatalogDelta {
            base_epoch: h.epoch(),
            new_epoch: h.epoch() + 1,
            policy_version: 0,
            changes: vec![put(&Path::new("/project").join(file), bytes.clone())],
        })
        .unwrap();
        let mut settings = config();
        settings.progressive_pages = true;
        settings.alignment_material_reuse = true;
        settings.material_region_admission = true;
        let mut session = EditingSession::new(settings.clone(), h).unwrap();
        for (i, (value, template, span)) in [
            (3, "#\\hfil", false),
            (3, "#\\hfil", false),
            (4, "#\\hfil", false),
            (4, "#\\hfil", false),
            (4, "\\hskip3pt#\\hfil", false),
            (4, "\\hskip3pt#\\hfil", false),
            (4, "#\\hfil", true),
            (4, "#\\hfil", true),
        ]
        .into_iter()
        .enumerate()
        {
            if i == 4 && file == "figure.jpg" {
                bytes = std::fs::read(
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/preview-L.jpg"),
                )
                .unwrap();
                let epoch = session.host().epoch();
                let mut change = put(Path::new("/project/figure.jpg"), bytes.clone());
                if let ResourceChange::Put { version, .. } = &mut change {
                    *version = 2;
                }
                session
                    .update_resources(CatalogDelta {
                        base_epoch: epoch,
                        new_epoch: epoch + 1,
                        policy_version: 0,
                        changes: vec![change],
                    })
                    .unwrap();
            }
            let prefix = prefix.replace("count8=3", &format!("count8={value}"));
            let prefix = if i >= 4 {
                prefix.replace("hsize=180pt", "hsize=190pt")
            } else {
                prefix
            };
            let prefix = if i >= 6 {
                prefix.replace("def\\word#1{#1}", "def\\word#1{#1 changed}")
            } else {
                prefix
            };
            let suffix = if span {
                "\\span Other\\cr"
            } else {
                "&Other\\cr"
            };
            let source = format!(
                "{prefix}First{}.{newline}\\par\n\\hbox{{$\\vcenter{{\\halign{{{template}&\\hskip3pt#\\hfil\\cr {construction}{suffix}}}}}$}}\\par\\immediate\\write1{{End:\\the\\count9}}\\end\n",
                " grows".repeat(i),
                newline = if i > 3 { "\n" } else { "" }
            );
            session
                .open_document(
                    "document",
                    i as u64 + 1,
                    Path::new("/project/document.tex"),
                    source.as_bytes().into(),
                )
                .unwrap();
            assert_eq!(
                settled(&mut session),
                RunProgress::Complete,
                "{:?}",
                session.engine().unwrap().core.diagnostics.errors
            );
            let stats = session.engine().unwrap().execution_reuse_stats();
            if [1, 3, 5, 7].contains(&i) {
                assert!(
                    stats.group_hits > 0 && stats.avoided_macros >= 24,
                    "{file} revision {i}: {stats:?}"
                );
            }
            let mut reference = settings.clone();
            reference.execution_reuse = false;
            compare_fresh_config(
                &mut session,
                source.as_bytes(),
                &[(file, bytes.clone())],
                reference,
            );
        }
    }
}

#[test]
fn alignment_material_reuse_rejects_cross_cell_continuations_and_recovers_after_cancel() {
    use retex_rs2::session::EditingSession;
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\hsize=180pt\\vsize=500pt\\def\\word#1{#1}\\let\\bgroup={\\let\\egroup=}\\count9=0\\immediate\\openout1=proof.txt\n";
    let body = format!(
        "\\halign{{#&#\\cr \\begingroup\\hbox{{{}}}\\global\\advance\\count9 by1\\immediate\\write1{{\\the\\count9}}\\endgroup&Other\\cr\\omit Omitted&Next\\cr}}\\par\\end\n",
        "\\word{saved} ".repeat(24)
    );
    let mut settings = config();
    settings.progressive_pages = true;
    settings.alignment_material_reuse = true;
    settings.material_region_admission = true;
    let mut session = EditingSession::new(settings.clone(), host(b"First")).unwrap();
    for revision in 1..=4 {
        let source = format!(
            "{prefix}First{}.\\par\n{body}",
            " grows".repeat(revision as usize)
        );
        session
            .open_document(
                "document",
                revision,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        if revision == 3 {
            let mut hit = false;
            for _ in 0..100_000 {
                assert_eq!(session.advance(1).unwrap(), RunProgress::Yielded);
                if session.engine().unwrap().execution_reuse_stats().group_hits > 0 {
                    hit = true;
                    break;
                }
            }
            assert!(hit);
            session.cancel("document", revision).unwrap();
            assert_eq!(session.host().pending_bytes(), 0);
        } else {
            assert_eq!(settled(&mut session), RunProgress::Complete);
            if revision > 1 {
                assert!(
                    session.engine().unwrap().execution_reuse_stats().group_hits > 0,
                    "{:?}",
                    session.engine().unwrap().execution_reuse_stats()
                );
            }
            let mut reference = settings.clone();
            reference.execution_reuse = false;
            compare_reuse_fresh(&mut session, source.as_bytes(), reference);
        }
    }
    // A closing group obtained from a template may move the interceptor from
    // body into suffix. Never import material across that scanner transition.
    let source = format!(
        "{prefix}Changed.\\par\n\\halign{{#\\endgroup&#\\cr \\begingroup\\word{{Cell}}&Other\\cr}}\\par\\end\n"
    );
    session
        .open_document(
            "document",
            5,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(
        settled(&mut session),
        RunProgress::Complete,
        "{:?}",
        session.engine().unwrap().core.diagnostics.errors
    );
    let mut reference = settings;
    reference.execution_reuse = false;
    compare_reuse_fresh(&mut session, source.as_bytes(), reference);
}

#[test]
fn material_admission_leaves_empty_stateful_groups_on_the_reference_path() {
    use retex_rs2::session::EditingSession;
    for admission in [false, true] {
        let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\count9=0\\def\\step{\\global\\advance\\count9 by1}\\immediate\\openout1=proof.txt\n";
        let suffix = format!(
            "\n\nHeading\\hbox{{}}\\begingroup{}\\immediate\\write1{{Middle:\\the\\count9}}\\endgroup Tail:\\the\\count9\\par\\end\n",
            "\\step ".repeat(32)
        );
        let mut settings = config();
        settings.progressive_pages = true;
        settings.stateful_paragraph_reuse = false;
        settings.material_region_admission = admission;
        let mut session = EditingSession::new(settings.clone(), host(b"First")).unwrap();
        for revision in 1..=3 {
            let source = format!(
                "{prefix}First{}.\\par{suffix}",
                " grows".repeat(revision as usize)
            );
            session
                .open_document(
                    "document",
                    revision,
                    Path::new("/project/document.tex"),
                    source.as_bytes().into(),
                )
                .unwrap();
            assert_eq!(settled(&mut session), RunProgress::Complete);
            if revision > 1 {
                let stats = session.engine().unwrap().execution_reuse_stats();
                if admission {
                    assert_eq!(stats.group_hits, 0, "{stats:?}");
                } else {
                    assert!(stats.group_hits > 0, "{stats:?}");
                }
            }
            let mut reference = settings.clone();
            reference.execution_reuse = false;
            compare_reuse_fresh(&mut session, source.as_bytes(), reference);
        }
    }
}

#[test]
fn physical_input_reuse_preserves_definitions_effects_and_source_file_observations() {
    use retex_rs2::session::EditingSession;
    use sha2::{Digest, Sha256};
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\hsize=180pt\\vsize=500pt\\count9=0\\def\\unused{Original}\\immediate\\openout1=proof.txt\n";
    let suffix = "\n\nStable \\begingroup\\input part-material.tex \\global\\advance\\count9 by1\\immediate\\write1{Middle:\\the\\count9}\\write1{Later:\\the\\count9}\\endgroup tail \\reusedword{Outside}.\\par\\end\n";
    let child = format!(
        "\\font\\temporary=cmti10 at9pt\\temporary\\def\\word#1{{#1}}\\global\\let\\reusedword\\word\\hbox{{{}}}\\global\\advance\\count9 by1\n\\endinput\nIgnored after endinput\n",
        "\\word{saved} ".repeat(32)
    );
    let mut bytes = child.as_bytes().to_vec();
    let h = host(b"First");
    h.apply(CatalogDelta {
        base_epoch: h.epoch(),
        new_epoch: h.epoch() + 1,
        policy_version: 0,
        changes: vec![put(Path::new("/project/part-material.tex"), bytes.clone())],
    })
    .unwrap();
    let mut settings = config();
    settings.progressive_pages = true;
    settings.stateful_paragraph_reuse = false;
    settings.input_execution_reuse = true;
    settings.material_region_admission = true;
    let mut session = EditingSession::new(settings.clone(), h).unwrap();
    for revision in 1..=7 {
        if revision == 4 {
            bytes = child.replace("saved", "changed").into_bytes();
            let epoch = session.host().epoch();
            let mut change = put(Path::new("/project/part-material.tex"), bytes.clone());
            if let ResourceChange::Put { version, .. } = &mut change {
                *version = 2;
            }
            session
                .update_resources(CatalogDelta {
                    base_epoch: epoch,
                    new_epoch: epoch + 1,
                    policy_version: 0,
                    changes: vec![change],
                })
                .unwrap();
        }
        let prefix = if revision >= 6 {
            prefix.replace("unused{Original}", "unused{Unrelated}")
        } else {
            prefix.to_string()
        };
        let source = format!(
            "{prefix}First{}.{}{suffix}",
            " grows".repeat(revision as usize),
            if revision >= 3 { "\n" } else { "" }
        );
        session
            .open_document(
                "document",
                revision,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(
            settled(&mut session),
            RunProgress::Complete,
            "{:?}",
            session.engine().unwrap().core.diagnostics.errors
        );
        let stats = session.engine().unwrap().execution_reuse_stats();
        if [2, 3, 5, 7].contains(&revision) {
            assert!(
                stats.group_hits > 0 && stats.avoided_macros >= 32,
                "revision {revision}: {stats:?}"
            );
        }
        assert_eq!(
            session
                .engine()
                .unwrap()
                .core
                .input
                .source_resources
                .get("/project/part-material.tex"),
            Some(&format!("{:x}", Sha256::digest(&bytes)))
        );
        let mut reference = settings.clone();
        reference.execution_reuse = false;
        compare_fresh_config(
            &mut session,
            source.as_bytes(),
            &[("part-material.tex", bytes.clone())],
            reference,
        );
    }
    let later_prefix = prefix.replace("unused{Original}", "unused{Unrelated}");
    let source = format!("{later_prefix}First{}.\n{suffix}", " grows".repeat(8));
    session
        .open_document(
            "document",
            8,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    let mut hit = false;
    for _ in 0..100_000 {
        assert_eq!(session.advance(1).unwrap(), RunProgress::Yielded);
        if session.engine().unwrap().execution_reuse_stats().group_hits > 0 {
            hit = true;
            break;
        }
    }
    assert!(hit);
    session.cancel("document", 8).unwrap();
    assert_eq!(session.host().pending_bytes(), 0);
    let source = format!("{later_prefix}First{}.\n{suffix}", " grows".repeat(9));
    session
        .open_document(
            "document",
            9,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let mut reference = settings;
    reference.execution_reuse = false;
    compare_fresh_config(
        &mut session,
        source.as_bytes(),
        &[("part-material.tex", bytes)],
        reference,
    );
}

#[test]
fn owned_input_reuse_tracks_eof_tokens_for_physical_and_pseudo_input() {
    use retex_rs2::session::EditingSession;
    for pseudo in [false, true] {
        let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\count9=0\\def\\word#1{#1}\\everyeof={\\global\\advance\\count9 by1}\\immediate\\openout1=proof.txt\n";
        let words = "\\word{saved} ".repeat(32);
        let operation = if pseudo {
            format!("\\scantokens{{\\hbox{{{words}}}}}")
        } else {
            "\\input part-material.tex ".to_string()
        };
        let suffix = format!(
            "\n\nStable \\begingroup{operation}\\global\\advance\\count9 by1\\endgroup tail.\\par\\immediate\\write1{{\\the\\count9}}\\end\n"
        );
        let child = format!("\\hbox{{{words}}}\n").into_bytes();
        let h = host(b"First");
        h.apply(CatalogDelta {
            base_epoch: h.epoch(),
            new_epoch: h.epoch() + 1,
            policy_version: 0,
            changes: vec![put(Path::new("/project/part-material.tex"), child.clone())],
        })
        .unwrap();
        let mut settings = config();
        settings.progressive_pages = true;
        settings.stateful_paragraph_reuse = false;
        settings.input_execution_reuse = true;
        settings.material_region_admission = true;
        let mut session = EditingSession::new(settings.clone(), h).unwrap();
        for revision in 1..=3 {
            let source = format!(
                "{prefix}First{}.\\par{suffix}",
                " grows".repeat(revision as usize)
            );
            session
                .open_document(
                    "document",
                    revision,
                    Path::new("/project/document.tex"),
                    source.as_bytes().into(),
                )
                .unwrap();
            assert_eq!(
                settled(&mut session),
                RunProgress::Complete,
                "{:?}",
                session.engine().unwrap().core.diagnostics.errors
            );
            if revision > 1 {
                let stats = session.engine().unwrap().execution_reuse_stats();
                assert!(stats.group_hits > 0, "pseudo={pseudo} {stats:?}");
            }
            let mut reference = settings.clone();
            reference.execution_reuse = false;
            compare_fresh_config(
                &mut session,
                source.as_bytes(),
                &[("part-material.tex", child.clone())],
                reference,
            );
        }
    }
}

#[test]
fn physical_input_reuse_respects_added_higher_priority_files_and_removal() {
    use retex_rs2::session::EditingSession;
    let prefix = "\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\count9=0\\immediate\\openout1=proof.txt\n";
    let suffix = "\n\nStable \\begingroup\\input m.tex \\endgroup tail.\\par\\immediate\\write1{\\the\\count9}\\end\n";
    let content = |name: &str, n: usize| {
        format!(
            "\\def\\word#1{{{name}#1}}\\hbox{{{}}}\\global\\advance\\count9 by{n}\\relax\n",
            "\\word{saved} ".repeat(32)
        )
        .into_bytes()
    };
    let second = content("Second", 1);
    let first = content("First", 2);
    let h = host(b"First");
    h.apply(CatalogDelta {
        base_epoch: h.epoch(),
        new_epoch: h.epoch() + 1,
        policy_version: 0,
        changes: vec![put(Path::new("/project/second/m.tex"), second.clone())],
    })
    .unwrap();
    let mut settings = config();
    settings.progressive_pages = true;
    settings.stateful_paragraph_reuse = false;
    settings.input_execution_reuse = true;
    settings.material_region_admission = true;
    settings.tex_input_roots = Some(vec!["/project/first".into(), "/project/second".into()]);
    let mut session = EditingSession::new(settings.clone(), h).unwrap();
    for revision in 1..=6 {
        if revision == 3 || revision == 5 {
            let change = if revision == 3 {
                put(Path::new("/project/first/m.tex"), first.clone())
            } else {
                ResourceChange::Remove(resource_path(Path::new("/project/first/m.tex")))
            };
            let epoch = session.host().epoch();
            session
                .update_resources(CatalogDelta {
                    base_epoch: epoch,
                    new_epoch: epoch + 1,
                    policy_version: 0,
                    changes: vec![change],
                })
                .unwrap();
        }
        let source = format!(
            "{prefix}First{}.\\par{suffix}",
            "i".repeat(revision as usize)
        );
        session
            .open_document(
                "document",
                revision,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(
            settled(&mut session),
            RunProgress::Complete,
            "{:?}",
            session.engine().unwrap().core.diagnostics.errors
        );
        if [2, 4, 6].contains(&revision) {
            let stats = session.engine().unwrap().execution_reuse_stats();
            assert!(stats.group_hits > 0, "{revision}: {stats:?}");
        }
        let resources = if revision == 3 || revision == 4 {
            vec![
                ("first/m.tex", first.clone()),
                ("second/m.tex", second.clone()),
            ]
        } else {
            vec![("second/m.tex", second.clone())]
        };
        let mut reference = settings.clone();
        reference.execution_reuse = false;
        compare_fresh_config(&mut session, source.as_bytes(), &resources, reference);
    }
}

#[test]
fn pseudo_input_reuse_invalidates_tokenization_printing_and_eof_dependencies() {
    use retex_rs2::session::EditingSession;
    let mut settings = config();
    settings.progressive_pages = true;
    settings.stateful_paragraph_reuse = false;
    settings.input_execution_reuse = true;
    settings.material_region_admission = true;
    let mut session = EditingSession::new(settings.clone(), host(b"First")).unwrap();
    for revision in 1..=9 {
        let word = if revision < 3 { "#1" } else { "Changed #1" };
        let (escape, catcode) = if revision < 5 { (92, 12) } else { (33, 0) };
        let eof = if revision < 7 { 1 } else { 7 };
        let source = format!(
            "\\pdfinfoomitdate=1\\pdftrailerid{{}}\\font\\body=cmr10 \\body\\hsize=1000pt\\vsize=500pt\\count9=0\\def\\word#1{{{word}}}\\escapechar={escape}\\catcode33={catcode}\\newlinechar=10\\everyeof={{\\global\\advance\\count9 by{eof}}}\\immediate\\openout1=proof.txt\nFirst{}.\\par\n\n{}Stable \\begingroup\\scantokens{{\\hbox{{{}}}\\global\\advance\\count9 by1}}\\endgroup tail.\\par\\immediate\\write1{{Final:\\the\\count9}}\\end\n",
            " grows".repeat(revision as usize),
            if revision == 9 { "\n" } else { "" },
            "\\word{saved} ".repeat(32),
        );
        session
            .open_document(
                "document",
                revision,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(
            settled(&mut session),
            RunProgress::Complete,
            "revision {revision}: {:?}",
            session.engine().unwrap().core.diagnostics.errors
        );
        if [2, 4, 6, 8, 9].contains(&revision) {
            let stats = session.engine().unwrap().execution_reuse_stats();
            assert!(stats.group_hits > 0, "revision {revision}: {stats:?}");
        }
        let mut reference = settings.clone();
        reference.execution_reuse = false;
        compare_reuse_fresh(&mut session, source.as_bytes(), reference);
    }
}

#[test]
fn checked_local_page_precedes_shipment_and_matches_uncached_engine() {
    checked_local_case(false, true, "");
    checked_local_case(false, false, "");
    checked_local_case(true, true, "");
    checked_local_case(true, false, "");
    checked_local_case(
        true,
        false,
        r"\output={\setbox0=\vbox{\unvbox255}\setbox1=\copy0\shipout\vbox{\box0\vskip20pt\box1}}",
    );
    checked_local_case(
        true,
        false,
        r"\output={\setbox0=\vsplit255 to 20pt\shipout\vbox{\box0\unvbox255}}",
    );
    checked_local_case(
        true,
        false,
        r"\output={\shipout\vbox{\pdfliteral page{q 0.5 g 0 0 800 800 re W n 1 0 0 1 5 7 cm}\unvbox255\pdfliteral page{Q}}}",
    );
}
fn checked_local_case(macro_end: bool, historical: bool, output: &str) {
    use retex_rs2::session::EditingSession;
    let source = b"\\pdfoutput=1\\hsize=240pt\\vsize=400pt\n\\par\nA plain initial line to establish a physical checkpoint.\nThis paragraph has ordinary words and spaces, with enough text to make several lines of useful material for the checked consumer. More plain words here complete the paragraph.\n\n\\end\n";
    let source = if macro_end {
        [
            b"\\let\\realend\\par\\def\\par{\\ifhmode\\unskip\\fi\\realend}\n".as_slice(),
            source.as_slice(),
        ]
        .concat()
    } else {
        source.to_vec()
    };
    let source = [output.as_bytes(), b"\n", source.as_slice()].concat();
    let mut cfg = config();
    cfg.exact_repro = historical;
    cfg.progressive_pages = true;
    cfg.exact_local_updates = true;
    cfg.exact_local_verify = macro_end;
    let mut session = EditingSession::new(cfg, host(&source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(
        settled(&mut session),
        RunProgress::Complete,
        "macro={macro_end} historical={historical} {:?}",
        session.engine().unwrap().core.failure
    );
    let mut outlines = Default::default();
    session.capture_page(1, &mut outlines).unwrap();
    let changed = String::from_utf8(source.to_vec())
        .unwrap()
        .replace("This paragraph", "Thiis paragraph")
        .into_bytes();
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            changed.clone().into(),
        )
        .unwrap();
    for _ in 0..100_000 {
        if session.early_page().is_some() {
            break;
        }
        if session.advance(1).unwrap() != RunProgress::Yielded {
            break;
        }
    }
    assert_eq!(
        session.early_page(),
        Some(1),
        "{}",
        serde_json::to_string_pretty(&session.reuse()).unwrap()
    );
    assert_eq!(session.engine().unwrap().completed_page_count(), 0);
    let early = session.capture_page(1, &mut outlines).unwrap();
    assert!(!early.complete);
    let mut cfg = config();
    cfg.exact_repro = historical;
    cfg.progressive_pages = true;
    cfg.execution_reuse = false;
    let mut oracle = EditingSession::new(cfg, host(&changed)).unwrap();
    oracle
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            changed.into(),
        )
        .unwrap();
    assert_eq!(settled(&mut oracle), RunProgress::Complete);
    let expected = oracle.capture_page(1, &mut outlines).unwrap();
    assert_eq!(
        early.pages[0].content_sha256,
        expected.pages[0].content_sha256
    );
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.export_pdf().unwrap(), oracle.export_pdf().unwrap());
}

#[test]
fn checked_local_content_inspection_rejects_copied_material_and_preserves_effects() {
    use retex_rs2::session::EditingSession;
    let source=include_str!("fixtures/exact-local-content-inspection.tex").replace("\\immediate\\write16{OBSERVED=\\the\\count0}","\\immediate\\openout7=observed.txt \\immediate\\write7{OBSERVED=\\the\\count0}\\immediate\\closeout7").replace("\\noindent A\\par\\penalty-10000\\end", "\\noindent\nA\n\n\\penalty-10000\\end");
    let mut cfg = config();
    cfg.progressive_pages = true;
    cfg.exact_local_updates = true;
    cfg.exact_local_verify = true;
    let mut session = EditingSession::new(cfg, host(source.as_bytes())).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    session.capture_page(1, &mut Default::default()).unwrap();
    let proof = session.reuse().local_updates;
    assert!(proof["candidates"].as_u64().unwrap() > 0, "{proof}");
    assert!(
        !proof["consumer_misses"].as_object().unwrap().is_empty(),
        "{proof}"
    );
    assert!(
        proof["consumers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["operation"] == "independent_copy"),
        "{proof}"
    );
    assert_eq!(proof["retained_bases"], 0);
    assert_eq!(
        session
            .host()
            .read(Path::new("/project/observed.txt"))
            .unwrap(),
        b"OBSERVED=0\n"
    );
    let changed = source.replace("\nA\n", "\nAi\n");
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            changed.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.early_page(), None);
    assert_eq!(
        session
            .host()
            .read(Path::new("/project/observed.txt"))
            .unwrap(),
        b"OBSERVED=1\n"
    );
    compare_fresh(&mut session, changed.as_bytes());
}

#[test]
fn checked_local_unsupported_edits_use_the_ordinary_engine() {
    use retex_rs2::session::EditingSession;
    let source = "\\pdfoutput=1\\hsize=240pt\\vsize=400pt\n\\par\nAn unchanged physical introductory line establishes the boundary.\nThis paragraph has ordinary words and spaces, with enough text to make several lines of useful material for the checked consumer. More plain words here complete the paragraph.\n\n\\end\n";
    let changes = [
        ("font", source.replace("\\hsize=240pt", "\\font\\body=cmtt10 \\body\\hsize=240pt")),
        ("definition", source.replace("\\hsize=240pt", "\\def\\later{changed}\\hsize=240pt")),
        ("dimension", source.replace("\\hsize=240pt", "\\hsize=241pt")),
        ("wrapping", source.replace("This paragraph", &"Another long ordinary sentence with many extra words. ".repeat(20))),
        ("mark", source.replace("This paragraph", "This\\mark{changed} paragraph")),
        ("counter", source.replace("This paragraph", "This\\global\\advance\\count9 by1 paragraph")),
        ("write", source.replace("This paragraph", "This\\immediate\\openout7=observed.txt\\immediate\\write7{once}\\immediate\\closeout7 paragraph")),
        ("link", source.replace("This paragraph", "This\\pdfstartlink attr{/Border[0 0 0]} user{/Subtype/Link /A<</S/URI/URI(example)>>} paragraph\\pdfendlink")),
        ("color", source.replace("This paragraph", "This\\pdfliteral{0.5 g} paragraph")),
        ("clip", source.replace("This paragraph", "This\\pdfliteral{q 0 0 100 100 re W n} paragraph\\pdfliteral{Q}")),
        ("transform", source.replace("This paragraph", "This\\pdfliteral{q 1 0 0 1 10 0 cm} paragraph\\pdfliteral{Q}")),
        ("source lines", source.replace("This paragraph", "This\nparagraph")),
        ("later source", source.replace("\\end", "\\count9=7\\end").replace("This paragraph", "Thiis paragraph")),
    ];
    for (name, changed) in changes {
        let mut cfg = config();
        cfg.exact_local_updates = true;
        cfg.exact_local_verify = true;
        cfg.progressive_pages = true;
        let mut session = EditingSession::new(cfg, host(source.as_bytes())).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete, "{name}");
        session.capture_page(1, &mut Default::default()).unwrap();
        assert!(
            session.reuse().local_updates["retained_bases"]
                .as_u64()
                .unwrap()
                > 0,
            "{name}"
        );
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                changed.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete, "{name}");
        assert_eq!(
            session.early_page(),
            None,
            "{name}: {}",
            session.reuse().local_updates
        );
        compare_fresh(&mut session, changed.as_bytes());
    }
}

#[test]
fn checked_local_repeated_edits_cancel_undo_redo_and_history_eviction() {
    use retex_rs2::session::EditingSession;
    let source = "\\pdfoutput=1\\hsize=240pt\\vsize=400pt\n\\par\nAn unchanged physical introductory line establishes the boundary.\nThis paragraph has ordinary words and spaces, with enough text to make several lines of useful material for the checked consumer. More plain words here complete the paragraph.\n\n\\immediate\\openout7=observed.txt\\immediate\\write7{once}\\immediate\\closeout7\\end\n";
    let mut cfg = config();
    cfg.exact_local_updates = true;
    cfg.exact_local_verify = true;
    cfg.progressive_pages = true;
    let mut session = EditingSession::new(cfg, host(source.as_bytes())).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    session.capture_page(1, &mut Default::default()).unwrap();
    for (revision, word) in [(2, "Thiis"), (3, "Thiiis")] {
        let changed = source.replace("This paragraph", &format!("{word} paragraph"));
        session
            .open_document(
                "document",
                revision,
                Path::new("/project/document.tex"),
                changed.as_bytes().into(),
            )
            .unwrap();
        for _ in 0..100_000 {
            if session.early_page().is_some() {
                break;
            }
            assert_eq!(session.advance(16).unwrap(), RunProgress::Yielded);
        }
        assert_eq!(
            session.early_page(),
            Some(1),
            "{}",
            session.reuse().local_updates
        );
        assert_eq!(session.engine().unwrap().completed_page_count(), 0);
        assert!(session.export_pdf().is_err());
    }
    session.cancel("document", 3).unwrap();
    assert_eq!(session.early_page(), None);
    assert!(session.export_pdf().is_err());
    session
        .open_document(
            "document",
            4,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.early_page(), None);
    compare_fresh(&mut session, source.as_bytes());
    let redo = source.replace("This paragraph", "Thiis paragraph");
    session
        .open_document(
            "document",
            5,
            Path::new("/project/document.tex"),
            redo.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.early_page(), Some(1));
    assert_eq!(
        session
            .host()
            .read(Path::new("/project/observed.txt"))
            .unwrap(),
        b"once\n"
    );
    compare_fresh(&mut session, redo.as_bytes());
    // The input-history bound is a proof limit, not permission to use an
    // uncheckable old recipe. Rapid revisions deliberately age out its witness.
    let mut last = String::new();
    for revision in 6..=72 {
        last = source.replace(
            "This paragraph",
            &format!("This{} paragraph", "i".repeat((revision % 2 + 1) as usize)),
        );
        session
            .open_document(
                "document",
                revision,
                Path::new("/project/document.tex"),
                last.as_bytes().into(),
            )
            .unwrap();
    }
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.early_page(), None);
    compare_fresh(&mut session, last.as_bytes());
}

#[test]
fn checked_local_unobserved_last_line_keeps_current_revision_state() {
    use retex_rs2::session::EditingSession;
    let source = "\\pdfoutput=1\\hsize=1000pt\\vsize=400pt\n\\par\nAn unchanged introductory line establishes the boundary.\nThis paragraph ends with ordinary material.\n\n\\end\n";
    let changed = source.replace("ordinary material", "ordinary miaterial");
    let mut cfg = config();
    cfg.exact_local_updates = true;
    cfg.exact_local_verify = true;
    cfg.progressive_pages = true;
    cfg.exact_repro = false;
    let mut session = EditingSession::new(cfg.clone(), host(source.as_bytes())).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    session.capture_page(1, &mut Default::default()).unwrap();
    let old_extent = session.engine().unwrap().math.last_line();
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            changed.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(
        session.early_page(),
        Some(1),
        "{}",
        session.reuse().local_updates
    );
    assert_ne!(old_extent, session.engine().unwrap().math.last_line());
    cfg.execution_reuse = false;
    compare_fresh_config(&mut session, changed.as_bytes(), &[], cfg);
}

#[test]
fn checked_local_catalog_changes_reject_even_unqueried_inputs() {
    use retex_rs2::session::EditingSession;
    let source = "\\pdfoutput=1\\hsize=240pt\\vsize=400pt\n\\par\nAn unchanged introductory line establishes the boundary.\nThis paragraph has ordinary words and spaces, with enough text to make several lines of useful material for the checked consumer. More plain words here complete the paragraph.\n\n\\end\n";
    for name in [
        "part.tex",
        "document.aux",
        "settings.sty",
        "font-resource.bin",
    ] {
        let mut cfg = config();
        cfg.progressive_pages = true;
        cfg.exact_local_updates = true;
        cfg.exact_local_verify = true;
        let mut session = EditingSession::new(cfg, host(source.as_bytes())).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        session.capture_page(1, &mut Default::default()).unwrap();
        let path = Path::new("/project").join(name);
        let identity = session
            .host()
            .resource_identity(&resource_path(&path))
            .unwrap();
        let mut change = put(&path, b"Changed independent input\n".to_vec());
        if let (
            Some((id, version)),
            ResourceChange::Put {
                id: out_id,
                version: out_version,
                ..
            },
        ) = (identity, &mut change)
        {
            *out_id = id;
            *out_version = version + 1;
        }
        session
            .update_resources(CatalogDelta {
                base_epoch: session.host().epoch(),
                new_epoch: session.host().epoch() + 1,
                policy_version: session.host().policy_version(),
                changes: vec![change],
            })
            .unwrap();
        let changed = source.replace("This paragraph", "Thiis paragraph");
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                changed.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        assert_eq!(session.early_page(), None, "{name}");
        compare_fresh_resources(
            &mut session,
            changed.as_bytes(),
            &[(name, b"Changed independent input\n".to_vec())],
        );
    }
}

#[test]
fn checked_local_badness_used_for_state_rejects_a_differing_value() {
    use retex_rs2::session::EditingSession;
    for observe in [false, true] {
        let source = format!(
            "\\pdfoutput=1\\font\\f=cmr10 \\f\\hsize=500pt\\vsize=400pt\\parfillskip=0pt plus100pt\\hbadness=10000\n\\par\nAn unchanged introductory line establishes the boundary.\nThis paragraph ends with ordinary material.\n\n{}\\end\n",
            if observe { "\\count9=\\badness\n" } else { "" }
        );
        let changed = source.replace("ordinary material", "ordinary miaterial");
        let mut cfg = config();
        cfg.exact_local_updates = true;
        cfg.exact_local_verify = true;
        cfg.progressive_pages = true;
        cfg.exact_repro = false;
        let mut session = EditingSession::new(cfg.clone(), host(source.as_bytes())).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        session.capture_page(1, &mut Default::default()).unwrap();
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                changed.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        assert_eq!(
            session.early_page(),
            if observe { None } else { Some(1) },
            "observe={observe}: {}",
            session.reuse().local_updates
        );
        if observe {
            assert!(
                session.reuse().local_updates["prefix_misses"]
                    .get("exact-local:paragraph_badness_observation")
                    .is_some(),
                "{}",
                session.reuse().local_updates
            );
        }
        cfg.execution_reuse = false;
        compare_fresh_config(&mut session, changed.as_bytes(), &[], cfg);
    }
}

#[test]
fn provisional_paragraph_is_independent_of_exact_proof_and_never_engine_state() {
    use retex_rs2::session::EditingSession;
    let original=b"\\pdfinfoomitdate=1 \\pdftrailerid{}\\hsize=150pt\nThis paragraph has ordinary text for editing and wrapping on several lines.\n\nFollowing material stays retained until shipment.\n\n\\end";
    for exact in [false, true] {
        let mut cfg = config();
        cfg.exact_repro = false;
        cfg.progressive_pages = true;
        cfg.paragraph_preview = true;
        cfg.paragraph_preview_timings = true;
        cfg.exact_local_updates = exact;
        cfg.exact_local_verify = exact;
        let mut session = EditingSession::new(cfg, host(original)).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                original.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        let mut outlines = Default::default();
        let mut images = Default::default();
        let scene = session.capture_page(1, &mut outlines).unwrap();
        assert!(!session.acknowledge_paragraph_page("document", 1, 1, 1, "unpublished-hash"));
        assert!(session.acknowledge_paragraph_page(
            "document",
            1,
            1,
            1,
            &scene.pages[0].content_sha256
        ));
        assert!(
            !scene.pages[0]
                .provisional_regions
                .as_ref()
                .unwrap()
                .is_empty()
        );
        let mut source = String::from_utf8(original.to_vec()).unwrap();
        for (revision, insert) in [
            (2, " "),
            (3, "?"),
            (
                4,
                " wider words force the paragraph to wrap to additional lines ",
            ),
        ] {
            let at = source.find("ordinary").unwrap() + "ordinary".len();
            source.insert_str(at, insert);
            session
                .open_document(
                    "document",
                    revision,
                    Path::new("/project/document.tex"),
                    source.as_bytes().into(),
                )
                .unwrap();
            let mut fragment = None;
            for _ in 0..100_000 {
                let progress = session.advance(1).unwrap();
                if session.paragraph_preview_status()["available"] == true {
                    assert_eq!(session.engine().unwrap().completed_page_count(), 0);
                    assert!(session.export_pdf().is_err());
                    fragment = session
                        .take_paragraph_preview(&mut outlines, &mut images)
                        .unwrap();
                    break;
                }
                if progress != RunProgress::Yielded {
                    break;
                }
            }
            let fragment =
                fragment.unwrap_or_else(|| panic!("{}", session.paragraph_preview_status()));
            assert!(fragment.source_bounds[0] <= at);
            assert!(fragment.source_bounds[1] >= at + insert.len());
            assert!(fragment.source_bounds[1] <= source.len());
            assert!(!fragment.preview.complete);
            assert_eq!(fragment.base_revision, 1);
            assert!(!fragment.preview.fonts.is_empty());
            if insert == "?" {
                assert!(
                    fragment
                        .preview
                        .fonts
                        .values()
                        .any(|f| f.widths.contains_key(&63))
                );
            }
            // Two further edits replace live unfinished computation. The old
            // authoritative placement remains the base; no preview is captured.
        }
        assert_eq!(settled(&mut session), RunProgress::Complete);
        let final_scene = session.capture_page(1, &mut outlines).unwrap();
        let mut cfg = config();
        cfg.exact_repro = false;
        cfg.progressive_pages = true;
        cfg.execution_reuse = false;
        let mut oracle = EditingSession::new(cfg, host(source.as_bytes())).unwrap();
        oracle
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut oracle), RunProgress::Complete);
        let expected = oracle.capture_page(1, &mut outlines).unwrap();
        assert_eq!(
            final_scene.pages[0].content_sha256,
            expected.pages[0].content_sha256
        );
        assert_eq!(session.export_pdf().unwrap(), oracle.export_pdf().unwrap());
    }
}

#[test]
fn provisional_paragraph_keeps_graphics_context_and_does_not_run_output_effects() {
    use retex_rs2::session::EditingSession;
    let source=b"\\pdfinfoomitdate=1 \\pdftrailerid{}\\hsize=150pt\\immediate\\openout7=observed.txt\n\\output={\\immediate\\write7{page}\\shipout\\hbox{\\pdfliteral direct{q 0.2 0.3 0.4 rg 1 0.1 0 1 0 0 cm 0 0 600 800 re W n}\\box255\\pdfliteral direct{Q}}}\nThis paragraph has ordinary text to edit and wrap across several lines.\n\nFollowing material.\n\n\\penalty-10000\\immediate\\closeout7\\end";
    let mut cfg = config();
    cfg.exact_repro = false;
    cfg.progressive_pages = true;
    cfg.paragraph_preview = true;
    let mut session = EditingSession::new(cfg, host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let mut outlines = Default::default();
    let scene = session.capture_page(1, &mut outlines).unwrap();
    assert!(session.acknowledge_paragraph_page(
        "document",
        1,
        1,
        1,
        &scene.pages[0].content_sha256
    ));
    let changed = String::from_utf8(source.to_vec())
        .unwrap()
        .replace("ordinary", "ordinary?");
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            changed.as_bytes().into(),
        )
        .unwrap();
    let mut fragment = None;
    for _ in 0..100_000 {
        let progress = session.advance(1).unwrap();
        if session.paragraph_preview_status()["available"] == true {
            fragment = session
                .take_paragraph_preview(&mut outlines, &mut Default::default())
                .unwrap();
            break;
        }
        if progress != RunProgress::Yielded {
            break;
        }
    }
    let fragment = fragment.unwrap_or_else(|| panic!("{}", session.paragraph_preview_status()));
    assert!(fragment.restore_count > 0);
    assert!(fragment.prefix.iter().any(|op| op.trim() == "W"));
    assert_eq!(session.engine().unwrap().completed_page_count(), 0);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(
        session
            .host()
            .read(Path::new("/project/observed.txt"))
            .unwrap(),
        b"page\n"
    );
    // A retained effect node in the paragraph has no provisional paint contract.
    let unsupported = changed.replace("ordinary?", "ordinary?\\write7{delayed}");
    session
        .open_document(
            "document",
            3,
            Path::new("/project/document.tex"),
            unsupported.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.paragraph_preview_status()["published"], false);
}

#[test]
fn provisional_paint_observer_preserves_existing_exact_local_certification() {
    use retex_rs2::session::EditingSession;
    let source=b"\n\\pdfoutput=1\\hsize=240pt\\vsize=400pt\n\\par\nA plain initial line to establish a physical checkpoint.\nThis paragraph has ordinary words and spaces, with enough text to make several lines of useful material for the checked consumer. More plain words here complete the paragraph.\n\n\\end\n";
    for historical in [false, true] {
        let mut cfg = config();
        cfg.exact_repro = historical;
        cfg.progressive_pages = true;
        cfg.exact_local_updates = true;
        cfg.exact_local_verify = true;
        cfg.paragraph_preview = true;
        let mut session = EditingSession::new(cfg, host(source)).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        let mut outlines = Default::default();
        let scene = session.capture_page(1, &mut outlines).unwrap();
        assert!(session.acknowledge_paragraph_page(
            "document",
            1,
            1,
            1,
            &scene.pages[0].content_sha256
        ));
        let changed = String::from_utf8(source.to_vec())
            .unwrap()
            .replace("This paragraph", "Thiis paragraph");
        session
            .open_document(
                "document",
                2,
                Path::new("/project/document.tex"),
                changed.as_bytes().into(),
            )
            .unwrap();
        let mut provisional = false;
        for _ in 0..100_000 {
            let progress = session.advance(1).unwrap();
            if session.paragraph_preview_status()["available"] == true {
                provisional |= session
                    .take_paragraph_preview(&mut outlines, &mut Default::default())
                    .unwrap()
                    .is_some();
            }
            if session.early_page().is_some() || progress != RunProgress::Yielded {
                break;
            }
        }
        assert!(provisional);
        assert_eq!(
            session.early_page(),
            Some(1),
            "{}",
            session.reuse().local_updates
        );
        assert_eq!(session.engine().unwrap().completed_page_count(), 0);
        assert_eq!(settled(&mut session), RunProgress::Complete);
    }
}

#[test]
fn working_auxiliary_revisions_resume_resolved_checkpoints_without_publishing_outputs() {
    use retex_rs2::session::EditingSession;
    let source = String::from_utf8(SETTLING_COUNTER.to_vec())
        .unwrap()
        .replace(
            "\\the\\count7\\par",
            "First paragraph.\\par\nOriginal body. \\the\\count7\\par",
        );
    let mut cfg = config();
    cfg.progressive_pages = true;
    cfg.persistent_auxiliary_inputs = true;
    cfg.exact_local_updates = true;
    cfg.exact_local_verify = true;
    let mut session = EditingSession::new(cfg, host(source.as_bytes())).unwrap();
    let main = Path::new("/project/document.tex");
    let aux = PathBuf::from("/project/notes.aux");
    session
        .open_document("document", 1, main, source.as_bytes().into())
        .unwrap();
    session
        .begin_settling("document", 1, std::slice::from_ref(&aux), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.settling().unwrap().pass, 3);
    let generation = session.settling().unwrap().output_generation.unwrap();
    let edited = source.replace("Original", "Changed!");
    session
        .open_document("document", 2, main, edited.as_bytes().into())
        .unwrap();
    assert_eq!(session.reuse().auxiliary_input_generation, Some(generation));
    assert!(session.reuse().restored_offset > 0);
    assert!(session.reuse().auxiliary_checkpoint_reuses > 0);
    session
        .begin_settling("document", 2, std::slice::from_ref(&aux), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.settling().unwrap().pass, 1);
    assert_eq!(
        session.settling().unwrap().generations[0].inputs[0].sha256,
        session.settling().unwrap().generations[0].outputs[0].sha256
    );
    assert_eq!(session.host().read(&aux).unwrap(), b"2\n");
    assert!(session.host().committed(&aux).unwrap().is_none());
    compare_fresh_resources(
        &mut session,
        edited.as_bytes(),
        &[("notes.aux", b"2\n".to_vec())],
    );
    // A further revision can reuse the same input generation even though its
    // source version changed. The source prefix still decides where to resume.
    let edited_again = edited.replace("Changed!", "Another!");
    session
        .open_document("document", 3, main, edited_again.as_bytes().into())
        .unwrap();
    session
        .begin_settling("document", 3, std::slice::from_ref(&aux), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.settling().unwrap().pass, 1);
    compare_fresh_resources(
        &mut session,
        edited_again.as_bytes(),
        &[("notes.aux", b"2\n".to_vec())],
    );
}

#[test]
fn working_auxiliary_saves_complete_passes_and_rejects_partial_or_failed_writes() {
    use retex_rs2::session::EditingSession;
    let source = r"\pdfinfoomitdate=1\pdftrailerid{}
\count7=0 \openin1=notes.aux \ifeof1\else\read1 to\saved \count7=\saved\relax\fi\closein1
\immediate\openout1=notes.aux \immediate\write1{7}\immediate\closeout1
First paragraph.\par
Original body. Count: \the\count7\par\end
";
    let mut cfg = config();
    cfg.progressive_pages = true;
    cfg.persistent_auxiliary_inputs = true;
    let mut session = EditingSession::new(cfg, host(source.as_bytes())).unwrap();
    let main = Path::new("/project/document.tex");
    let aux = PathBuf::from("/project/notes.aux");
    session
        .open_document("document", 1, main, source.as_bytes().into())
        .unwrap();
    session
        .begin_settling("document", 1, std::slice::from_ref(&aux), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let first_generation = session.settling().unwrap().output_generation.unwrap();
    let changed = source.replace("{7}", "{8}");
    session
        .open_document("document", 2, main, changed.as_bytes().into())
        .unwrap();
    session
        .begin_settling("document", 2, std::slice::from_ref(&aux), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.settling().unwrap().pass, 2);
    let generation = session.settling().unwrap().output_generation.unwrap();
    assert!(generation > first_generation);
    compare_fresh_resources(
        &mut session,
        changed.as_bytes(),
        &[("notes.aux", b"8\n".to_vec())],
    );
    let unfinished = changed.replace("{8}", "{9}");
    session
        .open_document("document", 3, main, unfinished.as_bytes().into())
        .unwrap();
    session
        .begin_settling("document", 3, std::slice::from_ref(&aux), 4)
        .unwrap();
    for _ in 0..100_000 {
        assert_eq!(session.advance(1).unwrap(), RunProgress::Yielded);
        if session.settling().unwrap().pass == 2 {
            break;
        }
    }
    assert_eq!(session.settling().unwrap().pass, 2);
    let completed_generation = session.settling().unwrap().output_generation.unwrap();
    assert!(completed_generation > generation);
    let repair = changed.replace("Original", "Repaired");
    session
        .open_document("document", 4, main, repair.as_bytes().into())
        .unwrap();
    assert_eq!(
        session.reuse().auxiliary_input_generation,
        Some(completed_generation)
    );
    assert_eq!(session.reuse().auxiliary_input_revision, Some(3));
    assert_eq!(session.reuse().auxiliary_input_pass, Some(1));
    assert_eq!(session.reuse().auxiliary_input_stable, Some(false));
    session
        .begin_settling("document", 4, std::slice::from_ref(&aux), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.settling().unwrap().pass, 2);
    let generation = session.settling().unwrap().output_generation.unwrap();
    compare_fresh_resources(
        &mut session,
        repair.as_bytes(),
        &[("notes.aux", b"8\n".to_vec())],
    );
    let failed = unfinished.replace("Original body.", "\\workingAuxiliaryUndefined");
    session
        .open_document("document", 5, main, failed.as_bytes().into())
        .unwrap();
    session
        .begin_settling("document", 5, std::slice::from_ref(&aux), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Failed);
    session
        .open_document("document", 6, main, repair.as_bytes().into())
        .unwrap();
    assert_eq!(session.reuse().auxiliary_input_generation, Some(generation));
    session
        .begin_settling("document", 6, std::slice::from_ref(&aux), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.settling().unwrap().pass, 1);
    session
        .open_document("document", 7, main, unfinished.as_bytes().into())
        .unwrap();
    session
        .begin_settling("document", 7, std::slice::from_ref(&aux), 1)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.settling().unwrap().status, "limit");
    let limited_generation = session.settling().unwrap().output_generation.unwrap();
    assert!(limited_generation > generation);
    session
        .open_document("document", 8, main, repair.as_bytes().into())
        .unwrap();
    assert_eq!(
        session.reuse().auxiliary_input_generation,
        Some(limited_generation)
    );
    assert_eq!(session.reuse().auxiliary_input_stable, Some(false));
    session
        .begin_settling("document", 8, std::slice::from_ref(&aux), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.settling().unwrap().pass, 2);
    compare_fresh_resources(
        &mut session,
        repair.as_bytes(),
        &[("notes.aux", b"8\n".to_vec())],
    );
    assert!(session.host().committed(&aux).unwrap().is_none());
}

#[test]
fn working_auxiliary_before_first_settlement_keeps_completed_inputs_and_private_checkpoints() {
    use retex_rs2::session::EditingSession;
    use sha2::{Digest, Sha256};
    let source = String::from_utf8(SETTLING_COUNTER.to_vec())
        .unwrap()
        .replace(
            "\\the\\count7\\par\\end",
            "First paragraph. Count: \\the\\count7\\par\nOriginal body.\\par\\end",
        );
    let mut cfg = config();
    cfg.persistent_auxiliary_inputs = true;
    cfg.progressive_pages = true;
    let mut session = EditingSession::new(cfg, host(source.as_bytes())).unwrap();
    let main = Path::new("/project/document.tex");
    let aux = PathBuf::from("/project/notes.aux");
    session
        .open_document("document", 1, main, source.as_bytes().into())
        .unwrap();
    session
        .begin_settling("document", 1, std::slice::from_ref(&aux), 4)
        .unwrap();
    for _ in 0..100_000 {
        assert_eq!(session.advance(1).unwrap(), RunProgress::Yielded);
        if session.settling().unwrap().pass == 2 {
            break;
        }
    }
    assert_eq!(session.settling().unwrap().status, "running");
    assert_eq!(session.settling().unwrap().pass, 2);
    let completed = session.settling().unwrap().output_generation.unwrap();
    // Pass two writes different bytes and captures a body checkpoint, but has
    // not finished. The saved input must remain pass one's complete file.
    for _ in 0..100_000 {
        assert_eq!(session.advance(1).unwrap(), RunProgress::Yielded);
        if session.engine().unwrap().layout.paragraph_count > 0 {
            break;
        }
    }
    assert_eq!(session.host().read(&aux).unwrap(), b"2\n");
    assert_eq!(session.settling().unwrap().pass, 2);
    assert_eq!(
        session.settling().unwrap().output_generation,
        Some(completed)
    );
    assert!(session.host().committed(&aux).unwrap().is_none());
    let at = source.find("Original").unwrap();
    let edited = source.replace("Original", "Changed!");
    session
        .apply_edit("document", 1, at, at + 8, b"Changed!", 2)
        .unwrap();
    assert_eq!(session.reuse().auxiliary_input_generation, Some(completed));
    assert_eq!(session.reuse().auxiliary_input_revision, Some(1));
    assert_eq!(session.reuse().auxiliary_input_pass, Some(1));
    assert_eq!(session.reuse().auxiliary_input_stable, Some(false));
    assert!(session.reuse().restored_auxiliary_checkpoint);
    session
        .begin_settling("document", 2, std::slice::from_ref(&aux), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let result = session.settling().unwrap();
    assert_eq!(result.pass, 2);
    assert_eq!(result.status, "stable");
    assert_eq!(
        result.generations[0].inputs[0].sha256,
        Some(format!("{:x}", Sha256::digest(b"1\n")))
    );
    // Compare the final pass to fresh execution with its own input bytes;
    // the \saved macro intentionally still records the value it read.
    compare_fresh_resources(
        &mut session,
        edited.as_bytes(),
        &[("notes.aux", b"2\n".to_vec())],
    );
    assert!(session.host().committed(&aux).unwrap().is_none());
}

#[test]
fn working_auxiliary_keeps_the_last_complete_pass_when_a_later_pass_fails() {
    use retex_rs2::session::EditingSession;
    use sha2::{Digest, Sha256};
    let source = String::from_utf8(SETTLING_COUNTER.to_vec())
        .unwrap()
        .replace(
            "\\the\\count7\\par\\end",
            "\\ifnum\\count7=2 \\workingAuxiliaryUndefined\\fi \\the\\count7\\par\\end",
        );
    let mut cfg = config();
    cfg.persistent_auxiliary_inputs = true;
    cfg.progressive_pages = true;
    let mut session = EditingSession::new(cfg, host(source.as_bytes())).unwrap();
    let main = Path::new("/project/document.tex");
    let aux = PathBuf::from("/project/notes.aux");
    session
        .open_document("document", 1, main, source.as_bytes().into())
        .unwrap();
    session
        .begin_settling("document", 1, std::slice::from_ref(&aux), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Failed);
    assert_eq!(session.settling().unwrap().pass, 2);
    let completed = session.settling().unwrap().output_generation.unwrap();
    session
        .open_document("document", 2, main, SETTLING_COUNTER.into())
        .unwrap();
    assert_eq!(session.reuse().auxiliary_input_generation, Some(completed));
    assert_eq!(session.reuse().auxiliary_input_pass, Some(1));
    assert_eq!(session.reuse().auxiliary_input_stable, Some(false));
    session
        .begin_settling("document", 2, std::slice::from_ref(&aux), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(
        session.settling().unwrap().generations[0].inputs[0].sha256,
        Some(format!("{:x}", Sha256::digest(b"1\n")))
    );
    compare_fresh_resources(
        &mut session,
        SETTLING_COUNTER,
        &[("notes.aux", b"2\n".to_vec())],
    );
    assert!(session.host().committed(&aux).unwrap().is_none());
}

#[test]
fn working_auxiliary_resource_changes_and_new_documents_invalidate_the_seed() {
    use retex_rs2::session::EditingSession;
    let mut cfg = config();
    cfg.persistent_auxiliary_inputs = true;
    cfg.progressive_pages = true;
    let mut session = EditingSession::new(cfg, host(SETTLING_COUNTER)).unwrap();
    let main = Path::new("/project/document.tex");
    let aux = PathBuf::from("/project/notes.aux");
    session
        .open_document("document", 1, main, SETTLING_COUNTER.into())
        .unwrap();
    session
        .begin_settling("document", 1, std::slice::from_ref(&aux), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    session
        .update_resources(CatalogDelta {
            base_epoch: session.host().epoch(),
            new_epoch: session.host().epoch() + 1,
            policy_version: session.host().policy_version(),
            changes: vec![put(&aux, b"9\n".to_vec())],
        })
        .unwrap();
    session
        .open_document("document", 2, main, SETTLING_COUNTER.into())
        .unwrap();
    session
        .begin_settling("document", 2, std::slice::from_ref(&aux), 4)
        .unwrap();
    assert_eq!(session.reuse().auxiliary_input_generation, None);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert_eq!(session.host().read(&aux).unwrap(), b"9\n");
    compare_fresh_resources(
        &mut session,
        SETTLING_COUNTER,
        &[("notes.aux", b"9\n".to_vec())],
    );
    session
        .open_document("different", 1, main, SETTLING_COUNTER.into())
        .unwrap();
    assert_eq!(session.reuse().auxiliary_input_generation, None);
    session
        .begin_settling("different", 1, std::slice::from_ref(&aux), 4)
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    session
        .open_document("different", 2, main, SETTLING_COUNTER.into())
        .unwrap();
    assert!(session.reuse().auxiliary_input_generation.is_some());
    // A caller changing the declared input set must not keep the prior seed,
    // even when the additional path is absent and the source is identical.
    session
        .begin_settling(
            "different",
            2,
            &[aux, PathBuf::from("/project/extra.toc")],
            4,
        )
        .unwrap();
    assert_eq!(session.reuse().auxiliary_input_generation, None);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh_resources(
        &mut session,
        SETTLING_COUNTER,
        &[("notes.aux", b"9\n".to_vec())],
    );
}

#[test]
fn working_auxiliary_seed_excludes_other_outputs_and_prefix_writes_run_once() {
    use retex_rs2::session::EditingSession;
    let source = r"\pdfinfoomitdate=1\pdftrailerid{}
\openin1=notes.aux \ifeof1\else\read1 to\saved \fi\closein1
\immediate\openout1=notes.aux \immediate\write1{ready}\immediate\closeout1
\immediate\openout2=side.txt \immediate\write2{prefix}
First paragraph.\par
Original body.\par
\immediate\write2{suffix}\immediate\closeout2\end
";
    let mut cfg = config();
    cfg.persistent_auxiliary_inputs = true;
    cfg.progressive_pages = true;
    let mut session = EditingSession::new(cfg, host(source.as_bytes())).unwrap();
    let main = Path::new("/project/document.tex");
    let aux = PathBuf::from("/project/notes.aux");
    for (revision, word) in [(1, "Original"), (2, "Changed!"), (3, "Another!")] {
        let bytes = source.replace("Original", word);
        session
            .open_document("document", revision, main, bytes.as_bytes().into())
            .unwrap();
        session
            .begin_settling("document", revision, std::slice::from_ref(&aux), 4)
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        if revision > 1 {
            assert_eq!(session.settling().unwrap().pass, 1);
        }
        assert_eq!(
            session.host().read(Path::new("/project/side.txt")).unwrap(),
            b"prefix\nsuffix\n"
        );
        assert!(
            session
                .host()
                .committed(Path::new("/project/side.txt"))
                .unwrap()
                .is_none()
        );
        compare_fresh_resources(
            &mut session,
            bytes.as_bytes(),
            &[("notes.aux", b"ready\n".to_vec())],
        );
    }
}

fn source_transaction(
    session: &retex_rs2::session::EditingSession,
    revision: u64,
    changes: Vec<(&str, usize, usize, &[u8])>,
) -> retex_rs2::session::SourceTransaction {
    use retex_rs2::session::{SourceChange, SourceReplacement, SourceTransaction};
    SourceTransaction {
        project: "document".into(),
        document: "document".into(),
        base_revision: session.tag().unwrap().revision,
        revision,
        catalog_epoch: session.host().epoch(),
        changes: changes
            .into_iter()
            .map(|(path, start, end, bytes)| {
                let (id, version) = session
                    .host()
                    .resource_identity(&resource_path(Path::new(path)))
                    .unwrap()
                    .unwrap();
                SourceChange {
                    id: id.0,
                    path: path.into(),
                    expected_version: version,
                    version: version + 1,
                    replacements: vec![SourceReplacement {
                        start,
                        end,
                        bytes: bytes.to_vec(),
                    }],
                }
            })
            .collect(),
    }
}

#[test]
fn project_source_transactions_restore_inside_nested_includes_and_match_fresh() {
    use retex_rs2::session::EditingSession;
    let source=b"\\pdfinfoomitdate=1\\pdftrailerid{}\n\\font\\body=cmr10 \\body\\hsize=300pt\\vsize=500pt\\output={\\shipout\\box255}\n\\input chapter.tex\n\\input sibling.tex\nLast root paragraph.\\par\\end\n";
    let nested=(0..250).map(|i|format!("Nested paragraph {i}. This physical line contains enough plain text to wrap naturally.\\par\n")).collect::<String>();
    let chapter = b"Chapter begins.\\par\n\\input nested/child.tex\nChapter ends.\\par\n".to_vec();
    let sibling = b"Sibling paragraph.\\par\n".to_vec();
    let h = host(source);
    h.apply(CatalogDelta {
        base_epoch: h.epoch(),
        new_epoch: h.epoch() + 1,
        policy_version: h.policy_version(),
        changes: vec![
            put(Path::new("/project/chapter.tex"), chapter.clone()),
            put(
                Path::new("/project/nested/child.tex"),
                nested.as_bytes().to_vec(),
            ),
            put(Path::new("/project/sibling.tex"), sibling.clone()),
        ],
    })
    .unwrap();
    let mut settings = config();
    settings.progressive_pages = true;
    let mut session = EditingSession::new(settings, h).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let at = nested.find("Nested paragraph 210.").unwrap() + "Nested paragraph 210".len();
    let before = session.checkpoint_storage();
    let captures = session.reuse().checkpoint_captures;
    let tag = session.tag().cloned();
    session
        .set_checkpoint_focus("document", Path::new("/project/nested/child.tex"), at)
        .unwrap();
    assert_eq!(session.tag(), tag.as_ref());
    assert_eq!(session.reuse().checkpoint_captures, captures);
    assert_eq!(session.checkpoint_storage(), before);
    // Cold focus can use a coarse page boundary. Warm execution below must
    // establish local body boundaries without allocating a per-file cache.
    let tx = source_transaction(
        &session,
        2,
        vec![
            ("/project/nested/child.tex", at, at, b" grows"),
            ("/project/sibling.tex", 0, 7, b"Revised"),
        ],
    );
    session.apply_source_transaction(tx).unwrap();
    assert!(session.reuse().restored_offset > 0, "{:?}", session.reuse());
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let mut edited = nested.as_bytes().to_vec();
    edited.splice(at..at, b" grows".iter().copied());
    compare_fresh_resources(
        &mut session,
        source,
        &[
            ("chapter.tex", chapter.clone()),
            ("nested/child.tex", edited.clone()),
            ("sibling.tex", b"Revised paragraph.\\par\n".to_vec()),
        ],
    );
    // Backspace at the same boundary must retain a useful preceding state.
    let tx = source_transaction(
        &session,
        3,
        vec![("/project/nested/child.tex", at, at + 6, b"")],
    );
    session.apply_source_transaction(tx).unwrap();
    assert_eq!(
        session.reuse().restored_file.as_deref(),
        Some("/project/nested/child.tex")
    );
    assert_eq!(session.reuse().restored_occurrence, Some(1));
    assert!(session.reuse().restored_file_offset > 8192);
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert!(session.checkpoint_storage()["states"].as_u64().unwrap() <= 256);
    compare_fresh_resources(
        &mut session,
        source,
        &[
            ("chapter.tex", chapter),
            ("nested/child.tex", nested.into_bytes()),
            ("sibling.tex", b"Revised paragraph.\\par\n".to_vec()),
        ],
    );
}

#[test]
fn project_source_transaction_preflight_preserves_active_run_and_rejects_stale_bases() {
    use retex_rs2::session::EditingSession;
    let source = b"First paragraph.\\par\nLast paragraph.\\par\\end\n";
    let mut settings = config();
    settings.progressive_pages = true;
    let mut session = EditingSession::new(settings, host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    session.advance(1).unwrap();
    let epoch = session.host().epoch();
    let mut tx = source_transaction(
        &session,
        2,
        vec![
            ("/project/document.tex", 0, 5, b"Changed"),
            ("/project/part.tex", 1000, 1001, b"bad"),
        ],
    );
    assert!(session.apply_source_transaction(tx.clone()).is_err());
    assert_eq!(session.host().epoch(), epoch);
    assert_eq!(session.tag().unwrap().revision, 1);
    tx.changes.pop();
    tx.changes[0].expected_version += 1;
    assert!(session.apply_source_transaction(tx).is_err());
    assert_eq!(settled(&mut session), RunProgress::Complete);
    compare_fresh(&mut session, source);
}

#[test]
fn project_included_paragraph_preview_uses_included_bytes_and_admitted_page() {
    use retex_rs2::session::EditingSession;
    let source=b"\\pdfinfoomitdate=1\\pdftrailerid{}\\font\\body=cmr10 \\body\\hsize=220pt\\output={\\shipout\\box255}\n\\input part.tex\nFollowing root text.\\par\\end\n";
    let included =
        b"This included paragraph has ordinary text to edit and wrap across several lines.\n\n";
    let h = host(source);
    h.apply(CatalogDelta {
        base_epoch: h.epoch(),
        new_epoch: h.epoch() + 1,
        policy_version: 0,
        changes: vec![ResourceChange::Put {
            path: resource_path(Path::new("/project/part.tex")),
            id: ResourceId("/project/part.tex".into()),
            version: 2,
            bytes: included.as_slice().into(),
        }],
    })
    .unwrap();
    let mut settings = config();
    settings.exact_repro = false;
    settings.progressive_pages = true;
    settings.paragraph_preview = true;
    let mut session = EditingSession::new(settings.clone(), h).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let mut outlines = Default::default();
    let scene = session.capture_page(1, &mut outlines).unwrap();
    assert!(session.acknowledge_paragraph_page(
        "document",
        1,
        1,
        1,
        &scene.pages[0].content_sha256
    ));
    let at = included.windows(8).position(|w| w == b"ordinary").unwrap() + 8;
    let tx = source_transaction(
        &session,
        2,
        vec![("/project/part.tex", at, at, b" current")],
    );
    session.apply_source_transaction(tx).unwrap();
    let mut fragment = None;
    for _ in 0..100000 {
        let p = session.advance(1).unwrap();
        if session.paragraph_preview_status()["available"] == true {
            fragment = session
                .take_paragraph_preview(&mut outlines, &mut Default::default())
                .unwrap();
            break;
        }
        if p != RunProgress::Yielded {
            break;
        }
    }
    let fragment = fragment.unwrap_or_else(|| panic!("{}", session.paragraph_preview_status()));
    assert!(fragment.region.starts_with("/project/part.tex:1:"));
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let mut edited = included.to_vec();
    edited.splice(at..at, b" current".iter().copied());
    compare_fresh_config(&mut session, source, &[("part.tex", edited)], settings);
}

#[test]
fn project_sources_reject_consumed_occurrences_explicit_queries_and_buffered_callers() {
    use retex_rs2::session::EditingSession;
    for guard in ["repeat", "hash", "size", "caller", "prefix"] {
        let query = match guard {
            "hash" => "\\edef\\observed{\\pdfmdfivesum file{part.tex}}\n",
            "size" => "\\edef\\observed{\\pdffilesize{part.tex}}\n",
            _ => "",
        };
        let repeats = if guard == "repeat" {
            "\\input part.tex\n"
        } else {
            ""
        };
        let source = format!(
            "\\pdfinfoomitdate=1\\pdftrailerid{{}}\\font\\body=cmr10 \\body\\hsize=300pt\\vsize=500pt\\output={{\\shipout\\box255}}\\count9=0\n{query}\\input part.tex \\count8=7\n{repeats}End.\\par\\end\n"
        );
        let part=format!("\\advance\\count9 by1\n{}",(0..120).map(|i|format!("Paragraph {i}. Source prefixes must preserve every decoded and suspended line.\\par\n")).collect::<String>());
        let h = host(source.as_bytes());
        h.apply(CatalogDelta {
            base_epoch: h.epoch(),
            new_epoch: h.epoch() + 1,
            policy_version: 0,
            changes: vec![ResourceChange::Put {
                path: resource_path(Path::new("/project/part.tex")),
                id: ResourceId("/project/part.tex".into()),
                version: 2,
                bytes: part.as_bytes().into(),
            }],
        })
        .unwrap();
        let mut settings = config();
        settings.progressive_pages = true;
        let mut session = EditingSession::new(settings, h).unwrap();
        session
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        let at = if guard == "prefix" {
            0
        } else {
            part.find("Paragraph 100.").unwrap() + 13
        };
        let root_at = source.find("\\count8=7").unwrap() + 8;
        let mut changes = vec![("/project/part.tex", at, at, b" edited".as_slice())];
        if guard == "caller" {
            changes.push(("/project/document.tex", root_at, root_at + 1, b"9"));
        }
        let tx = source_transaction(&session, 2, changes);
        session.apply_source_transaction(tx).unwrap();
        if guard == "repeat" {
            if session.reuse().restored_file.as_deref() == Some("/project/part.tex") {}
        } else {
            assert_ne!(
                session.reuse().restored_file.as_deref(),
                Some("/project/part.tex"),
                "{guard}: {:?}",
                session.reuse()
            );
        }
        assert_eq!(settled(&mut session), RunProgress::Complete, "{guard}");
        let mut edited = part.into_bytes();
        edited.splice(at..at, b" edited".iter().copied());
        let mut root = source.into_bytes();
        if guard == "caller" {
            root[root_at] = b'9';
        }
        compare_fresh_resources(&mut session, &root, &[("part.tex", edited)]);
    }
}

#[test]
fn switching_focus_rebases_resumed_physical_provenance_before_later_includes() {
    use retex_rs2::session::EditingSession;
    let source = b"\\pdfinfoomitdate=1\\pdftrailerid{}\n\\font\\body=cmr10 \\body\\hsize=300pt\\vsize=500pt\\count8=0\n\\immediate\\openout1=proof.txt\n\\input a.tex\n\\input b.tex\nCount: \\the\\count8\\par\\immediate\\closeout1\\end\n";
    let mut a = b"\\global\\advance\\count8 by1\\immediate\\write1{A}\nIntroduction A.\\par\nTarget a. More ordinary text.\\par\n".to_vec();
    let mut b = b"\\global\\advance\\count8 by2\\immediate\\write1{B}\nIntroduction B.\\par\nTarget b. More ordinary text.\\par\n".to_vec();
    let h = host(source);
    h.apply(CatalogDelta {
        base_epoch: h.epoch(),
        new_epoch: h.epoch() + 1,
        policy_version: h.policy_version(),
        changes: vec![
            put(Path::new("/project/a.tex"), a.clone()),
            put(Path::new("/project/b.tex"), b.clone()),
        ],
    })
    .unwrap();
    let mut settings = config();
    settings.progressive_pages = true;
    let mut session = EditingSession::new(settings, h).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    for (index, path, warm) in [
        (0, "/project/a.tex", false),
        (1, "/project/b.tex", false),
        (2, "/project/b.tex", true),
        (3, "/project/a.tex", true),
    ] {
        let bytes = if path.ends_with("a.tex") {
            &mut a
        } else {
            &mut b
        };
        let at = bytes
            .windows(9)
            .position(|w| {
                w == if path.ends_with("a.tex") {
                    b"Target a."
                } else {
                    b"Target b."
                }
            })
            .unwrap()
            + 9;
        session
            .set_checkpoint_focus("document", Path::new(path), at)
            .unwrap();
        let tx = source_transaction(&session, index + 2, vec![(path, at, at, b"x")]);
        session.apply_source_transaction(tx).unwrap();
        if warm {
            assert_eq!(
                session.reuse().restored_file.as_deref(),
                Some(path),
                "{:?}",
                session.reuse()
            );
        }
        bytes.insert(at, b'x');
        assert_eq!(settled(&mut session), RunProgress::Complete);
        compare_fresh_resources(
            &mut session,
            source,
            &[("a.tex", a.clone()), ("b.tex", b.clone())],
        );
    }
}

#[test]
fn source_provenance_maps_definitions_arguments_ligatures_and_physical_ranges() {
    let source = b"\\font\\body=cmr10 \\body\n\\def\\word{XY}\\def\\dup#1{#1#1}\n\\word \\dup{Z} fi ^^41\\par\n\\hbox to5pt{Too wide}\\par\\end\n";
    let mut session = retex_rs2::session::EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let scene = session.capture_page(1, &mut Default::default()).unwrap();
    let map = &scene.pages[0].source_map;
    let mapped = |line, start, end| {
        map.items
            .iter()
            .filter(|item| {
                item.kind == "glyph"
                    && item.origins.iter().any(|&id| {
                        let range = &map.ranges[id];
                        range.line == line && range.start == start && range.end == end
                    })
            })
            .count()
    };
    assert_eq!(
        mapped(2, 11, 12),
        1,
        "macro body X must map to its definition"
    );
    assert_eq!(
        mapped(3, 12, 13),
        2,
        "duplicated argument has two output occurrences"
    );
    let fi = map
        .items
        .iter()
        .find(|item| item.kind == "glyph" && item.origins.len() == 2)
        .unwrap();
    assert_eq!(
        fi.origins
            .iter()
            .map(|&id| map.ranges[id].start)
            .collect::<Vec<_>>(),
        vec![15, 16]
    );
    assert_eq!(mapped(3, 18, 22), 1, "^^41 retains all four physical bytes");
    let warning = map
        .items
        .iter()
        .find(|item| {
            item.warning
                .as_deref()
                .is_some_and(|s| s.starts_with("Overfull"))
        })
        .unwrap();
    assert_eq!(warning.kind, "hbox");
    assert!(warning.origins.iter().all(|&id| map.ranges[id].line == 4));
    assert!(
        warning.origins.len() <= 2,
        "warning ranges should merge touching letters without bridging the space"
    );
    assert!(!warning.origins.is_empty());
    assert!(warning.quad.iter().flatten().all(|n| n.is_finite()));
}

#[test]
fn source_provenance_nested_box_warnings_exclude_siblings() {
    let source =
        b"\\font\\body=cmr10 \\body\n\\hbox to1pt{\\hbox to1pt{AB}C}\\hbox to1pt{D}\\par\\end\n";
    let mut session = retex_rs2::session::EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let scene = session.capture_page(1, &mut Default::default()).unwrap();
    let map = &scene.pages[0].source_map;
    let line = source.split(|b| *b == b'\n').nth(1).unwrap();
    let mut owners: Vec<_> = map
        .items
        .iter()
        .filter(|i| {
            i.warning
                .as_deref()
                .is_some_and(|w| w.starts_with("Overfull"))
        })
        .map(|item| {
            item.origins
                .iter()
                .flat_map(|&id| {
                    let range = &map.ranges[id];
                    assert_eq!(range.line, 2);
                    line[range.start - 1..range.end - 1].iter().copied()
                })
                .collect::<Vec<_>>()
        })
        .collect();
    owners.sort();
    assert_eq!(owners, [b"AB".to_vec(), b"ABC".to_vec(), b"D".to_vec()]);
}

#[test]
fn source_provenance_preserves_unicode_bom_and_control_sequence_error_ranges() {
    let source =
        "\u{feff}\\font\\body=cmr10 \\body\r\nA ^^42\\par\r\n\\undefinedword\\end\r\n".as_bytes();
    let mut settings = config();
    settings.language_profile = retex_rs2::core::characters::LanguageProfile::Unicode;
    let mut session = retex_rs2::session::EditingSession::new(settings, host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Failed);
    let diagnostic = session
        .engine()
        .unwrap()
        .core
        .diagnostics
        .records
        .last()
        .unwrap();
    let range = diagnostic.source.as_ref().unwrap();
    assert_eq!(
        (
            range.line,
            range.byte_column.map(std::num::NonZeroUsize::get),
            range.end_byte_column.map(std::num::NonZeroUsize::get)
        ),
        (3, Some(1), Some(15))
    );
}

#[test]
fn source_provenance_math_nuclei_scripts_and_generated_rules_keep_origins() {
    let source = br#"\font\body=cmr10 \body
\font\sy=cmsy10 \font\ex=cmex10 \textfont2=\sy\scriptfont2=\sy\scriptscriptfont2=\sy\textfont3=\ex\scriptfont3=\ex\scriptscriptfont3=\ex
\textfont0=\body\scriptfont0=\body\scriptscriptfont0=\body\textfont1=\body\scriptfont1=\body\scriptscriptfont1=\body
$a^2+{b\over c}$\par\end
"#;
    let mut session = retex_rs2::session::EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let scene = session.capture_page(1, &mut Default::default()).unwrap();
    let map = &scene.pages[0].source_map;
    for column in [2, 4, 5, 7, 14] {
        assert!(
            map.ranges.iter().any(|r| r.line == 4 && r.start == column),
            "missing math origin at {column}: {map:?}"
        );
    }
    assert!(
        map.items
            .iter()
            .any(|i| i.kind == "rule" && i.origins.iter().any(|&id| map.ranges[id].start == 8))
    );
}

#[test]
fn source_provenance_incremental_nested_maps_match_fresh_execution() {
    use retex_rs2::session::EditingSession;
    let root=b"\\pdfinfoomitdate=1\\pdftrailerid{}\n\\font\\body=cmr10 \\body\\hsize=220pt\\vsize=500pt\\def\\word{Defined}\n\\input nested.tex\nRoot end.\\par\\end\n";
    let mut included = (0..90)
        .map(|i| format!("Paragraph {i}. \\word has fi and a repeated body.\\par\n"))
        .collect::<String>()
        .into_bytes();
    let h = host(root);
    h.apply(CatalogDelta {
        base_epoch: h.epoch(),
        new_epoch: h.epoch() + 1,
        policy_version: h.policy_version(),
        changes: vec![put(Path::new("/project/nested.tex"), included.clone())],
    })
    .unwrap();
    let mut cfg = config();
    cfg.progressive_pages = true;
    let mut session = EditingSession::new(cfg.clone(), h).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            root.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let at = included
        .windows(13)
        .position(|s| s == b"Paragraph 70.")
        .unwrap()
        + 12;
    session
        .set_checkpoint_focus("document", Path::new("/project/nested.tex"), at)
        .unwrap();
    let mut warm_outlines = Default::default();
    for page in 1..=session.engine().unwrap().completed_page_count() {
        session.capture_page(page, &mut warm_outlines).unwrap();
    }
    for revision in 2..=6 {
        let (until, replacement): (usize, &[u8]) = match revision {
            2 => (at, b" new"),
            3 => (at, b"x"),
            4 => (at, b"\n"),
            _ => (at + 1, b""),
        };
        let tx = source_transaction(
            &session,
            revision,
            vec![("/project/nested.tex", at, until, replacement)],
        );
        session.apply_source_transaction(tx).unwrap();
        included.splice(at..until, replacement.iter().copied());
        if revision >= 3 {
            assert!(session.reuse().restored_offset > 0);
        }
        assert_eq!(settled(&mut session), RunProgress::Complete);
        let fresh_host = host(root);
        fresh_host
            .apply(CatalogDelta {
                base_epoch: fresh_host.epoch(),
                new_epoch: fresh_host.epoch() + 1,
                policy_version: fresh_host.policy_version(),
                changes: vec![put(Path::new("/project/nested.tex"), included.clone())],
            })
            .unwrap();
        let mut fresh_cfg = cfg.clone();
        fresh_cfg.execution_reuse = false;
        let mut fresh = EditingSession::new(fresh_cfg, fresh_host).unwrap();
        fresh
            .open_document(
                "document",
                1,
                Path::new("/project/document.tex"),
                root.as_slice().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut fresh), RunProgress::Complete);
        let mut outlines = Default::default();
        let canonical = |scene: retex_rs2::pdf::preview::RetainedPreview| {
            let map = &scene.pages[0].source_map;
            map.items.iter().map(|item|serde_json::json!({"kind":item.kind,"quad":item.quad,"origins":item.origins.iter().map(|&id|&map.ranges[id]).collect::<Vec<_>>(),"warning":item.warning})).collect::<Vec<_>>()
        };
        for page in 1..=session.engine().unwrap().completed_page_count() {
            assert_eq!(
                canonical(session.capture_page(page, &mut outlines).unwrap()),
                canonical(fresh.capture_page(page, &mut outlines).unwrap()),
                "revision {revision}, page {page}"
            );
        }
    }
}

#[test]
fn source_provenance_case_changes_preserve_letters_and_generated_tokens_do_not_inherit_them() {
    use retex_rs2::session::EditingSession;
    let source=b"\\font\\body=cmr10 \\body\n\\uppercase{ab}\\par\n\\def\\generated{\\number123}\\generated\\par\n\\toks0={Stored}\\edef\\indirect{\\the\\toks0}\\indirect\\par\n\\toks0={f}\\edef\\mixed{\\the\\toks0}\\mixed i\\par\\end\n";
    let mut session = EditingSession::new(config(), host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let scene = session.capture_page(1, &mut Default::default()).unwrap();
    let map = &scene.pages[0].source_map;
    assert_eq!(
        map.items
            .iter()
            .filter(|i| i.kind == "glyph" && !i.origins.is_empty())
            .count(),
        2,
        "generated text must never inherit physical letter origins: {map:?}"
    );
    let physical = map
        .items
        .iter()
        .flat_map(|i| i.origins.iter())
        .map(|id| &map.ranges[*id])
        .collect::<Vec<_>>();
    assert!(physical.iter().all(|r| r.line == 2));
    assert_eq!(
        physical
            .iter()
            .map(|r| (r.start, r.end))
            .collect::<Vec<_>>(),
        vec![(12, 13), (13, 14)]
    );
    let generated = map
        .items
        .iter()
        .filter(|i| i.kind == "glyph" && i.origins.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(generated.len(), 6); // Stored: invocation links, never character origins.
    assert!(
        generated.iter().all(|i| !i.invocations.is_empty()
            && i.invocations.iter().all(|id| map.ranges[*id].line == 4))
    );
}

#[test]
fn source_provenance_paragraph_bounds_include_the_entire_expanded_character_spelling() {
    use retex_rs2::session::EditingSession;
    let source=b"\\font\\body=cmr10 \\body\\hsize=200pt\\vsize=500pt\nAn ordinary paragraph ends with ^^41\\par\nFollowing material.\\par\\end\n";
    let mut cfg = config();
    cfg.exact_repro = false;
    cfg.progressive_pages = true;
    cfg.paragraph_preview = true;
    let mut session = EditingSession::new(cfg, host(source)).unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    let mut outlines = Default::default();
    let scene = session.capture_page(1, &mut outlines).unwrap();
    assert!(session.acknowledge_paragraph_page(
        "document",
        1,
        1,
        1,
        &scene.pages[0].content_sha256
    ));
    let at = source.windows(4).position(|s| s == b"^^41").unwrap();
    let mut edited = source.to_vec();
    edited[at + 3] = b'2';
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            edited.as_slice().into(),
        )
        .unwrap();
    let mut fragment = None;
    for _ in 0..100000 {
        let progress = session.advance(1).unwrap();
        if session.paragraph_preview_status()["available"] == true {
            fragment = session
                .take_paragraph_preview(&mut outlines, &mut Default::default())
                .unwrap();
            break;
        }
        if progress != RunProgress::Yielded {
            break;
        }
    }
    let fragment = fragment.unwrap_or_else(|| panic!("{:?}", session.paragraph_preview_status()));
    assert_eq!(fragment.source_bounds[1], at + 4);
    assert!(!fragment.preview.pages[0].source_map.items.is_empty());
}

#[test]
fn recovered_included_file_does_not_publish_auxiliary_state_or_survive_a_clean_edit() {
    use retex_rs2::{core::recovery::RecoveryPolicy, session::EditingSession};
    let source = b"\\font\\body=cmr10 \\body\\input part.tex\nRootSuffix.\\par\\end\n";
    let host = host(source);
    let child = |version, bytes: &[u8]| ResourceChange::Put {
        id: ResourceId("/project/part.tex".into()),
        path: resource_path(Path::new("/project/part.tex")),
        version,
        bytes: bytes.into(),
    };
    host.apply(CatalogDelta {base_epoch:host.epoch(),new_epoch:host.epoch()+1,policy_version:host.policy_version(),changes:vec![child(2,b"\\immediate\\openout0=notes.aux\\immediate\\write0{broken}\\immediate\\closeout0\n$x^1^2$\nChildSuffix.\\par\n"),put(Path::new("/project/notes.aux"),b"valid generation\n".to_vec())]}).unwrap();
    let mut session = EditingSession::new(
        EngineConfig {
            recovery_policy: RecoveryPolicy::Interactive,
            ..config()
        },
        host.clone(),
    )
    .unwrap();
    session
        .open_document(
            "document",
            1,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert!(session.engine().unwrap().recovered());
    assert!(session.commit_outputs("document", 1).is_err());
    assert!(
        session
            .engine()
            .unwrap()
            .core
            .diagnostics
            .records
            .iter()
            .any(|r| r
                .source
                .as_ref()
                .is_some_and(|s| s.file.ends_with("part.tex")))
    );
    session
        .update_resources(CatalogDelta {
            base_epoch: host.epoch(),
            new_epoch: host.epoch() + 1,
            policy_version: host.policy_version(),
            changes: vec![child(3, b"CleanChild.\\par\n")],
        })
        .unwrap();
    assert_eq!(
        host.read(Path::new("/project/notes.aux")).unwrap(),
        b"valid generation\n"
    );
    session
        .open_document(
            "document",
            2,
            Path::new("/project/document.tex"),
            source.as_slice().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert!(!session.engine().unwrap().recovered());
    assert!(session.engine().unwrap().pdf_output_available());
    assert!(
        session
            .engine()
            .unwrap()
            .core
            .diagnostics
            .records
            .iter()
            .all(|r| r.details.as_ref().is_none_or(|d| d.recovery.is_none()))
    );
}

#[test]
fn interactive_recovery_preserves_valid_reuse_and_never_caches_away_errors() {
    use retex_rs2::{core::recovery::RecoveryPolicy, session::EditingSession};
    let prefix =
        "\\font\\body=cmr10 \\body\\hsize=160pt\\vsize=500pt\n\\def\\word#1{#1 material}\n";
    let suffix =
        "\n\nUnchanged paragraph uses \\word{reusable} and several ordinary words.\n\n\\end\n";
    let settings = EngineConfig {
        recovery_policy: RecoveryPolicy::Interactive,
        progressive_pages: true,
        ..config()
    };
    let mut session = EditingSession::new(settings.clone(), host(b"\\end")).unwrap();
    for revision in 1..=3 {
        let source = format!(
            "{prefix}First paragraph {}.{suffix}",
            "grows ".repeat(revision)
        );
        session
            .open_document(
                "document",
                revision as u64,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        assert!(!session.engine().unwrap().recovered());
        if revision > 1 {
            assert!(
                session.engine().unwrap().execution_reuse_stats().hits > 0,
                "interactive policy disabled valid computation reuse"
            );
        }
    }
    let damaged = suffix.replace("reusable}", "reusable} \\undefinedcommand");
    for revision in 4..=6 {
        let source = format!(
            "{prefix}First paragraph {}.{damaged}",
            "grows ".repeat(revision)
        );
        session
            .open_document(
                "document",
                revision as u64,
                Path::new("/project/document.tex"),
                source.as_bytes().into(),
            )
            .unwrap();
        assert_eq!(settled(&mut session), RunProgress::Complete);
        let engine = session.engine().unwrap();
        assert!(
            engine.recovered(),
            "an erroneous receipt hid the diagnostic"
        );
        assert!(engine.core.diagnostics.records.iter().any(|r| {
            r.details
                .as_ref()
                .is_some_and(|d| d.code == "tex.undefined_command")
        }));
        assert!(!engine.pdf_output_available());
    }
    let source = format!("{prefix}First paragraph repaired.{suffix}");
    session
        .open_document(
            "document",
            7,
            Path::new("/project/document.tex"),
            source.as_bytes().into(),
        )
        .unwrap();
    assert_eq!(settled(&mut session), RunProgress::Complete);
    assert!(!session.engine().unwrap().recovered());
    assert!(session.engine().unwrap().pdf_output_available());
}
