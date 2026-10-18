//! Isolated, bounded replay for errors that cannot resume a live continuation.
mod host;
mod structure;
use super::*;
use crate::core::{
    errors::Diagnostic,
    recovery::{DiagnosticDetails, Issue, RecoveryPolicy},
    source::SourceSpan,
};
use std::{collections::BTreeMap, path::PathBuf, rc::Rc};
struct Origin {
    template: Engine,
    path: PathBuf,
}
#[derive(Clone)]
pub(super) struct Replay {
    origin: Rc<Origin>,
    inputs: BTreeMap<PathBuf, Vec<u8>>,
    history: Vec<Diagnostic>,
    emergency_output: bool,
    plain_paragraphs: bool,
    finite_shrinkage: bool,
    masks: Vec<Arc<SourceSpan>>,
    placeholders: Vec<(String, String)>,
    attempts: usize,
}
impl Engine {
    pub(super) fn prepare_replay(&mut self, path: &Path) {
        if self.core.config.recovery_policy != RecoveryPolicy::Interactive || self.replay.is_some()
        {
            return;
        }
        if self.core.ready_already == 0 {
            if self.init_prim().is_err() {
                return;
            }
            self.core.ready_already = 314159;
        }
        let template = self.checkpoint_clone().ok().flatten();
        if let Some(template) = template {
            self.replay = Some(Replay {
                origin: Rc::new(Origin {
                    template,
                    path: path.to_path_buf(),
                }),
                inputs: BTreeMap::new(),
                history: Vec::new(),
                emergency_output: false,
                plain_paragraphs: false,
                finite_shrinkage: false,
                masks: Vec::new(),
                placeholders: Vec::new(),
                attempts: 0,
            });
        }
    }
    pub(crate) fn retry_recovery(&mut self, allow_missing: bool) -> Result<bool> {
        if self.core.config.recovery_policy != RecoveryPolicy::Interactive {
            return Ok(false);
        }
        let Some(error) = self.core.failure.clone() else {
            return Ok(false);
        };
        if error.missing_resource.is_some() && !allow_missing {
            return Ok(false);
        }
        let Some(issue) = error.details.as_ref().map(|d| d.issue).or_else(|| {
            error.missing_resource.as_ref().map(|r| {
                if r.kind.contains("font")
                    || ["tfm", "encoding", "sfd", "map", "outline"].contains(&r.kind.as_str())
                {
                    Issue::Font
                } else if r.kind.contains("image") {
                    Issue::Image
                } else {
                    Issue::MissingInput
                }
            })
        }) else {
            return Ok(false);
        };
        if matches!(
            issue,
            Issue::EngineFault | Issue::RecoveryLimit | Issue::ViewerFailure
        ) {
            return Ok(false);
        }
        let Some(mut replay) = self.replay.take() else {
            return Ok(false);
        };
        if replay.attempts >= 8 || self.core.input.recovery.count >= self.core.config.recovery_limit
        {
            self.replay = Some(replay);
            return Ok(false);
        }
        let mut source = error.source.clone().or_else(|| self.core.input.source());
        // Prefer a project invocation over package implementation or a runaway
        // macro's definition. This is a diagnostic anchor, not proven causality.
        let parent = replay.origin.path.parent().unwrap_or(Path::new("/"));
        let mut cursor = source.clone();
        let mut visited = 0;
        while let Some(s) = cursor {
            if Path::new(s.file.as_ref()).starts_with(parent) && s.file.ends_with(".tex") {
                source = Some(s.clone());
            }
            cursor = s.call_site.clone();
            visited += 1;
            if visited >= 64 {
                break;
            }
        }
        let Some(source) = source else {
            self.replay = Some(replay);
            return Ok(false);
        };
        let path = PathBuf::from(source.file.as_ref());
        if !path.starts_with(parent) || !path.extension().is_some_and(|e| e == "tex") {
            self.replay = Some(replay);
            return Ok(false);
        }
        let base = replay.origin.template.host.clone();
        let key = base.canonicalize(&path).unwrap_or(path);
        let original = base.read(&key)?;
        let bytes = replay
            .inputs
            .get(&key)
            .cloned()
            .unwrap_or_else(|| original.clone());
        let strategy_only = matches!(
            issue,
            Issue::OutputRoutine | Issue::FontExpansion | Issue::GlueShrinkage
        );
        let patch = if strategy_only {
            Some(structure::Patch{start:0,end:0,text:Vec::new(),placeholder:false,action:match issue {
                Issue::OutputRoutine=>"Used a simple body preview without the broken output routine; page furniture and pagination may differ",
                Issue::FontExpansion=>"Retried paragraph layout without font expansion; line breaks may differ",
                _=>"Converted infinite glue shrinkage to finite shrinkage in the recovered preview",
            }.into()})
        } else {
            structure::plan(&bytes, &source, issue, replay.attempts)
        };
        let Some(mut patch) = patch else {
            self.replay = Some(replay);
            return Ok(false);
        };
        let mut details = DiagnosticDetails::new(issue, Some(&patch.action));
        let mut related = structure::spans(&original, source.file.clone(), patch.start, patch.end);
        if related.is_empty() {
            related = structure::opener_spans(&original, source.file.clone(), issue);
        }
        details.related = related.clone();
        if patch.placeholder && patch.end - patch.start >= 9 {
            let name = format!("RTerr{}", char::from(b'A' + replay.attempts as u8));
            let marker = format!("\\{name} ");
            // A placeholder definition has generated provenance; its letters
            // never claim to be the original bytes that were removed.
            let text = String::from_utf8_lossy(&bytes[patch.start..patch.end]);
            let dimension = |key: &str, default: f64| -> String {
                let pattern = format!(r"{}\s*(?:=\s*)?([0-9]+(?:\.[0-9]+)?)(pt|mm|cm|in)", key);
                let captures = regex::Regex::new(&pattern).ok().and_then(|re| {
                    re.captures(&text)
                        .map(|c| (c[1].to_string(), c[2].to_string()))
                });
                let value = captures
                    .and_then(|(n, u)| {
                        n.parse::<f64>().ok().map(|n| {
                            n * match u.as_str() {
                                "mm" => 72.27 / 25.4,
                                "cm" => 72.27 / 2.54,
                                "in" => 72.27,
                                _ => 1.0,
                            }
                        })
                    })
                    .unwrap_or(default)
                    .clamp(1.0, 600.0);
                format!("{value:.2}pt")
            };
            let width = dimension("width", 60.0);
            let height = dimension("height", 24.0);
            let body = format!(
                "\\hbox to {width}{{\\vrule width .4pt height {height} [error]\\hfil\\vrule width .4pt height {height}}}"
            );
            replay.placeholders.push((name, body));
            if marker.len() <= patch.text.len() {
                patch.text[..marker.len()].copy_from_slice(marker.as_bytes());
            }
        }
        let mut changed = bytes.clone();
        changed.splice(patch.start..patch.end, patch.text);
        if changed == bytes && !strategy_only {
            self.replay = Some(replay);
            return Ok(false);
        }
        replay.emergency_output |= issue == Issue::OutputRoutine;
        replay.plain_paragraphs |= issue == Issue::FontExpansion;
        replay.finite_shrinkage |= issue == Issue::GlueShrinkage;
        replay.inputs.insert(key, changed);
        replay.masks.extend(related);
        replay.attempts += 1;
        let mut record = error.diagnostic();
        record.source = Some(source);
        record.details = Some(Arc::new(details));
        replay.history = self
            .core
            .diagnostics
            .records
            .iter()
            .filter(|d| d.details.as_ref().is_some_and(|m| m.recovery.is_some()))
            .cloned()
            .collect();
        replay.history.push(record);
        let Some(mut fresh) = replay.origin.template.checkpoint_clone()? else {
            self.replay = Some(replay);
            return Ok(false);
        };
        fresh.host = Rc::new(host::ReplayHost::new(base, replay.inputs.clone()));
        // Keep evidence from discarded attempts, including warnings inside the
        // skipped region and secondary cleanup errors. Repeated prefixes are
        // explicitly separated by attempt; the normal terminal log cap applies.
        fresh.core.diagnostics.terminal_log = self.core.diagnostics.terminal_log.clone();
        fresh.core.diagnostics.terminal_log_truncated =
            self.core.diagnostics.terminal_log_truncated;
        fresh.core.diagnostics.collect_terminal(&format!(
            "[Failed attempt {}; replay follows]",
            replay.attempts
        ));
        for diagnostic in &self.core.diagnostics.records {
            fresh
                .core
                .diagnostics
                .collect_terminal(&format!("{}: {}", diagnostic.severity, diagnostic.message));
        }
        fresh.layout.recovery_placeholders = true;
        if replay.emergency_output {
            fresh.page_builder.output_handler = crate::typesetting::pages::OutputHandler::Direct;
        }
        if replay.plain_paragraphs {
            fresh.layout.greedy_paragraphs = true;
        }
        fresh.builder.recovery_finite_shrinkage = replay.finite_shrinkage;
        fresh.core.config.execution_reuse = false;
        fresh.core.config.grouped_execution_reuse = false;
        fresh.core.input.recovery.count = replay.history.len();
        fresh.core.input.recovery.pending = replay.history.clone();
        fresh.core.input.recovery.masks = replay.masks.clone();
        for (name, body) in &replay.placeholders {
            define_placeholder(&mut fresh.env, name, body)?;
        }
        let root = replay.origin.path.clone();
        fresh.replay = Some(replay);
        if !fresh.start_input(&root) {
            return Ok(false);
        }
        fresh.begin_run()?;
        *self = fresh;
        Ok(true)
    }
}
fn define_placeholder(env: &mut Environment, name: &str, body: &str) -> Result<()> {
    use crate::core::enums::CharacterCategory as Cat;
    use crate::language::{
        character_tokens::character_token,
        records::{CommandOperand, MeaningCommand},
        tokens::MacroDefinition,
    };
    let mut tokens = Vec::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let mut name = String::new();
            while chars.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                name.push(chars.next().unwrap());
            }
            if name.is_empty() {
                if let Some(c) = chars.next() {
                    name.push(c);
                }
            }
            tokens.push(Token::control_sequence(env.intern(&name, true)));
        } else {
            let cat = match c {
                '{' => Cat::LeftBrace,
                '}' => Cat::RightBrace,
                ' ' => Cat::Spacer,
                _ if c.is_ascii_alphabetic() => Cat::Letter,
                _ => Cat::OtherChar,
            };
            tokens.push(character_token(cat, c as i64)?);
        }
    }
    let cs = env.intern(name, true);
    env.define(
        cs,
        MeaningCommand::Operation(crate::core::enums::Command::Macro),
        CommandOperand::Macro(Arc::new(MacroDefinition {
            body: tokens.into(),
            parameters: None,
            protected: false,
            source: None,
            long: false,
            outer: false,
            body_sources: None,
        })),
        true,
    );
    Ok(())
}
