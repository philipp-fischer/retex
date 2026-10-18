//! Visible-edge descent and owned margin kerns (`protrusion.py`).
use super::layout::TypesettingContext;
use super::model::{Node, NodePayload};
use super::nodes::NodeBuilder;
use crate::core::constants::Constants as C;
use crate::core::enums::NodeKind;
use crate::core::errors::{Error, Result};
use crate::core::values::GlueSpec;
use crate::fonts::catalog::FontCatalog;
use crate::fonts::microtypography::{FontCode, em_amount};
use std::collections::BTreeSet;
pub fn skippable(node: &Node) -> bool {
    match node.node_type {
        NodeKind::Insert | NodeKind::Mark | NodeKind::Adjust | NodeKind::Penalty => true,
        NodeKind::Whatsit => !node.sized_whatsit(),
        NodeKind::Disc => [node.list_ptr, node.pre_break, node.post_break]
            .iter()
            .all(|&pointer| pointer == C::NULL),
        NodeKind::Math => node.width == 0,
        NodeKind::Kern => node.width == 0 || matches!(node.kern_kind.as_str(), "normal" | "auto"),
        NodeKind::Glue => node
            .glue
            .is_some_and(|spec| spec == GlueSpec::default() && !spec.protrusion_barrier),
        NodeKind::Hlist => {
            node.width == 0 && node.height == 0 && node.depth == 0 && node.list_ptr == C::NULL
        }
        _ => false,
    }
}
pub fn edge(builder: &NodeBuilder, pointers: &[i64], right: bool) -> Result<Option<Node>> {
    let order = |mut sequence: Vec<i64>| {
        if !right {
            sequence.reverse();
        }
        sequence
    };
    let mut stack = vec![order(pointers.to_vec())];
    let mut ancestors = BTreeSet::new();
    let mut owners = Vec::new();
    while let Some(sequence) = stack.last_mut() {
        let Some(pointer) = sequence.pop() else {
            stack.pop();
            if let Some(owner) = owners.pop() {
                ancestors.remove(&owner);
            }
            continue;
        };
        let node = builder.node(pointer)?;
        if node.node_type == NodeKind::Hlist && node.list_ptr != C::NULL {
            if !ancestors.insert(pointer) {
                return Err(Error::value("Cycle in edge hierarchy"));
            }
            owners.push(pointer);
            stack.push(order(builder.siblings(node.list_ptr)?));
        } else if !skippable(node) {
            return Ok(node.glyph().then(|| node.clone()));
        }
    }
    Ok(None)
}
pub fn amount(fonts: &FontCatalog, character: Option<&Node>, right: bool) -> Result<i64> {
    match character.filter(|node| node.glyph()) {
        None => Ok(0),
        Some(node) => em_amount(
            &fonts.items[node.font],
            if right {
                FontCode::Right
            } else {
                FontCode::Left
            },
            node.char,
        ),
    }
}
pub fn add_margins(context: &mut TypesettingContext<'_>, mut head: i64) -> Result<i64> {
    let pointers = context.builder.siblings(head)?;
    if pointers.is_empty() {
        return Ok(head);
    }
    for right in [true, false] {
        let terminal_fill =
            right && context.builder.node(*pointers.last().unwrap())?.glue_kind == "parfillskip";
        let visible = if terminal_fill {
            &pointers[..pointers.len() - 1]
        } else {
            &pointers
        };
        let character = edge(context.builder, visible, right)?;
        let width = amount(context.fonts, character.as_ref(), right)?;
        if width == 0 {
            continue;
        }
        let character = character.unwrap();
        let pointer = context.builder.new_kern(-width)?;
        let node = context.builder.node_mut(pointer)?;
        node.node_type = NodeKind::MarginKern;
        node.kern_kind = if right { "right" } else { "left" }.into();
        node.payload = NodePayload::CharacterPolicy {
            font: character.font,
            code: character.char,
        };
        if right {
            let predecessor = pointers[pointers.len() - if terminal_fill { 2 } else { 1 }];
            let link = context.builder.node(predecessor)?.link;
            context.builder.node_mut(pointer)?.link = link;
            context.builder.node_mut(predecessor)?.link = pointer;
        } else {
            context.builder.node_mut(pointer)?.link = head;
            head = pointer;
        }
    }
    Ok(head)
}
pub fn margin_query(builder: &NodeBuilder, head: i64, right: bool) -> Result<i64> {
    let mut pointers = builder.siblings(head)?;
    if right {
        pointers.reverse();
    }
    for pointer in pointers {
        let node = builder.node(pointer)?;
        let side = if right { "right" } else { "left" };
        if skippable(node)
            || node.node_type == NodeKind::Glue && node.glue_kind == format!("{side}skip")
        {
            continue;
        }
        return Ok(
            if node.node_type == NodeKind::MarginKern && node.kern_kind == side {
                node.width
            } else {
                0
            },
        );
    }
    Ok(0)
}
