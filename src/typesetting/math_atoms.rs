//! Parsed fields, noads and transient math ownership (`math_atoms.py`).
use super::math_codes::{DelimiterCode, MathCharacterCode};
use super::math_spacing::AtomKind;
use crate::core::enums::MathStyle;
use crate::core::source::SourceSpan;
use crate::core::values::GlueSpec;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
pub fn half(value: i64) -> i64 {
    value.div_euclid(2) + value.rem_euclid(2)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MathField {
    Character {
        family: i64,
        code: u32,
        origins: Vec<Arc<SourceSpan>>,
    },
    Box(i64),
    List(Vec<Atom>),
}
impl MathField {
    pub fn with_origin(mut self, source: Option<Arc<SourceSpan>>) -> Self {
        if let Self::Character { origins, .. } = &mut self {
            origins.extend(source);
        }
        self
    }
    pub fn origins(&self) -> &[Arc<SourceSpan>] {
        match self {
            Self::Character { origins, .. } => origins,
            _ => &[],
        }
    }
}
pub type Field = Option<Arc<MathField>>;
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub enum MathCommand {
    #[default]
    Nucleus,
    Style(MathStyle),
    Raw(i64),
    MuGlue(GlueSpec),
    MuKern(i64),
    Nonscript,
    Choice([Field; 4]),
    Fraction {
        thickness: Option<i64>,
        left: DelimiterCode,
        right: DelimiterCode,
        denominator: Field,
    },
    Delimited {
        left: DelimiterCode,
        right: DelimiterCode,
    },
    Middle(DelimiterCode),
    Radical(DelimiterCode),
    Accent(MathCharacterCode),
    Overline,
    Underline,
    Vcenter,
    Alignment(i64),
}
impl MathCommand {
    pub fn is_transparent(&self) -> bool {
        matches!(
            self,
            Self::Style(_) | Self::Raw(_) | Self::MuGlue(_) | Self::MuKern(_) | Self::Nonscript
        )
    }
    pub fn needs_new_script_atom(&self) -> bool {
        self.is_transparent() || matches!(self, Self::Choice(_))
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Atom {
    pub source: Option<Arc<SourceSpan>>,
    pub kind: AtomKind,
    pub nucleus: Field,
    pub sup: Field,
    pub sub: Field,
    pub command: MathCommand,
    pub limits: i64,
    pub math_text: bool,
}
#[derive(Clone)]
pub struct MathListFrame {
    pub context_depth: usize,
    pub atoms: Rc<RefCell<Vec<Atom>>>,
}
#[derive(Clone, Default)]
pub struct MathState {
    pub held: Vec<i64>,
    pub depth: usize,
    last_line: Option<i64>,
    last_line_parameter: Option<(crate::checked_material::Observer, usize)>,
    pub list_frames: Vec<MathListFrame>,
    pub equation_number: Option<(bool, i64)>,
}

impl MathState {
    /// The existing checkpoint prefix guard owns concrete revision state; it
    /// starts a new evidence scope on restoration. Ordinary copies propagate.
    pub(crate) fn concrete_snapshot(&self) -> Self {
        let mut state = self.clone();
        state.last_line_parameter = None;
        state
    }
    pub(crate) fn copy_last_line_from(&mut self, other: &Self) {
        self.last_line = other.last_line;
        self.last_line_parameter = other.last_line_parameter.clone();
    }

    pub fn last_line(&self) -> Option<i64> {
        if let Some((observer, candidate)) = &self.last_line_parameter {
            observer.borrow_mut().last_line_observed.insert(*candidate);
        }
        self.last_line
    }
    pub(crate) fn last_line_value(&self) -> Option<i64> {
        self.last_line
    }
    pub(crate) fn set_last_line(&mut self, value: Option<i64>) {
        self.last_line = value;
        self.last_line_parameter = None;
    }
    pub(crate) fn parameterize_last_line(
        &mut self,
        observer: crate::checked_material::Observer,
        candidate: usize,
    ) {
        self.last_line_parameter = Some((observer, candidate));
    }
}
