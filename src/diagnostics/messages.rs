//! Human errors, warning history and live TeX tracing controls.
use crate::{
    core::{
        errors::{Diagnostic, Error, Result},
        source::SourceSpan,
    },
    fonts::model::FontInfo,
};
use serde_json::{Map, Value};
use std::sync::Arc;
pub const TEX_TRACE_CONTROLS: &[(&str, &str)] = &[
    ("expansion", "tracingmacros"),
    ("command", "tracingcommands"),
    ("group_begin", "tracinggroups"),
    ("group_end", "tracinggroups"),
    ("conditional", "tracingifs"),
    ("definition", "tracingassigns"),
    ("assignment", "tracingassigns"),
    ("restore", "tracingrestores"),
    ("file_open", "tracingnesting"),
    ("rescan", "tracingscantokens"),
    ("paragraph_line", "tracingparagraphs"),
    ("page_break_candidate", "tracingpages"),
    ("shipout", "tracingoutput"),
    ("run_end", "tracingstats"),
];

/// LaTeX includes terminal-response instructions in otherwise useful messages.
/// ReTeX reports diagnostics and recovers or fails without asking for input.
pub fn noninteractive_message(text: String) -> String {
    fn prompt(line: &str) -> bool {
        let line = line.trim().to_ascii_lowercase();
        (line.contains("<return>")
            && ["type", "typing", "press", "hit", "proceed", "continue"]
                .iter()
                .any(|word| line.contains(word)))
            || line.starts_with("or enter new name")
            || line.starts_with("enter file name:")
            || line.starts_with("please type another input file name")
            || line.starts_with("please type another output file name")
            || (line.starts_with("type ")
                && [" to quit", " to exit", " for help", " for immediate help"]
                    .iter()
                    .any(|word| line.contains(word)))
    }
    if !text.lines().any(prompt) {
        return text;
    }
    text.lines()
        .filter(|line| !prompt(line))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_owned()
}
#[derive(Clone, Default)]
pub struct Diagnostics {
    pub debug: bool,
    pub debug_tokens: bool,
    pub debug_commands: bool,
    pub debug_nodes: bool,
    pub debug_pdf: bool,
    pub errors: Vec<String>,
    pub records: Vec<Diagnostic>,
    pub warnings: Vec<String>,
    pub help: Vec<String>,
    pub record_limit: Option<usize>,
    pub dropped: usize,
    pub terminal_log: Arc<String>,
    pub terminal_log_truncated: bool,
}
impl Diagnostics {
    pub fn print_err(
        &mut self,
        error: &Error,
        interaction: u8,
        source: Option<Arc<SourceSpan>>,
        help: Option<String>,
        context_lines: i64,
    ) {
        if error.kind == crate::core::errors::ErrorKind::Processing {
            self.records.push(error.diagnostic())
        }
        self.errors.push(error.message.clone());
        if let Some(help) = help
            .map(noninteractive_message)
            .filter(|help| !help.is_empty())
        {
            self.help.push(help)
        }
        if interaction == 0 {
            return;
        }
        crate::resources::host::console_line(true, format!("! {}", error.message));
        if let Some(help) = self.help.last() {
            crate::resources::host::console_line(true, format!("{help}"))
        }
        if let Some(mut source) = error.source.clone().or(source) {
            crate::resources::host::console_line(
                true,
                format!("{}:{}:{}", source.file, source.line, source.column),
            );
            let mut remaining = context_lines.max(0);
            while remaining > 0 {
                let Some(call) = source.call_site.clone() else {
                    break;
                };
                source = call;
                crate::resources::host::console_line(
                    true,
                    format!("  from {}:{}:{}", source.file, source.line, source.column),
                );
                remaining -= 1
            }
        }
    }
    pub fn collect_terminal(&mut self, text: &str) {
        if self.record_limit.is_none() {
            return;
        }
        let remaining = 131072usize.saturating_sub(self.terminal_log.len());
        if remaining == 0 {
            self.terminal_log_truncated = true;
            return;
        }
        let mut end = remaining.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let log = Arc::make_mut(&mut self.terminal_log);
        log.push_str(&text[..end]);
        if end < text.len() {
            self.terminal_log_truncated = true;
        }
        if log.len() < 131072 {
            log.push('\n');
        }
    }
    pub fn collect_warning(
        &mut self,
        message: &str,
        source: Option<Arc<SourceSpan>>,
        package: bool,
    ) {
        use crate::core::recovery::{DiagnosticDetails, Issue};
        if let Some(limit) = self.record_limit {
            if self.warnings.len() >= limit || self.records.len() >= limit {
                self.dropped += 1;
                return;
            }
            let message: String = message.chars().take(8192).collect();
            self.warnings.push(message.clone());
            if !self.records.last().is_some_and(|r| {
                r.message == message && r.severity == "warning" && r.source == source
            }) {
                let issue = if package {
                    Issue::PackageWarning
                } else if message.starts_with("Overfull")
                    || message.starts_with("Underfull")
                    || message.starts_with("Tight")
                {
                    Issue::LayoutWarning
                } else {
                    Issue::ResourceWarning
                };
                self.records.push(Diagnostic {
                    severity: "warning".into(),
                    message,
                    phase: if package { "package" } else { "typesetting" }.into(),
                    exception_type: "Warning".into(),
                    source,
                    cause_type: None,
                    cause_message: None,
                    details: Some(Arc::new(DiagnosticDetails::new(issue, None))),
                });
            }
        } else {
            self.warnings.push(message.into());
        }
    }
    pub fn warning(&mut self, message: &str, interaction: u8) {
        self.collect_warning(message, None, false);
        if interaction != 0 {
            crate::resources::host::console_line(true, format!("{message}"))
        }
    }
    pub fn missing_character(
        &mut self,
        code: u32,
        font: &FontInfo,
        severity: i64,
        interaction: u8,
        source: Option<Arc<SourceSpan>>,
    ) -> Result<()> {
        if severity > 0 {
            if self
                .record_limit
                .is_some_and(|limit| self.records.len() >= limit || self.warnings.len() >= limit)
            {
                self.dropped += 1;
                return Ok(());
            }
            let message = format!("Missing character: code {code} in font {}", font.name);
            if severity >= 3 {
                return Err(Error::value(message).with_issue(crate::core::recovery::Issue::Glyph));
            }
            if self.record_limit.is_some() {
                self.collect_warning(&message, source, false);
                if let Some(record) = self.records.last_mut() {
                    record.details = Some(Arc::new(crate::core::recovery::DiagnosticDetails::new(
                        crate::core::recovery::Issue::Glyph,
                        None,
                    )));
                }
                if interaction != 0 {
                    crate::resources::host::console_line(true, &message);
                }
                return Ok(());
            }
            self.records.push(Diagnostic {
                severity: "warning".into(),
                message: message.clone(),
                phase: "typesetting".into(),
                exception_type: "Warning".into(),
                source,
                cause_type: None,
                cause_message: None,
                details: None,
            });
            self.warning(&message, interaction)
        }
        Ok(())
    }
    pub fn tex_event(
        &self,
        kind: &str,
        fields: &Map<String, Value>,
        interaction: u8,
        parameter: &dyn Fn(&str) -> i64,
    ) {
        let Some((_, control)) = TEX_TRACE_CONTROLS.iter().find(|(event, _)| *event == kind) else {
            return;
        };
        if parameter(control) == 0 || interaction == 0 {
            return;
        }
        let mut details = fields.clone();
        for key in ["source", "definition", "call"] {
            details.remove(key);
        }
        crate::resources::host::console_line(
            true,
            format!("TeX trace {kind}: {}", Value::Object(details)),
        );
    }
    pub fn debug_print(&self, message: &str, level: &str) {
        if self.debug {
            if level.is_empty() {
                crate::resources::host::console_line(true, format!("{message}"))
            } else {
                crate::resources::host::console_line(true, format!("[{level}] {message}"))
            }
        }
    }
    pub fn debug_command(&self, action: &str, details: &str) {
        if self.debug_commands {
            self.debug_print(&format!("{action} {details}"), "CMD")
        }
    }
    pub fn debug_node(&self, action: &str, details: &str) {
        if self.debug_nodes {
            self.debug_print(&format!("{action} {details}"), "NODE")
        }
    }
    pub fn debug_pdf(&self, action: &str, details: &str) {
        if self.debug_pdf {
            self.debug_print(&format!("{action} {details}"), "PDF")
        }
    }
    pub fn debug_token(
        &self,
        number: usize,
        input: &crate::language::input::InputProcessor,
        env: &crate::language::state::Environment,
    ) {
        if !self.debug_tokens {
            return;
        }
        let current = &input.current;
        let cs = current.control_sequence;
        let label = format!("{:?}", current.command);
        let detail = if cs > 0 {
            format!("\\{} (cs={cs})", env.cs_name(cs))
        } else {
            format!("{:?}", current.value)
        };
        self.debug_print(&format!("Token {number:4}: {label:15} {detail}"), "TOKEN");
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[test]
    fn noninteractive_diagnostics_keep_the_problem_and_explanation_without_prompts() {
        let message = "LaTeX Error: File `missing.cls' not found.\n\nType X to quit or <RETURN> to proceed,\nor enter new name. (Default extension: cls)\nEnter file name: ";
        assert_eq!(
            noninteractive_message(message.into()),
            "LaTeX Error: File `missing.cls' not found."
        );
        assert_eq!(
            noninteractive_message(
                "Package error.\nSee the package manual.\nType  H <return>  for immediate help"
                    .into()
            ),
            "Package error.\nSee the package manual."
        );
        assert_eq!(
            noninteractive_message("A useful warning about line breaks.".into()),
            "A useful warning about line breaks."
        );
        assert_eq!(
            noninteractive_message("The <return> token is documented here.".into()),
            "The <return> token is documented here."
        );
    }
    #[test]
    fn interactive_warning_floods_and_terminal_text_are_bounded() {
        let mut diagnostics = Diagnostics {
            record_limit: Some(3),
            ..Default::default()
        };
        for line in 1..=20 {
            diagnostics.collect_warning(
                "Package sample Warning: Repeated",
                Some(Arc::new(SourceSpan::new("/project/main.tex", line, 1))),
                true,
            );
        }
        assert_eq!(diagnostics.records.len(), 3);
        assert_eq!(diagnostics.dropped, 17);
        assert_ne!(diagnostics.records[0].source, diagnostics.records[1].source);
        diagnostics.collect_terminal(&"α".repeat(100000));
        assert!(diagnostics.terminal_log.len() <= 131072);
        assert!(diagnostics.terminal_log_truncated);
        assert!(std::str::from_utf8(diagnostics.terminal_log.as_bytes()).is_ok());
    }
}
