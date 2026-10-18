//! Minimal native integration host: actual input -> shared retained scene.
//! Browser Canvas can consume this same scene; PDF generation is independent.
use retex_rs2::{Engine, core::config::EngineConfig, fonts::outlines::OutlineCache};
use std::path::Path;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let input = args.next().ok_or("Expected input.tex and scene.json")?;
    let output = args.next().ok_or("Expected scene.json")?;
    let format = args.next();
    if args.next().is_some() {
        return Err("Usage: preview-host input.tex scene.json [initialized.rsfmt]".into());
    }
    let mut engine = Engine::new(EngineConfig {
        ini: format.is_some(),
        trace_enabled: false,
        exact_repro: true,
        interaction_mode: Some(0),
        ..Default::default()
    })?;
    if let Some(format) = format {
        engine.load_format(Path::new(&format))?;
    }
    if !engine.start_input(Path::new(&input)) || !engine.main_body() {
        return Err(format!("TeX execution failed: {:?}", engine.core.diagnostics.errors).into());
    }
    let scene = retex_rs2::pdf::preview::capture(&engine, &mut OutlineCache::default())?;
    let file = std::fs::File::create(output)?;
    serde_json::to_writer(file, &scene)?;
    Ok(())
}
