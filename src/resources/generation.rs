//! Native mktex resource requests (`resources/generation.py`).
use super::paths::{PathPolicy, resolve_path};
use crate::core::errors::{Error, Result};
use crate::language::random_numbers::{LOG_SCALE, Logarithm, logarithm};
use num_bigint::BigInt;
use num_traits::{FromPrimitive, Signed, ToPrimitive, Zero};
use regex::Regex;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
pub const NATIVE_UNSIGNED_MODULUS: i64 = 1_i64 << 32;

pub fn magstep_fix(dpi: &BigInt, base: &BigInt, exact_repro: bool) -> Result<(BigInt, i64)> {
    if dpi <= &BigInt::zero() || base <= &BigInt::zero() {
        return Err(Error::value(
            "Bitmap DPI and base DPI must be positive integers",
        ));
    }
    if exact_repro {
        return exact_magstep_fix(dpi, base);
    }
    let log_dpi = match logarithm(dpi, false)? {
        Logarithm::Modern(value) => value / LOG_SCALE as f64,
        _ => unreachable!(),
    };
    let log_base = match logarithm(base, false)? {
        Logarithm::Modern(value) => value / LOG_SCALE as f64,
        _ => unreachable!(),
    };
    let half_steps = (2.0 * (log_dpi - log_base) / 1.2_f64.ln()).round_ties_even() as i64;
    let exponent = u32::try_from(half_steps.unsigned_abs())
        .map_err(|_| Error::value("Magstep exponent exceeds host address range"))?;
    let (mut numerator, mut denominator) = (
        BigInt::from(6_u8).pow(exponent),
        BigInt::from(5_u8).pow(exponent),
    );
    if half_steps < 0 {
        std::mem::swap(&mut numerator, &mut denominator);
    }
    let squared = base * base * numerator;
    let mut candidate = integer_sqrt(&(&squared / &denominator));
    let twice_plus_one = &candidate * 2 + 1;
    if &squared * 4 >= &denominator * &twice_plus_one * &twice_plus_one {
        candidate += 1;
    }
    if (&candidate - dpi).abs() <= BigInt::from(1_u8) {
        Ok((candidate, half_steps))
    } else {
        Ok((dpi.clone(), 0))
    }
}
fn integer_sqrt(value: &BigInt) -> BigInt {
    if value.is_zero() {
        return BigInt::zero();
    }
    let mut guess = BigInt::from(1_u8) << value.bits().div_ceil(2) as usize;
    loop {
        let next = (&guess + value / &guess) / 2;
        if next >= guess {
            return guess;
        }
        guess = next;
    }
}
pub fn exact_magstep_fix(dpi: &BigInt, base: &BigInt) -> Result<(BigInt, i64)> {
    let sign = if dpi < base { -1_i64 } else { 1 };
    let mut real = BigInt::zero();
    let mut candidate = BigInt::zero();
    let mut last_step = 0;
    let base_float = base
        .to_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| Error::value("Historical bitmap DPI exceeds floating-point range"))?;
    for step in 0..40 {
        last_step = step;
        let mut whole = step / 2;
        let mut multiplier = if step % 2 != 0 { 1.095445115 } else { 1.0 };
        while whole > 4 {
            multiplier *= 2.0736;
            whole -= 4;
        }
        while whole != 0 {
            multiplier *= 1.2;
            whole -= 1;
        }
        let number = 0.5
            + if sign < 0 {
                base_float / multiplier
            } else {
                base_float * multiplier
            };
        candidate = BigInt::from_f64(number.trunc())
            .ok_or_else(|| Error::value("Historical bitmap DPI exceeds floating-point range"))?;
        if (&candidate - dpi).abs() <= BigInt::from(1_u8) {
            real = candidate.clone();
        } else if (&candidate - dpi) * sign > BigInt::zero() {
            real = dpi.clone();
        }
        if !real.is_zero() {
            break;
        }
    }
    let encoded = if candidate.is_zero() {
        0
    } else {
        last_step * sign
    }
    .rem_euclid(NATIVE_UNSIGNED_MODULUS);
    let reported = i64::from(real == BigInt::from(encoded));
    Ok((if real.is_zero() { dpi.clone() } else { real }, reported))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ResourceGeneration {
    pub exact_repro: bool,
}
impl ResourceGeneration {
    pub fn make(&self, policy: &mut PathPolicy, kind: &str, name: &str) -> Result<Option<PathBuf>> {
        let (program, default) = match kind {
            "tex" => ("mktextex", false),
            "tfm" => ("mktextfm", true),
            "pk" => ("mktexpk", true),
            _ => return Ok(None),
        };
        let enabled = policy.raw_value(&program.to_ascii_uppercase(), true);
        let mut enabled = if enabled.is_empty() {
            default
        } else {
            enabled.starts_with('1')
        };
        if kind == "pk" && crate::resources::host::var_os("PDFTEXMAKEPK").is_some() {
            enabled = true;
        }
        if !enabled
            || name.is_empty()
            || name.starts_with('-')
            || !Regex::new(r"^[A-Za-z0-9_+./-]+$")
                .expect("mktex filename grammar")
                .is_match(name)
        {
            return Ok(None);
        }
        let mut arguments = vec![program.to_string()];
        if !crate::resources::host::native_capabilities() {
            let mut error = Error::value(format!(
                "Host cannot run {program}; supply the generated {kind} resource '{name}' in the project catalog"
            ));
            if kind == "tfm" {
                error = error.with_missing_resource(crate::core::errors::MissingResource {
                    kind: kind.into(),
                    name: format!("{name}.{kind}").into_bytes(),
                    directory: None,
                });
            }
            return Err(error);
        }
        if kind == "pk" {
            let number = |key: &str| -> Result<BigInt> {
                policy
                    .runtime_variables
                    .get(key)
                    .and_then(|value| value.parse().ok())
                    .ok_or_else(|| Error::value(format!("Invalid bitmap variable {key}")))
            };
            let dpi = number("KPATHSEA_DPI")?;
            let base = number("MAKETEX_BASE_DPI")?;
            let (_, step) = magstep_fix(&dpi, &base, self.exact_repro)?;
            let magnitude = if step != 0 {
                let absolute = step.unsigned_abs();
                format!(
                    "magstep\\({}{}.{}\\)",
                    if step < 0 { "-" } else { "" },
                    absolute / 2,
                    if absolute % 2 != 0 { 5 } else { 0 }
                )
            } else if base <= BigInt::from(4000) {
                format!("{}+{}/{}", &dpi / &base, &dpi % &base, base)
            } else {
                let factor = &base / 4000;
                let remainder: BigInt = &base % 4000;
                let denominator = if factor > BigInt::from(1) {
                    format!(
                        "{factor}*{}{}",
                        (&base - &remainder) / &factor,
                        if remainder.is_zero() {
                            String::new()
                        } else {
                            format!("+{remainder}")
                        }
                    )
                } else {
                    format!("4000+{remainder}")
                };
                format!("{}+{}/({denominator})", &dpi / &base, &dpi % &base)
            };
            policy
                .runtime_variables
                .insert("MAKETEX_MAG".into(), magnitude.clone());
            let mode = policy
                .runtime_variables
                .get("MAKETEX_MODE")
                .cloned()
                .ok_or_else(|| Error::value("Missing bitmap mode"))?;
            arguments.extend([
                "--mfmode".into(),
                mode,
                "--bdpi".into(),
                base.to_string(),
                "--mag".into(),
                magnitude,
                "--dpi".into(),
                dpi.to_string(),
            ]);
        }
        arguments.push(name.into());
        crate::resources::host::console_line(
            true,
            format!("kpathsea: Running {}", arguments.join(" ")),
        );
        let result = Command::new(program)
            .args(&arguments[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .envs(&policy.runtime_variables)
            .output();
        match result {
            Ok(result) => {
                let mut bytes = result.stdout;
                while bytes
                    .last()
                    .is_some_and(|byte| *byte == b'\r' || *byte == b'\n')
                {
                    bytes.pop();
                }
                if let Some(at) = bytes.iter().position(|&byte| byte == 0) {
                    bytes.truncate(at);
                }
                if !bytes.is_empty() {
                    #[cfg(unix)]
                    let filename = {
                        use std::os::unix::ffi::OsStringExt;
                        std::ffi::OsString::from_vec(bytes)
                    };
                    #[cfg(not(unix))]
                    let filename =
                        std::ffi::OsString::from(String::from_utf8_lossy(&bytes).into_owned());
                    let path = PathBuf::from(filename);
                    if crate::resources::host::is_file(&path)
                        && crate::resources::host::open_read(&path).is_ok()
                    {
                        return Ok(Some(resolve_path(&path)));
                    }
                    crate::resources::host::console_line(
                        true,
                        format!(
                            "kpathsea: {program} output `{}' instead of a filename",
                            path.display()
                        ),
                    );
                }
            }
            Err(error) => crate::resources::host::console_line(true, format!("{program}: {error}")),
        }
        if ["tfm", "pk"].contains(&kind) {
            self.log_missing(policy, &arguments);
        }
        Ok(None)
    }
    pub fn log_missing(&self, policy: &PathPolicy, arguments: &[String]) {
        let filename = policy.raw_value("MISSFONT_LOG", true);
        if filename.starts_with('0') {
            return;
        }
        let filename = if filename.is_empty() || filename.starts_with('1') {
            "missfont.log".into()
        } else {
            filename
        };
        let name = PathBuf::from(filename);
        let primary = if !crate::resources::host::is_absolute(&name) {
            policy
                .output_directory
                .as_ref()
                .map_or_else(|| name.clone(), |root| root.join(&name))
        } else {
            name.clone()
        };
        let mut targets = vec![primary];
        let fallback = policy.raw_value("TEXMFOUTPUT", true);
        if !fallback.is_empty() {
            targets.push(PathBuf::from(fallback).join(&name));
        }
        let data: Vec<_> = (arguments.join(" ") + "\n")
            .chars()
            .filter_map(|c| u8::try_from(c as u32).ok())
            .collect();
        for target in targets {
            if let Ok(mut stream) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(target)
            {
                if stream.write_all(&data).is_ok() {
                    return;
                }
            }
        }
    }
}
