//! Classic font map grammar and incremental declaration policy (`fonts/maps.py`).
use super::subfonts;
use crate::core::errors::{Error, Result};
use num_bigint::BigInt;
use num_traits::{ToPrimitive, Zero};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

// Fixed grammar compilation was the dominant actual Wasm font-map cost.
// These immutable matchers preserve the same pattern, Unicode classes and
// replacement grammar; map declarations and resource effects remain job-local.
static PID_GRAMMAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(PidEid=[^\s,]+,)\s+").expect("map pid grammar"));
static TOKEN_GRAMMAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""[^"]*"|[^\s"]+"#).expect("map token grammar"));
static TRANSFORM_GRAMMAR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?)\s*(SlantFont|ExtendFont)")
        .expect("map transform grammar")
});
static SUBFONT_GRAMMAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([^@]+)@([^@]+)@$").expect("subfont name grammar"));
pub(super) fn subfont_pattern() -> &'static Regex {
    &SUBFONT_GRAMMAR
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FontMapEntry {
    pub tfm_name: String,
    pub ps_name: Option<String>,
    pub encoding: Option<String>,
    pub fontfile: Option<String>,
    pub options: Option<String>,
    pub flags: Option<i64>,
    pub included: bool,
    pub subset: bool,
    pub slant: i64,
    pub extend: i64,
    pub pid_eid: Option<(i64, i64)>,
    pub subfont: Option<Vec<i32>>,
    pub directory: Option<String>,
}
pub type Resolver = std::rc::Rc<dyn Fn(&str, &Path, &str) -> Option<PathBuf>>;
#[derive(Clone)]
pub struct MapFileParser {
    pub fonts: crate::core::persistence::SharedTable<String, FontMapEntry>,
    pub resolver: Option<Resolver>,
    pub subfont_definitions: crate::core::persistence::SharedTable<String, Vec<(String, Vec<i32>)>>,
    pub suppress_duplicates: std::rc::Rc<dyn Fn() -> bool>,
}
impl Default for MapFileParser {
    fn default() -> Self {
        Self {
            fonts: Default::default(),
            resolver: None,
            subfont_definitions: Default::default(),
            suppress_duplicates: std::rc::Rc::new(|| false),
        }
    }
}
impl MapFileParser {
    pub fn parse(
        &mut self,
        path: &Path,
        mode: &str,
        warn: &mut dyn FnMut(&str),
        used: &HashSet<String>,
    ) -> Result<bool> {
        let bytes = match crate::resources::host::read(path) {
            Ok(v) => v,
            Err(_) => return Ok(false),
        };
        let text: String = bytes.iter().map(|&c| char::from(c)).collect();
        for line in text.lines() {
            self.add(line, mode, warn, used, path.parent())?;
        }
        Ok(true)
    }
    pub fn add(
        &mut self,
        line: &str,
        mode: &str,
        warn: &mut dyn FnMut(&str),
        used: &HashSet<String>,
        directory: Option<&Path>,
    ) -> Result<()> {
        let entry = match Self::parse_map_line(line) {
            Ok(Some(entry)) => entry,
            Ok(None) => return Ok(()),
            Err(error) => {
                warn(&error.to_string());
                return Ok(());
            }
        };
        let pattern = subfont_pattern();
        if let (Some(pattern), Some(resolver)) = (pattern.captures(&entry.tfm_name), &self.resolver)
        {
            let (prefix, name) = (&pattern[1], &pattern[2]);
            if !self.subfont_definitions.contains_key(name) {
                let cwd = crate::resources::host::current_dir()?;
                if let Some(path) =
                    resolver(&format!("{name}.sfd"), directory.unwrap_or(&cwd), "sfd")
                {
                    self.subfont_definitions
                        .insert(name.into(), subfonts::read_definitions(path)?);
                } else {
                    warn(&format!("Cannot open SFD file for reading: {name}"));
                }
            }
            if let Some(definitions) = self.subfont_definitions.get(name).cloned() {
                for (infix, mapping) in definitions.into_iter().rev() {
                    let mut expanded = entry.clone();
                    expanded.tfm_name = format!("{prefix}{infix}");
                    expanded.subfont = Some(mapping);
                    expanded.pid_eid = entry.pid_eid.or(Some((3, 1)));
                    expanded.ps_name = None;
                    expanded.flags = None;
                    self.insert(expanded, mode, warn, used, directory)?;
                }
                return Ok(());
            }
        }
        self.insert(entry, mode, warn, used, directory)
    }
    pub fn insert(
        &mut self,
        mut entry: FontMapEntry,
        mode: &str,
        warn: &mut dyn FnMut(&str),
        used: &HashSet<String>,
        directory: Option<&Path>,
    ) -> Result<()> {
        let name = &entry.tfm_name;
        if self.fonts.contains_key(name) {
            if mode == "+" {
                if !(self.suppress_duplicates)() {
                    warn(&format!(
                        "fontmap entry for `{name}' already exists, duplicates ignored"
                    ));
                }
                return Ok(());
            }
            if used.contains(name) {
                warn(&format!(
                    "fontmap entry for `{name}' has been used, replace/delete not allowed"
                ));
                return Ok(());
            }
        }
        if mode == "-" {
            self.fonts.remove(name);
        } else {
            entry.directory = Some(
                directory
                    .map(Path::to_path_buf)
                    .unwrap_or(crate::resources::host::current_dir()?)
                    .to_string_lossy()
                    .into_owned(),
            );
            self.fonts.insert(entry.tfm_name.clone(), entry);
        }
        Ok(())
    }
    pub fn parse_map_line(line: &str) -> Result<Option<FontMapEntry>> {
        let line = line.trim();
        if line.is_empty() || "%*;#".contains(line.chars().next().expect("nonempty map line")) {
            return Ok(None);
        }
        let tidy = PID_GRAMMAR.replace_all(line, "$1").into_owned();
        if tidy.chars().filter(|&c| c == '"').count() % 2 != 0 {
            return Err(Error::value("Invalid fontmap entry: closing quote missing"));
        }
        let tokens = &TOKEN_GRAMMAR;
        let mut parts: Vec<_> = tokens.find_iter(&tidy).map(|m| m.as_str()).collect();
        let mut entry = FontMapEntry {
            tfm_name: parts.remove(0).into(),
            ..FontMapEntry::default()
        };
        if parts.first().is_some_and(|p| {
            !p.chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit() || c == '<' || c == '"')
        }) {
            entry.ps_name = Some(parts.remove(0).into());
        }
        if parts
            .first()
            .is_some_and(|p| p.chars().all(|c| c.is_ascii_digit()))
        {
            entry.flags = Some(
                parts
                    .remove(0)
                    .parse()
                    .map_err(|_| Error::value("Invalid fontmap flags"))?,
            );
        }
        let mut prefix = "";
        let transforms = &TRANSFORM_GRAMMAR;
        for part in parts {
            if let Some(pair) = part.strip_prefix("PidEid=") {
                let values: Vec<_> = pair.split(',').collect();
                if values.len() != 2 {
                    return Err(Error::value("Invalid fontmap PidEid"));
                }
                entry.pid_eid =
                    Some((subfonts::integer(values[0])?, subfonts::integer(values[1])?));
                continue;
            }
            if part.starts_with('"') {
                let options = &part[1..part.len() - 1];
                for matched in transforms.captures_iter(options) {
                    let value = decimal_milli(&matched[1])?;
                    if &matched[2] == "SlantFont" {
                        entry.slant = value;
                    } else {
                        entry.extend = if value == 1000 { 0 } else { value };
                    }
                }
                entry.options = Some(options.into());
                continue;
            }
            if ["<", "<<", "<["].contains(&part) {
                prefix = part;
                continue;
            }
            let joined = format!("{prefix}{part}");
            prefix = "";
            let (marker, filename) = if let Some(name) = joined.strip_prefix("<<") {
                ("<<", name)
            } else if let Some(name) = joined.strip_prefix("<[") {
                ("<[", name)
            } else if let Some(name) = joined.strip_prefix('<') {
                ("<", name)
            } else {
                ("", joined.as_str())
            };
            if filename.to_ascii_lowercase().ends_with(".enc") {
                entry.encoding = Some(filename.into());
            } else if !filename.is_empty() {
                entry.fontfile = (!marker.is_empty()).then(|| filename.into());
                entry.included = !marker.is_empty();
                entry.subset = marker == "<";
            }
        }
        if entry.slant.unsigned_abs() > 1000 || entry.extend.unsigned_abs() > 2000 {
            return Err(Error::value(format!(
                "Invalid fontmap entry for `{0}': unreasonable SlantFont/ExtendFont",
                entry.tfm_name
            )));
        }
        let extension = Path::new(entry.fontfile.as_deref().unwrap_or(""))
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if (entry.slant != 0 || entry.extend != 0)
            && (!entry.included || !["pfb", "pfa"].contains(&extension.as_str()))
        {
            return Err(Error::value(format!(
                "Invalid fontmap entry for `{0}': SlantFont/ExtendFont require embedded Type1",
                entry.tfm_name
            )));
        }
        if (entry.pid_eid.is_some() || subfont_pattern().is_match(&entry.tfm_name))
            && (!entry.subset
                || entry.encoding.is_some()
                || !["ttf", "ttc"].contains(&extension.as_str()))
        {
            return Err(Error::value(
                "PidEid/subfonts require subsetted non-reencoded TrueType fonts",
            ));
        }
        Ok(Some(entry))
    }
}

/// Exact decimal scaling with ROUND_HALF_UP; binary floats change tie cases.
fn decimal_milli(text: &str) -> Result<i64> {
    let negative = text.starts_with('-');
    let text = text.trim_start_matches(['+', '-']);
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(at) => (
            &text[..at],
            text[at + 1..]
                .parse::<i64>()
                .map_err(|_| Error::value("Invalid fontmap transformation"))?,
        ),
        None => (text, 0),
    };
    let decimals = mantissa
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len() as i64);
    let digits = mantissa.replace('.', "");
    let coefficient = BigInt::parse_bytes(digits.as_bytes(), 10)
        .ok_or_else(|| Error::value("Invalid fontmap transformation"))?;
    let power = exponent.saturating_add(3).saturating_sub(decimals);
    if coefficient.is_zero() {
        return Ok(0);
    }
    if power > 19 {
        return Err(Error::value("Invalid fontmap transformation"));
    }
    if power < -(digits.len() as i64 + 1) {
        return Ok(0);
    }
    let value = if power >= 0 {
        coefficient * BigInt::from(10_u8).pow(power as u32)
    } else {
        let divisor = BigInt::from(10_u8).pow((-power) as u32);
        let quotient = &coefficient / &divisor;
        let remainder = coefficient % &divisor;
        quotient + BigInt::from(u8::from(2 * remainder >= divisor))
    };
    let value = if negative { -value } else { value };
    value
        .to_i64()
        .ok_or_else(|| Error::value("Invalid fontmap transformation"))
}
