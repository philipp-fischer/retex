//! Standalone per-job command line with PDF, format and diagnostic exports.
use crate::{
    core::{
        characters::LanguageProfile,
        config::EngineConfig,
        constants::Constants as C,
        errors::{Error, Result},
    },
    engine::Engine,
};
use std::path::{Path, PathBuf};
pub fn print_banner() {
    println!("{}", C::BANNER)
}
pub fn print_version_info() {
    println!(
        "pdfTeX version {}.{}",
        C::PDFTEX_VERSION,
        C::PDFTEX_REVISION
    );
    println!("e-TeX version {}{}", C::ETEX_VERSION, C::ETEX_REVISION)
}
#[derive(Default)]
struct Options {
    config: EngineConfig,
    input: String,
    output: Option<String>,
    verbose: bool,
    debug_errors: bool,
    debug_tokens: bool,
    debug_commands: bool,
    debug_nodes: bool,
    debug_pdf: bool,
    debug_all: bool,
    layout: Option<String>,
    trace: Option<String>,
    no_pdf: bool,
    format: Option<String>,
    dump_format: Option<String>,
}
fn parse(args: &[String]) -> Result<Option<Options>> {
    let mut options = Options::default();
    let mut positionals = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let split = arg.split_once('=');
        let flag = split.map_or(arg.as_str(), |(flag, _)| flag);
        let mut value = || -> Result<String> {
            if let Some((_, value)) = split {
                return Ok(value.into());
            }
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| Error::value(format!("Missing value for {flag}")))
        };
        match flag {
            "-h" | "--help" => {
                println!(
                    "Standalone Rust TeX-to-PDF engine\nUsage: retex-rs2 INPUT [OUTPUT.pdf] [OPTIONS]\n  --ini  --format FORMAT  --dump-format FORMAT\n  --exact-repro  --language-profile pdftex|unicode\n  --dump-layout JSON  --trace JSONL  --no-pdf\n  --font-dir PATH  --encoding-file PATH\n  --tex-input-root PATH  --configuration-path PATH\n  --progname NAME  --output-directory PATH\n  --event-limit N  --expansion-limit N  --capacity NAME=INTEGER\n  --interaction batchmode|nonstopmode|scrollmode|errorstopmode\n  --8bit  --greedy-paragraphs  -v  --debug-all\n  --debug-tokens  --debug-commands  --debug-nodes  --debug-pdf  --debug-errors"
                );
                return Ok(None);
            }
            "--version" => {
                print_banner();
                print_version_info();
                return Ok(None);
            }
            "-v" | "--verbose" => options.verbose = true,
            "--exact-repro" => options.config.exact_repro = true,
            "--debug-errors" => options.debug_errors = true,
            "--debug-tokens" => options.debug_tokens = true,
            "--debug-commands" => options.debug_commands = true,
            "--debug-nodes" => options.debug_nodes = true,
            "--debug-pdf" => options.debug_pdf = true,
            "--debug-all" => options.debug_all = true,
            "--dump-layout" => options.layout = Some(value()?),
            "--trace" => options.trace = Some(value()?),
            "--no-pdf" => options.no_pdf = true,
            "--recovery" => {
                options.config.recovery_policy = match value()?.as_str() {
                    "strict" => crate::core::recovery::RecoveryPolicy::Strict,
                    "classical" => crate::core::recovery::RecoveryPolicy::Classical,
                    "interactive" => crate::core::recovery::RecoveryPolicy::Interactive,
                    _ => {
                        return Err(Error::value(
                            "Recovery must be strict, classical, or interactive",
                        ));
                    }
                };
            }
            "--recovery-limit" => {
                options.config.recovery_limit = value()?
                    .parse()
                    .map_err(|_| Error::value("Invalid recovery limit"))?
            }
            "--ini" => options.config.ini = true,
            "--8bit" => options.config.eight_bit_output = true,
            "--greedy-paragraphs" => options.config.greedy_paragraphs = true,
            "--format" => options.format = Some(value()?),
            "--dump-format" => options.dump_format = Some(value()?),
            "--font-dir" => options.config.font_dir = Some(value()?),
            "--encoding-file" => options.config.encoding_file = Some(value()?),
            "--progname" => options.config.program_name = value()?,
            "--output-directory" => options.config.output_directory = Some(value()?),
            "--tex-input-root" => options
                .config
                .tex_input_roots
                .get_or_insert_with(Vec::new)
                .push(value()?),
            "--configuration-path" => options
                .config
                .configuration_paths
                .get_or_insert_with(Vec::new)
                .push(value()?),
            "--event-limit" => {
                options.config.event_limit = value()?
                    .parse()
                    .map_err(|_| Error::value("--event-limit must be positive"))?
            }
            "--expansion-limit" => {
                options.config.expansion_limit = Some(
                    value()?
                        .parse()
                        .map_err(|_| Error::value("--expansion-limit must be positive"))?,
                )
            }
            "--capacity" => {
                let entry = value()?;
                let (name, number) = entry
                    .split_once('=')
                    .ok_or_else(|| Error::value("--capacity expects NAME=INTEGER"))?;
                options.config.capacities.push((
                    name.into(),
                    number
                        .parse()
                        .map_err(|_| Error::value("--capacity expects NAME=INTEGER"))?,
                ));
            }
            "--interaction" => {
                let mode = value()?;
                options.config.interaction_mode = Some(
                    ["batchmode", "nonstopmode", "scrollmode", "errorstopmode"]
                        .iter()
                        .position(|&name| name == mode)
                        .ok_or_else(|| Error::value("Invalid interaction mode"))?
                        as u8,
                )
            }
            "--language-profile" => {
                options.config.language_profile = LanguageProfile::parse(&value()?)?
            }
            "--" => {
                i += 1;
                positionals.extend(args[i..].iter().cloned());
                break;
            }
            _ => {
                if arg.starts_with('-') && arg != "-" {
                    return Err(Error::value(format!("Unknown option: {arg}")));
                }
                positionals.push(arg.clone());
            }
        }
        i += 1;
    }
    if positionals.is_empty() || positionals.len() > 2 {
        return Err(Error::value(
            "Usage: retex-rs2 INPUT [OUTPUT.pdf] [OPTIONS]",
        ));
    }
    options.input = positionals.remove(0);
    options.output = positionals.pop();
    if options.dump_format.is_some() && !options.config.ini {
        return Err(Error::value("--dump-format requires --ini"));
    }
    if options.format.is_some() && options.config.ini {
        return Err(Error::value("--format and --ini cannot be combined"));
    }
    options.config.ini |= options.format.is_some();
    options.config.trace_enabled = options.trace.is_some() || options.layout.is_some();
    options.config.validate()?;
    Ok(Some(options))
}
pub fn main(args: &[String]) -> i32 {
    let options = match parse(args) {
        Ok(Some(options)) => options,
        Ok(None) => return 0,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };
    let mut engine = match Engine::new(options.config) {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };
    engine.core.diagnostics.debug = options.verbose
        || options.debug_all
        || options.debug_tokens
        || options.debug_commands
        || options.debug_nodes
        || options.debug_pdf;
    engine.core.diagnostics.debug_tokens = options.debug_all || options.debug_tokens;
    engine.core.diagnostics.debug_commands = options.debug_all || options.debug_commands;
    engine.core.diagnostics.debug_nodes = options.debug_all || options.debug_nodes;
    engine.core.diagnostics.debug_pdf = options.debug_all || options.debug_pdf;
    let output = options.output.clone().unwrap_or_else(|| {
        if options.input == "-" {
            "texput.pdf".into()
        } else {
            Path::new(&options.input)
                .with_extension("pdf")
                .to_string_lossy()
                .into_owned()
        }
    });
    let output_directory = engine
        .core
        .input
        .resources
        .search
        .policy
        .output_directory
        .clone();
    let output = if let Some(dir) = &output_directory {
        dir.join(options.output.as_deref().map(Path::new).unwrap_or_else(|| {
            Path::new(&output)
                .file_name()
                .map(Path::new)
                .unwrap_or(Path::new("texput.pdf"))
        }))
    } else {
        PathBuf::from(output)
    };
    print_banner();
    let result = (|| {
        if let Some(format) = &options.format {
            engine.core.processing_phase = "format".into();
            engine.load_format(Path::new(format))?;
        }
        engine.core.processing_phase = "initialization".into();
        let started = if options.input == "-" {
            engine.start_terminal()
        } else {
            engine.start_input(Path::new(&options.input))
        };
        if !started {
            return Err(Error::value(format!(
                "Cannot open {}: {}",
                options.input,
                engine
                    .core
                    .input
                    .last_input_error
                    .as_deref()
                    .unwrap_or("unknown input error")
            )));
        }
        engine.main_body();
        if engine.status() == "complete" && engine.core.format_snapshot.is_some() {
            let target = options.dump_format.clone().unwrap_or_else(|| {
                Path::new(&options.input)
                    .with_extension("rsfmt")
                    .to_string_lossy()
                    .into_owned()
            });
            let target = if let Some(dir) = &output_directory {
                dir.join(
                    options
                        .dump_format
                        .as_deref()
                        .map(Path::new)
                        .unwrap_or_else(|| {
                            Path::new(&target)
                                .file_name()
                                .map(Path::new)
                                .unwrap_or(Path::new("texput.rsfmt"))
                        }),
                )
            } else {
                PathBuf::from(target)
            };
            engine.write_format(&target)?;
        } else if options.dump_format.is_some() && engine.status() == "complete" {
            return Err(Error::value("Initialization ended without a dump command"));
        }
        if !options.no_pdf && engine.status() == "complete" && engine.core.format_snapshot.is_none()
        {
            engine.core.processing_phase = "emission".into();
            engine.write_pdf(&output)?;
        }
        Ok(())
    })();
    let mut exit = if engine.status() == "complete" && !engine.recovered() {
        0
    } else {
        1
    };
    if let Err(error) = result {
        engine.record_failure(error);
        engine.core.input.close_files_and_terminate();
        exit = 1;
    }
    let diagnostics: Result<()> = (|| {
        if let Some(path) = &options.layout {
            engine.dump_layout(Path::new(path))?;
        }
        if let Some(path) = &options.trace {
            engine
                .core
                .trace
                .write_events(Path::new(path), engine.status())?;
        }
        Ok(())
    })();
    if let Err(error) = diagnostics {
        eprintln!("Cannot write diagnostics: {error}");
        exit = 1;
    }
    if options.debug_errors {
        if let Some(error) = &engine.core.failure {
            if let Some(cause) = &error.cause {
                eprintln!("{}: {}", cause.kind.exception_type(), cause.message);
            }
            eprintln!(
                "Traceback (most recent call last):\nNative Rust frames captured at the failure boundary:\n{error:#?}\n{}\nProcessingError: {}",
                std::backtrace::Backtrace::force_capture(),
                error.message
            );
        }
    }
    exit
}
