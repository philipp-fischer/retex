//! Initialization cache provenance validates source, configuration and format bytes.
use crate::{
    core::errors::{Error, Result},
    diagnostics::snapshot::ascii_json,
    resources::paths::PathPolicy,
    serialization::formats::FormatSnapshot,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
pub const PATH_VARIABLES: &[&str] = &[
    "TEXMFCNF",
    "TEXINPUTS",
    "TFMFONTS",
    "T1FONTS",
    "ENCFONTS",
    "TEXFONTMAPS",
    "VFFONTS",
    "TTFONTS",
    "OPENTYPEFONTS",
    "PKFONTS",
    "SFDFONTS",
    "TEXFONTS",
];
fn digest(data: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(data.as_ref()))
}
pub fn path_profile(p: &PathPolicy) -> Result<Value> {
    let mut files = Vec::new();
    for file in &p.configuration_files {
        files.push(json!({"path":file,"sha256":digest(crate::resources::host::read(file)?)}));
    }
    let mut environment = BTreeMap::new();
    for name in PATH_VARIABLES {
        for key in [
            name.to_string(),
            format!("{name}.{}", p.program),
            format!("{name}_{}", p.program),
        ] {
            if let Some(value) = crate::resources::host::var_os(&key) {
                environment.insert(key, value.to_string_lossy().into_owned());
            }
        }
    }
    Ok(
        json!({"locations":{"font_assets":p.locations.font_assets,"font_metrics":p.locations.font_metrics,"input_roots":p.locations.input_roots,"configuration_paths":p.locations.configuration_paths,"reference_root":p.locations.reference_root},"program":p.program,"configuration_files":files,"environment":environment}),
    )
}
pub fn sidecar(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        "{}.manifest.json",
        path.file_name().unwrap_or_default().to_string_lossy()
    ))
}
pub fn write_manifest(
    state: &FormatSnapshot,
    policy: &PathPolicy,
    sources: &BTreeMap<String, String>,
    exact_repro: bool,
    path: &Path,
) -> Result<Value> {
    let fonts = state
        .fonts
        .fonts
        .iter()
        .map(|f| json!({"name":f.name,"size":f.size,"tfm":f.tfm,"pfb":f.pfb,"enc":f.enc}))
        .collect::<Vec<_>>();
    let profile = json!({"schema_version":state.version,"primitive_signature":state.primitive_signature,"exact_repro":exact_repro,"resource_search":path_profile(policy)?,"sources":sources,"font_resources":fonts});
    let identity = digest(ascii_json(&profile, false)?);
    let manifest = json!({"manifest_version":1,"cache_identity":identity,"format_sha256":digest(crate::resources::host::read(path)?),"initialization":profile,"host":{"rust_minimum":env!("CARGO_PKG_RUST_VERSION"),"system":std::env::consts::OS}});
    let target = sidecar(path);
    let temporary = target.with_file_name(format!(
        "{}.tmp",
        target.file_name().unwrap_or_default().to_string_lossy()
    ));
    let result: Result<()> = (|| {
        crate::resources::host::write(&temporary, format!("{}\n", ascii_json(&manifest, true)?))?;
        crate::resources::host::rename(&temporary, &target)?;
        Ok(())
    })();
    let _ = crate::resources::host::remove_file(temporary);
    result?;
    Ok(manifest)
}
pub fn validate_manifest(path: &Path) -> Result<Option<Value>> {
    let sidecar = sidecar(path);
    if crate::resources::host::metadata(&sidecar).is_err() {
        return Ok(None);
    }
    let manifest: Value = serde_json::from_slice(&crate::resources::host::read(sidecar)?)
        .map_err(|e| Error::value(e.to_string()))?;
    if manifest["manifest_version"] != 1 {
        return Err(Error::value("Unsupported format provenance manifest"));
    }
    if manifest["format_sha256"] != digest(crate::resources::host::read(path)?) {
        return Err(Error::value(
            "Format cache content differs from provenance manifest",
        ));
    }
    let profile = &manifest["initialization"];
    if manifest["cache_identity"] != digest(ascii_json(profile, false)?) {
        return Err(Error::value("Format cache provenance is corrupt"));
    }
    let mut records = Vec::new();
    if let Some(sources) = profile["sources"].as_object() {
        records.extend(
            sources
                .iter()
                .map(|(p, d)| (p.clone(), d.as_str().unwrap_or("").to_string())),
        )
    }
    if let Some(files) = profile["resource_search"]["configuration_files"].as_array() {
        records.extend(files.iter().map(|v| {
            (
                v["path"].as_str().unwrap_or("").to_string(),
                v["sha256"].as_str().unwrap_or("").to_string(),
            )
        }))
    }
    for (path, expected) in records {
        let path = Path::new(&path);
        if !crate::resources::host::is_file(&path)
            || digest(crate::resources::host::read(path)?) != expected
        {
            return Err(Error::value(format!(
                "Format initialization resource changed: {}",
                path.display()
            )));
        }
    }
    Ok(Some(manifest))
}
