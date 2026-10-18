//! Validation-only bindings for the shared host's actual file operations.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn evaluate() -> Result<String, wasm_bindgen::JsValue> {
    execute()
        .map(|value| value.to_string())
        .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))
}
#[cfg(target_arch = "wasm32")]
fn execute() -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    use retex_rs2::{
        Engine,
        core::{characters::LanguageProfile, config::EngineConfig},
        language::records::{Quantity, QuantityKind, ValueKey},
        resources::{
            catalog::{CatalogDelta, ResourceCatalog, ResourceChange, ResourceId, ResourcePath},
            effects::DocumentRevision,
            host::{FixedClock, Host},
            virtual_host::VirtualHost,
        },
    };
    use sha2::{Digest, Sha256};
    use std::{
        collections::BTreeMap,
        io::Write,
        path::{Path, PathBuf},
        rc::Rc,
    };
    let a = Path::new("/project/a");
    let b = Path::new("/project/b");
    let initial = [b"A\n".as_slice(), &vec![b'x'; 9200], b"\nTail A\n"].concat();
    let source = b"\\catcode123=1\\catcode125=2\\count7=42\\def\\saved{Stored}\\dump\n";
    let mut catalog = ResourceCatalog::new(Default::default());
    catalog.apply(CatalogDelta {
        base_epoch: 0,
        new_epoch: 1,
        policy_version: 0,
        changes: [
            ("/project/a", initial),
            ("/project/b", b"B\nTail B\n".to_vec()),
            ("/project/document.tex", source.to_vec()),
            ("/project/subdir/child", b"old".to_vec()),
        ]
        .into_iter()
        .map(|(path, bytes)| ResourceChange::Put {
            id: ResourceId(path.into()),
            path: ResourcePath(path.as_bytes().to_vec()),
            version: 1,
            bytes: bytes.into(),
        })
        .collect(),
    })?;
    let host = Rc::new(VirtualHost::new(
        catalog,
        PathBuf::from("/project"),
        BTreeMap::new(),
        FixedClock::new(1_600_000_000, 0, 0)?,
        100_000,
    )?);
    let tag = DocumentRevision {
        document: "probe".into(),
        revision: 1,
    };
    host.begin_effects(tag.clone(), 10_000_000)?;
    let mut reader_a = host.open_physical(a)?.unwrap();
    let mut reader_b = host.open_physical(b)?.unwrap();
    let profile = LanguageProfile::Pdftex;
    let first_a = reader_a.readline(None, profile)?.unwrap();
    let first_b = reader_b.readline(None, profile)?.unwrap();
    let mut writer = host.open_output(a, true)?;
    host.rename(a, b)?;
    writer.write_all(b"Appended\n")?;
    let after_rename_a_exists = host.metadata(a).is_ok();
    let renamed_bytes_sha256 = format!("{:x}", Sha256::digest(host.read(b)?));
    host.remove_file(b)?;
    writer.write_all(b"Unlinked\n")?;
    host.open_output(b, false)?.write_all(b"Replacement\n")?;
    let mut remaining = String::new();
    while let Some(line) = reader_a.readline(None, profile)? {
        remaining.push_str(&line);
    }
    let reader_a_remaining_sha256 = format!("{:x}", Sha256::digest(remaining.as_bytes()));
    let reader_b_remaining = reader_b.readline(None, profile)?.unwrap();
    host.remove_file(Path::new("/project/subdir/child"))?;
    host.commit_effects(&tag)?;
    let directory_survives_file_removal = host.metadata(Path::new("/project/subdir"))?.is_dir();
    let final_files = serde_json::json!({"b":String::from_utf8(host.read(b)?)?});
    assert!(host.committed(a)?.is_none());
    host.begin_effects(
        DocumentRevision {
            revision: 2,
            ..tag.clone()
        },
        10_000_000,
    )?;
    let settings = EngineConfig {
        ini: true,
        exact_repro: true,
        configuration_paths: Some(vec![]),
        tex_input_roots: Some(vec!["/project".into()]),
        output_directory: Some("/project".into()),
        interaction_mode: Some(0),
        ..Default::default()
    };
    let mut engine = Engine::new_with_host(settings.clone(), host.clone())?;
    assert!(engine.start_input(Path::new("/project/document.tex")));
    assert!(engine.main_body());
    let format = Path::new("/project/minimal.rsfmt");
    engine.write_format(format)?;
    host.commit_effects(&DocumentRevision { revision: 2, ..tag })?;
    let mut restored = Engine::new_with_host(settings, host.clone())?;
    restored.load_format(format)?;
    assert_eq!(
        restored
            .env
            .values
            .get(&ValueKey::register(QuantityKind::Count, 7)),
        Some(&Quantity::Integer(42))
    );
    Ok(
        serde_json::json!({"first_a":first_a,"first_b":first_b,"after_rename_a_exists":after_rename_a_exists,
        "renamed_bytes_sha256":renamed_bytes_sha256,"reader_a_remaining_sha256":reader_a_remaining_sha256,
        "reader_b_remaining":reader_b_remaining,"final_files":final_files,"directory_survives_file_removal":directory_survives_file_removal,"format_roundtrip_count":42}),
    )
}
