//! Development-only bounded scheduling probe; TeX runs in the existing Engine.
use retex_rs2::{Engine, core::config::EngineConfig, engine::RunProgress};
use std::{io::Write, path::PathBuf};
fn main() {
    let path = PathBuf::from(std::env::args_os().nth(1).expect("source path"));
    let config = EngineConfig {
        interaction_mode: Some(0),
        exact_repro: true,
        output_directory: path.parent().map(|p| p.to_string_lossy().into_owned()),
        configuration_paths: Some(vec![]),
        font_dir: Some(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("assets/fonts/assets")
                .to_string_lossy()
                .into_owned(),
        ),
        expansion_limit: Some(1_000_000_000),
        ..Default::default()
    };
    let mut engine = Engine::new(config).unwrap();
    assert!(engine.start_input(&path));
    engine.begin_run().unwrap();
    for index in 0..1000 {
        println!("before {index}");
        std::io::stdout().flush().unwrap();
        let result = engine.advance_run(8).unwrap();
        println!("after {index} {result:?}");
        if result != RunProgress::Yielded {
            panic!("Recursive source unexpectedly settled");
        }
    }
}
