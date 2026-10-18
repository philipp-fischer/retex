//! Ordered scope restorations (`language/groups.py`).
use super::character_tokens::Token;
use super::records::{Equivalence, Quantity, ValueKey};
use crate::core::enums::GroupKind;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RestorationKey {
    Meaning(String),
    Value(ValueKey),
    Box(i64),
    Font,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SavedValue {
    Meaning(Option<Equivalence>),
    Value(Option<Quantity>),
    Box(i64),
    Font(usize),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Restoration {
    pub key: RestorationKey,
    pub value: SavedValue,
    pub level: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupFrame {
    pub kind: String,
    pub previous_group: GroupKind,
    pub entries: Vec<Restoration>,
    pub after_tokens: Vec<Token>,
}
impl GroupFrame {
    pub fn new(kind: impl Into<String>, previous_group: GroupKind) -> Self {
        Self {
            kind: kind.into(),
            previous_group,
            entries: Vec::new(),
            after_tokens: Vec::new(),
        }
    }
}
