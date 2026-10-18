//! Typed scanner frames and control-sequence meanings (`language/records.py`).
use super::character_tokens::Token;
use super::tokens::MacroDefinition;
use crate::core::constants::Constants as C;
use crate::core::enums::{
    BoxOperation, CharacterCategory, Command, DefinitionOperation, InputStateKind, MathStyle,
    NodeKind, TokenProtection, UnpackOperation,
};
use crate::core::source::SourceSpan;
use crate::core::values::GlueSpec;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QuantityKind {
    Count,
    Dimen,
    Skip,
    Muskip,
    Toks,
}
impl QuantityKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Dimen => "dimen",
            Self::Skip => "skip",
            Self::Muskip => "muskip",
            Self::Toks => "toks",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ValueIndex {
    Number(i64),
    Name(String),
    Pair(i64, i64),
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ValueKey {
    pub table: String,
    pub index: ValueIndex,
}
impl ValueKey {
    pub fn parameter(name: impl Into<String>) -> Self {
        Self {
            table: "parameter".into(),
            index: ValueIndex::Name(name.into()),
        }
    }
    pub fn register(kind: QuantityKind, number: i64) -> Self {
        Self {
            table: kind.name().into(),
            index: ValueIndex::Number(number),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuantityReference {
    pub kind: QuantityKind,
    pub key: ValueKey,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quantity {
    Integer(i64),
    Glue(GlueSpec),
    Tokens(Arc<[Token]>),
    Shape(Vec<(i64, i64)>),
    Penalties(Vec<i64>),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeaningCommand {
    Operation(Command),
    Character(CharacterCategory),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CommandOperand {
    Number(i64),
    Text(String),
    Quantity(QuantityReference),
    Macro(Arc<MacroDefinition>),
    Box(BoxOperation),
    Unpack(UnpackOperation),
    Definition(DefinitionOperation),
    MathStyle(MathStyle),
    NodeKind(NodeKind),
    FontCode(crate::fonts::microtypography::FontCode),
    Protection(TokenProtection),
    Alignment(crate::core::enums::AlignmentBoundary),
}
impl PartialEq for CommandOperand {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => a == b,
            (Self::Text(a), Self::Text(b)) => a == b,
            (Self::Quantity(a), Self::Quantity(b)) => a == b,
            (Self::Macro(a), Self::Macro(b)) => Arc::ptr_eq(a, b),
            (Self::Box(a), Self::Box(b)) => a == b,
            (Self::Unpack(a), Self::Unpack(b)) => a == b,
            (Self::Definition(a), Self::Definition(b)) => a == b,
            (Self::MathStyle(a), Self::MathStyle(b)) => a == b,
            (Self::NodeKind(a), Self::NodeKind(b)) => a == b,
            (Self::FontCode(a), Self::FontCode(b)) => a == b,
            (Self::Protection(a), Self::Protection(b)) => a == b,
            (Self::Alignment(a), Self::Alignment(b)) => a == b,
            _ => false,
        }
    }
}
impl Eq for CommandOperand {}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Equivalence {
    pub command: MeaningCommand,
    pub level: usize,
    pub value: CommandOperand,
}
impl Default for Equivalence {
    fn default() -> Self {
        Self {
            command: MeaningCommand::Operation(Command::UndefinedCs),
            level: C::LEVEL_ZERO,
            value: CommandOperand::Number(C::NULL),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputState {
    pub state: InputStateKind,
    pub index: usize,
    pub start: usize,
    pub loc: usize,
    pub limit: isize,
    pub name: Option<String>,
    pub token_list: Option<Arc<[Token]>>,
    pub token_index: usize,
    // Token frames retain physical lines without copying them. The physical
    // scanner detaches only when reducing ^^ notation; immutable suspended
    // cursors keep their own positions and column mappings.
    pub buffer: Option<Arc<Vec<u32>>>,
    pub columns: Option<Arc<Vec<usize>>>,
    pub byte_columns: Option<Arc<Vec<usize>>>,
    pub line: usize,
    pub last: usize,
    pub buffer_base: usize,
    pub source: Option<Arc<SourceSpan>>,
    pub token_sources: Option<Arc<[Option<Arc<SourceSpan>>]>>,
}
impl Default for InputState {
    fn default() -> Self {
        Self {
            state: InputStateKind::NewLine,
            index: 0,
            start: 0,
            loc: 0,
            limit: 0,
            name: None,
            token_list: None,
            token_index: 0,
            buffer: None,
            columns: None,
            byte_columns: None,
            line: 0,
            last: 0,
            buffer_base: 0,
            source: None,
            token_sources: None,
        }
    }
}
