//! Fresh-job measurements in a resident native harness. These are explicitly
//! not editing-session or edit-to-visible latency measurements.
use retex_rs2::{
    Engine,
    core::{characters::LanguageProfile, config::EngineConfig},
    engine::RunProgress,
};
use serde_json::json;
use std::{path::PathBuf, time::Instant};

fn milliseconds(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let input = PathBuf::from(args.next().ok_or("usage: engine-benchmark INPUT REPORT [--format FORMAT] [--quantum N] [--samples N] [--warmup N] [--unicode] [--exact]")?);
    let report_path = PathBuf::from(args.next().ok_or("Missing report path")?);
    let mut samples = 100usize;
    let mut warmup = 10usize;
    let mut quantum = usize::MAX;
    let mut format = None;
    let mut config = EngineConfig {
        interaction_mode: Some(0),
        ..Default::default()
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--samples" => samples = args.next().ok_or("Missing samples")?.parse()?,
            "--warmup" => warmup = args.next().ok_or("Missing warmup")?.parse()?,
            "--quantum" => quantum = args.next().ok_or("Missing quantum")?.parse()?,
            "--format" => format = Some(PathBuf::from(args.next().ok_or("Missing format")?)),
            "--unicode" => config.language_profile = LanguageProfile::Unicode,
            "--exact" => config.exact_repro = true,
            _ => return Err(format!("Unknown argument: {arg}").into()),
        }
    }
    if samples == 0 || quantum == 0 {
        return Err("Samples and quantum must be positive".into());
    }
    let output = report_path.parent().unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(output)?;
    config.output_directory = Some(std::fs::canonicalize(output)?.display().to_string());
    let mut records = Vec::new();
    let mut failures = Vec::new();
    for sample in 0..samples.checked_add(warmup).ok_or("Sample count overflow")? {
        let whole = Instant::now();
        let start = Instant::now();
        let mut engine = Engine::new(config.clone())?;
        let construction_ms = milliseconds(start);
        let start = Instant::now();
        if let Some(format) = &format {
            engine.load_format(format)?;
        }
        let format_load_ms = milliseconds(start);
        let start = Instant::now();
        if !engine.start_input(&input) {
            return Err(engine
                .core
                .input
                .last_input_error
                .clone()
                .unwrap_or_default()
                .into());
        }
        let input_open_ms = milliseconds(start);
        let start = Instant::now();
        engine.begin_run()?;
        let mut max_advance_ms = 0.0_f64;
        let mut advances = 0usize;
        let success = loop {
            let advance = Instant::now();
            let progress = engine.advance_run(quantum)?;
            max_advance_ms = max_advance_ms.max(milliseconds(advance));
            advances += 1;
            match progress {
                RunProgress::Yielded => {}
                RunProgress::Complete => break true,
                RunProgress::Failed => break false,
            }
        };
        let execution_with_host_ms = milliseconds(start);
        let start = Instant::now();
        let exported = if success {
            engine.write_pdf(&output.join("benchmark.pdf"))?
        } else {
            false
        };
        let pdf_export_ms = milliseconds(start);
        let whole_job_ms = milliseconds(whole);
        if !success {
            failures.push(json!({"sample":sample,"errors":engine.core.diagnostics.errors}));
        }
        if sample >= warmup {
            records.push(json!({"construction_with_host_ms":construction_ms,"format_load_ms":format_load_ms,
                "input_open_ms":input_open_ms,"execution_with_host_ms":execution_with_host_ms,
                "pdf_export_ms":pdf_export_ms,"whole_job_ms":whole_job_ms,
                "max_advance_ms":max_advance_ms,"advances":advances,"success":success,"pdf_exported":exported}));
        }
        if sample + 1 == samples + warmup {
            engine.dump_layout(&output.join("benchmark-layout.json"))?;
        }
    }
    let mut percentiles = serde_json::Map::new();
    for name in [
        "construction_with_host_ms",
        "format_load_ms",
        "input_open_ms",
        "execution_with_host_ms",
        "pdf_export_ms",
        "whole_job_ms",
        "max_advance_ms",
    ] {
        let mut times = records
            .iter()
            .map(|r| r[name].as_f64().unwrap())
            .collect::<Vec<_>>();
        times.sort_by(f64::total_cmp);
        let value = |fraction: f64| {
            times[((times.len() as f64 * fraction).ceil() as usize).saturating_sub(1)]
        };
        percentiles.insert(
            name.into(),
            json!({"p50":value(0.50),"p95":value(0.95),"p99":value(0.99)}),
        );
    }
    use sha2::{Digest, Sha256};
    let report = json!({"schema_version":1,"scope":"resident native harness; a fresh engine and disk format load for every job; no edit, transport or visible-presentation measurement",
        "execution_scope":"scanner/expansion/layout/emission preparation plus synchronous host services; host work inside execution is not yet separately instrumented",
        "input":input,"input_sha256":format!("{:x}",Sha256::digest(std::fs::read(&input)?)),
        "format":format,"format_sha256":format.as_ref().map(|p|std::fs::read(p).map(|b|format!("{:x}",Sha256::digest(b)))).transpose()?,
        "binary_sha256":format!("{:x}",Sha256::digest(std::fs::read(std::env::current_exe()?)?)),
        "configuration":config,"samples":samples,"warmup":warmup,"quantum":quantum,"percentiles":percentiles,"failures":failures,"records":records,
        "unmeasured":["process startup","pure engine vs host work during execution","queueing/transport","rendering/visible presentation","memory","edit backlog/cancellation"]});
    std::fs::write(
        &report_path,
        format!("{}\n", serde_json::to_string_pretty(&report)?),
    )?;
    if !failures.is_empty() {
        return Err("One or more measured jobs failed; see report".into());
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
