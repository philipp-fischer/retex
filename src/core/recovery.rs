//! Typed, bounded recovery decisions. No diagnostic work on a successful scan.
use super::{
    errors::{Diagnostic, Error, Result},
    source::SourceSpan,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryPolicy {
    #[default]
    Strict,
    Classical,
    Interactive,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Issue {
    UndefinedCommand,
    PackageError,
    InvalidCharacter,
    MissingNumber,
    BadNumber,
    MissingUnit,
    IncompatibleUnits,
    Arithmetic,
    MissingBrace,
    ExtraCloser,
    RunawayArgument,
    MacroDefinition,
    Conditional,
    MathDelimiter,
    MathStructure,
    Alignment,
    Mode,
    BoxOperand,
    Font,
    FontExpansion,
    Glyph,
    Image,
    MissingInput,
    PdfOperation,
    OutputRoutine,
    Hyphenation,
    GlueShrinkage,
    PageGeometry,
    Capacity,
    EngineFault,
    RecoveryLimit,
    PackageWarning,
    LayoutWarning,
    ResourceWarning,
    ViewerFailure,
}
impl Issue {
    pub fn code(self) -> &'static str {
        match self {
            Self::UndefinedCommand => "tex.undefined_command",
            Self::PackageError => "latex.error",
            Self::InvalidCharacter => "tex.invalid_character",
            Self::MissingNumber => "tex.missing_number",
            Self::BadNumber => "tex.number_range",
            Self::MissingUnit => "tex.missing_unit",
            Self::IncompatibleUnits => "tex.incompatible_units",
            Self::Arithmetic => "tex.arithmetic",
            Self::MissingBrace => "tex.missing_brace",
            Self::ExtraCloser => "tex.extra_closer",
            Self::RunawayArgument => "tex.runaway_argument",
            Self::MacroDefinition => "tex.macro_definition",
            Self::Conditional => "tex.conditional",
            Self::MathDelimiter => "tex.math_delimiter",
            Self::MathStructure => "tex.math_structure",
            Self::Alignment => "tex.alignment",
            Self::Mode => "tex.mode",
            Self::BoxOperand => "tex.box_operand",
            Self::Font => "resource.font",
            Self::FontExpansion => "tex.font_expansion",
            Self::Glyph => "tex.missing_glyph",
            Self::Image => "resource.image",
            Self::MissingInput => "resource.input",
            Self::PdfOperation => "pdf.operation",
            Self::OutputRoutine => "tex.output_routine",
            Self::Hyphenation => "tex.hyphenation",
            Self::GlueShrinkage => "tex.infinite_shrinkage",
            Self::PageGeometry => "tex.page_geometry",
            Self::Capacity => "engine.capacity",
            Self::EngineFault => "engine.fault",
            Self::RecoveryLimit => "engine.recovery_limit",
            Self::PackageWarning => "latex.warning",
            Self::LayoutWarning => "layout.spacing",
            Self::ResourceWarning => "resource.warning",
            Self::ViewerFailure => "viewer.failure",
        }
    }
    pub fn explanation(self) -> &'static str {
        match self {
            Self::UndefinedCommand => {
                "This command has no definition. Check its spelling and required packages."
            }
            Self::MissingNumber => {
                "A numeric value was required. A preceding command may be missing an argument."
            }
            Self::MissingUnit => "A length needs a unit, such as pt, mm, cm, or em.",
            Self::BadNumber => "This value is outside the range accepted by this operation.",
            Self::Arithmetic => "This calculation overflows, or divides by zero.",
            Self::PackageError => {
                "The document class or package reported an error. Its help and original message are retained."
            }
            Self::MissingBrace | Self::ExtraCloser => {
                "The grouping delimiters do not match. Check the opening group and its expected closer."
            }
            Self::RunawayArgument => {
                "TeX read an argument or definition beyond its intended boundary."
            }
            Self::Conditional => "The conditional delimiters or comparison operands do not match.",
            Self::MathDelimiter | Self::MathStructure => {
                "The formula contains an incomplete or incompatible construction."
            }
            Self::Alignment => "The table template or cell boundaries do not match.",
            Self::Font | Self::Glyph => {
                "A font resource or glyph is unavailable. A substitute may change layout."
            }
            Self::FontExpansion => {
                "Fonts in this paragraph have incompatible expansion settings. Check their expansion step and limits."
            }
            Self::GlueShrinkage => {
                "Glue in this context can only shrink by a finite amount. Replace infinite shrinkage with an explicit length."
            }
            Self::InvalidCharacter => {
                "This input character is invalid under the active encoding or category-code rules."
            }
            Self::IncompatibleUnits => {
                "Math glue uses mu units; ordinary dimensions use physical or font-relative units."
            }
            Self::MacroDefinition => {
                "The macro parameter specification or definition body is malformed. Check parameter numbering and doubled # markers in nested definitions."
            }
            Self::Mode => {
                "This operation cannot be used in the current horizontal, vertical, or mathematical context."
            }
            Self::BoxOperand => {
                "The operation requires a valid box or box register with the expected contents."
            }
            Self::Hyphenation => {
                "The hyphenation pattern or exception data is invalid for this language."
            }
            Self::PageGeometry => {
                "The page dimensions or placement are outside the supported range."
            }
            Self::Image => {
                "The included graphic could not be used. Other content can still be previewed."
            }
            Self::MissingInput => {
                "An input file is unavailable. Add it to the project or resolve the resource request."
            }
            Self::PdfOperation => {
                "This PDF operation could not be applied to the current output state."
            }
            Self::OutputRoutine => "The output routine failed to finish a page correctly.",
            Self::Capacity => {
                "An execution or storage budget was exceeded. Check for excessive recursion or a large document."
            }
            Self::EngineFault => {
                "The engine encountered inconsistent internal state. This execution cannot safely resume."
            }
            Self::RecoveryLimit => {
                "The recovery budget is exhausted. Fix an earlier error to reveal further diagnostics."
            }
            Self::LayoutWarning => {
                "This line or box has loose, compressed, or overflowing content. Inspect the highlighted region."
            }
            Self::ViewerFailure => {
                "The viewer could not paint this content. Compilation may still have succeeded."
            }
            _ => "Inspect the indicated source and the original engine message.",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuggestedEdit {
    pub title: String,
    pub source: Arc<SourceSpan>,
    pub replacement: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiagnosticDetails {
    pub issue: Issue,
    pub code: String,
    pub category: String,
    pub explanation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<Arc<SourceSpan>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixes: Vec<SuggestedEdit>,
    pub approximate: bool,
}
impl DiagnosticDetails {
    pub fn new(issue: Issue, action: Option<&str>) -> Self {
        let code = issue.code().to_string();
        Self {
            issue,
            category: code.split('.').next().unwrap_or("tex").into(),
            code,
            explanation: issue.explanation().into(),
            recovery: action.map(Into::into),
            related: Vec::new(),
            fixes: Vec::new(),
            approximate: action.is_some(),
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct RecoveryState {
    pub policy: RecoveryPolicy,
    pub pending: Vec<Diagnostic>,
    pub count: usize,
    pub limit: usize,
    pub masks: Vec<Arc<SourceSpan>>,
}
impl RecoveryState {
    pub fn new(policy: RecoveryPolicy, limit: usize) -> Self {
        Self {
            policy,
            limit,
            ..Self::default()
        }
    }
    pub fn enabled(&self) -> bool {
        self.policy != RecoveryPolicy::Strict
    }
    pub fn report(
        &mut self,
        issue: Issue,
        error: Error,
        source: Option<Arc<SourceSpan>>,
        action: &str,
    ) -> Result<()> {
        if !self.enabled() {
            return Err(error);
        }
        if self.count >= self.limit {
            return Err(
                Error::value("Recovery limit exceeded; fix an earlier error")
                    .with_details(DiagnosticDetails::new(Issue::RecoveryLimit, None)),
            );
        }
        self.count += 1;
        let source = error.source.clone().or(source);
        let mut record = error.capture("recovery", source).diagnostic();
        record.details = Some(Arc::new(DiagnosticDetails::new(issue, Some(action))));
        self.pending.push(record);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_preserves_failure_and_interactive_retains_range_and_budget() {
        let mut strict = RecoveryState::new(RecoveryPolicy::Strict, 2);
        assert!(
            strict
                .report(
                    Issue::MissingNumber,
                    Error::value("Missing number"),
                    None,
                    "Used zero"
                )
                .is_err()
        );
        assert_eq!(strict.count, 0);
        assert!(strict.pending.is_empty());
        let mut recovery = RecoveryState::new(RecoveryPolicy::Interactive, 1);
        let mut source = SourceSpan::new("/project/chapter.tex", 2, 3);
        source.byte_column = std::num::NonZeroUsize::new(3);
        source.end_byte_column = std::num::NonZeroUsize::new(7);
        recovery
            .report(
                Issue::MissingNumber,
                Error::value("Missing number"),
                Some(Arc::new(source)),
                "Used zero",
            )
            .unwrap();
        assert_eq!(
            recovery.pending[0].source.as_ref().unwrap().file.as_ref(),
            "/project/chapter.tex"
        );
        assert_eq!(
            recovery.pending[0].details.as_ref().unwrap().code,
            "tex.missing_number"
        );
        assert!(
            recovery
                .report(
                    Issue::MissingNumber,
                    Error::value("Missing number"),
                    None,
                    "Used zero"
                )
                .is_err()
        );
        assert_eq!(recovery.pending.len(), 1);
    }
}
