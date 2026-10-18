//! Explicit TeX query code mappings (`language/numeric_codes.py`).
use crate::core::enums::{GroupKind, NodeKind};
pub fn group_type_code(kind: GroupKind) -> i64 {
    match kind {
        GroupKind::Bottom => 0,
        GroupKind::Brace => 1,
        GroupKind::Hbox => 2,
        GroupKind::AdjustedHbox => 3,
        GroupKind::Vbox => 4,
        GroupKind::Vtop => 5,
        GroupKind::Alignment => 6,
        GroupKind::NoAlign => 7,
        GroupKind::Output => 8,
        GroupKind::MathField => 9,
        GroupKind::Disc => 10,
        GroupKind::Insert => 11,
        GroupKind::Vcenter => 12,
        GroupKind::MathChoice => 13,
        GroupKind::Explicit => 14,
        GroupKind::MathShift => 15,
        GroupKind::MathLeft => 16,
    }
}
pub fn last_node_type_code(kind: NodeKind) -> i64 {
    match kind {
        NodeKind::Char => 0,
        NodeKind::Hlist => 1,
        NodeKind::Vlist => 2,
        NodeKind::Rule => 3,
        NodeKind::Insert => 4,
        NodeKind::Mark => 5,
        NodeKind::Adjust => 6,
        NodeKind::Ligature => 7,
        NodeKind::Disc => 8,
        NodeKind::Whatsit => 9,
        NodeKind::Math => 10,
        NodeKind::Glue => 11,
        NodeKind::Kern => 12,
        NodeKind::MarginKern => 15,
        NodeKind::Penalty => 13,
    }
}
