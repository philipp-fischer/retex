//! Vertical break costs, marks, migration, discards and splitting (`vertical.py`).
use super::layout::{Layout, TypesettingContext};
use super::model::{BoxScanContext, Node, NodePayload};
use super::nodes::{NodeBuilder, glue_parameter, parameter};
use crate::core::constants::Constants as C;
use crate::core::enums::{ListMode, NodeKind};
use crate::core::errors::{Error, Result};
use crate::core::values::{GlueSpec, MAX_DIMEN, badness};
use crate::language::character_tokens::Token;
use crate::typesetting::model::NodeFields;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
pub const AWFUL: i64 = MAX_DIMEN;
pub fn is_box(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::Hlist | NodeKind::Vlist | NodeKind::Rule)
}
pub fn discardable(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::Glue | NodeKind::Kern | NodeKind::Penalty)
}
pub fn glue_break_predecessor(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Hlist
            | NodeKind::Vlist
            | NodeKind::Rule
            | NodeKind::Insert
            | NodeKind::Mark
            | NodeKind::Adjust
            | NodeKind::Ligature
            | NodeKind::Disc
            | NodeKind::Whatsit
            | NodeKind::Math
    )
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DiscardKind {
    Page,
    Split,
}
#[derive(Clone, Default)]
pub struct Vertical {
    pub marks: BTreeMap<(i64, u8), Arc<[Token]>>,
    pub pending_migrations: Vec<i64>,
    pub discards: BTreeMap<DiscardKind, Vec<i64>>,
}
impl Vertical {
    pub fn mark(
        &mut self,
        builder: &mut NodeBuilder,
        number: i64,
        tokens: Arc<[Token]>,
    ) -> Result<()> {
        let pointer = builder.allocate(Node::from(NodeFields {
            node_type: NodeKind::Mark,
            payload: NodePayload::Mark { number, tokens },
            ..NodeFields::default()
        }))?;
        builder.tail_append(pointer)
    }
    pub fn insert(
        &mut self,
        builder: &mut NodeBuilder,
        number: i64,
        box_pointer: i64,
        capture: &BoxScanContext,
    ) -> Result<i64> {
        let material = builder.node(box_pointer)?.clone();
        let pointer = builder.allocate(Node::from(NodeFields {
            node_type: NodeKind::Insert,
            list_ptr: material.list_ptr,
            height: material.height + material.depth,
            depth: capture.split_max_depth,
            payload: NodePayload::Insertion {
                number,
                split_top_skip: capture.split_top_skip,
                floating_penalty: capture.floating_penalty,
            },
            ..NodeFields::default()
        }))?;
        builder.node_mut(box_pointer)?.list_ptr = C::NULL;
        builder.release(box_pointer, false)?;
        builder.tail_append(pointer)?;
        Ok(pointer)
    }
    pub fn adjust(
        &mut self,
        builder: &mut NodeBuilder,
        box_pointer: i64,
        pre: bool,
    ) -> Result<i64> {
        if builder.context.mode == ListMode::Vertical {
            return Err(Error::value("Vertical adjustment in vertical mode")
                .with_issue(crate::core::recovery::Issue::Mode));
        }
        let list_ptr = builder.node(box_pointer)?.list_ptr;
        let pointer = builder.allocate(Node::from(NodeFields {
            node_type: NodeKind::Adjust,
            list_ptr,
            payload: NodePayload::Adjustment { pre },
            ..NodeFields::default()
        }))?;
        builder.node_mut(box_pointer)?.list_ptr = C::NULL;
        builder.release(box_pointer, false)?;
        builder.tail_append(pointer)?;
        Ok(pointer)
    }
    pub fn extract_adjustments(
        &mut self,
        builder: &mut NodeBuilder,
        box_pointer: i64,
    ) -> Result<(Vec<i64>, Vec<i64>)> {
        let _grant = builder
            .nodes
            .contract(crate::checked_material::Consumer::Migrations);
        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut retained = Vec::new();
        for pointer in builder.siblings(builder.node(box_pointer)?.list_ptr)? {
            let node = builder.node(pointer)?.clone();
            builder.node_mut(pointer)?.link = C::NULL;
            if node.node_type == NodeKind::Adjust {
                let NodePayload::Adjustment { pre } = node.payload else {
                    return Err(Error::value("Invalid adjustment payload"));
                };
                let target = if pre { &mut before } else { &mut after };
                target.extend(builder.siblings(node.list_ptr)?);
                builder.node_mut(pointer)?.list_ptr = C::NULL;
                builder.release(pointer, false)?;
            } else if matches!(node.node_type, NodeKind::Insert | NodeKind::Mark) {
                after.push(pointer);
            } else {
                retained.push(pointer);
            }
        }
        let head = Self::link(builder, &retained)?;
        builder.node_mut(box_pointer)?.list_ptr = head;
        for &pointer in before.iter().chain(&after) {
            builder.node_mut(pointer)?.link = C::NULL;
            self.pending_migrations.push(pointer);
        }
        Ok((before, after))
    }
    pub fn roots(&self) -> Vec<i64> {
        let mut roots = Vec::new();
        self.append_roots(&mut roots);
        roots
    }
    pub(crate) fn append_roots(&self, roots: &mut Vec<i64>) {
        roots.extend(
            self.pending_migrations
                .iter()
                .chain(self.discards.values().flatten())
                .copied(),
        );
    }
    pub fn save_discard(
        &mut self,
        context: &mut TypesettingContext<'_>,
        kind: DiscardKind,
        pointer: i64,
    ) -> Result<()> {
        context.builder.node_mut(pointer)?.link = C::NULL;
        if parameter(context.env, "savingvdiscards", 0) > 0 {
            self.discards.entry(kind).or_default().push(pointer);
            Ok(())
        } else {
            context.builder.release(pointer, false)
        }
    }
    pub fn clear_discards(&mut self, builder: &mut NodeBuilder, kind: DiscardKind) -> Result<()> {
        for pointer in self.discards.remove(&kind).unwrap_or_default() {
            builder.node_mut(pointer)?.link = C::NULL;
            builder.release(pointer, false)?;
        }
        Ok(())
    }
    pub fn take_discards(&mut self, builder: &mut NodeBuilder, kind: DiscardKind) -> Result<i64> {
        Self::link(builder, &self.discards.remove(&kind).unwrap_or_default())
    }
    pub fn take_migration(&mut self, builder: &mut NodeBuilder, pointer: i64) -> Result<()> {
        let index = self
            .pending_migrations
            .iter()
            .position(|&item| item == pointer)
            .ok_or_else(|| Error::value("Unowned migration"))?;
        self.pending_migrations.remove(index);
        builder.node_mut(pointer)?.link = C::NULL;
        builder.tail_append(pointer)
    }
    pub fn mark_text(&self, number: i64, kind: u8) -> Arc<[Token]> {
        self.marks
            .get(&(number, kind))
            .cloned()
            .unwrap_or_else(|| Arc::from([]))
    }
    pub fn record_split(&mut self, builder: &NodeBuilder, pointers: &[i64]) -> Result<()> {
        self.marks.retain(|&(_, kind), _| kind < 3);
        for &pointer in pointers {
            if builder.nodes.shape(pointer)?.node_type == NodeKind::Mark {
                let NodePayload::Mark { number, tokens } = &builder.node(pointer)?.payload else {
                    return Err(Error::value("Invalid mark"));
                };
                self.marks
                    .entry((*number, 3))
                    .or_insert_with(|| tokens.clone());
                self.marks.insert((*number, 4), tokens.clone());
            }
        }
        Ok(())
    }
    pub fn cost(
        height: i64,
        goal: i64,
        stretch: [i64; 4],
        shrink: i64,
        penalty: i64,
        insert_penalties: i64,
    ) -> (i64, i64) {
        let b = if height < goal {
            if stretch[1..].iter().any(|&amount| amount != 0) {
                0
            } else {
                badness(goal - height, stretch[0])
            }
        } else if height - goal > shrink {
            AWFUL
        } else {
            badness(height - goal, shrink)
        };
        let cost = if insert_penalties >= 10000 {
            AWFUL
        } else if b < AWFUL {
            if penalty <= -10000 {
                penalty
            } else if b < 10000 {
                b + penalty + insert_penalties
            } else {
                100000
            }
        } else {
            b
        };
        (b, cost)
    }
    pub fn break_list(
        builder: &NodeBuilder,
        pointers: &[i64],
        goal: i64,
        maximum: i64,
    ) -> Result<(usize, i64)> {
        let mut height = 0;
        let mut depth = 0;
        let mut shrink = 0;
        let mut stretch = [0; 4];
        let mut least = AWFUL;
        let mut best = 0;
        let mut best_size = 0;
        let mut previous = pointers
            .first()
            .map(|&pointer| builder.nodes.shape(pointer).map(|node| node.node_type))
            .transpose()?
            .unwrap_or(NodeKind::Glue);
        for index in 0..=pointers.len() {
            let node = pointers
                .get(index)
                .map(|&pointer| builder.nodes.shape(pointer))
                .transpose()?;
            let kind = node
                .as_ref()
                .map_or(NodeKind::Penalty, |node| node.node_type);
            let penalty = if node.is_none() {
                Some(-10000)
            } else if kind == NodeKind::Glue && glue_break_predecessor(previous) {
                Some(0)
            } else if kind == NodeKind::Kern
                && pointers
                    .get(index + 1)
                    .map(|&pointer| {
                        builder
                            .nodes
                            .shape(pointer)
                            .map(|node| node.node_type == NodeKind::Glue)
                    })
                    .transpose()?
                    .unwrap_or(false)
            {
                Some(0)
            } else if kind == NodeKind::Penalty {
                Some(node.as_ref().unwrap().penalty)
            } else {
                None
            };
            if let Some(penalty) = penalty.filter(|&penalty| penalty < 10000) {
                let (_, cost) = Self::cost(height, goal, stretch, shrink, penalty, 0);
                if cost <= least {
                    best = index;
                    least = cost;
                    best_size = height + depth;
                }
                if cost == AWFUL || penalty <= -10000 {
                    return Ok((best, best_size));
                }
            }
            let Some(node) = node else {
                break;
            };
            if is_box(kind) || node.sized_whatsit() {
                height += depth + node.height;
                depth = node.depth;
            } else if matches!(kind, NodeKind::Glue | NodeKind::Kern) {
                height += depth + node.width;
                depth = 0;
                if kind == NodeKind::Glue {
                    let spec = node
                        .glue
                        .ok_or_else(|| Error::value("Glue without specification"))?;
                    if spec.shrink != 0 && spec.shrink_order != 0 {
                        return Err(Error::value(
                            "Infinite glue shrinkage found in box being split",
                        )
                        .with_issue(crate::core::recovery::Issue::GlueShrinkage));
                    }
                    stretch[spec.stretch_order as usize] += spec.stretch;
                    shrink += spec.shrink;
                }
            } else if !matches!(
                kind,
                NodeKind::Penalty | NodeKind::Mark | NodeKind::Whatsit | NodeKind::Insert
            ) {
                return Err(Error::value("Nonvertical material in vertical break"));
            }
            if depth > maximum {
                height += depth - maximum;
                depth = maximum;
            }
            previous = kind;
        }
        Ok((best, best_size))
    }
    pub fn link(builder: &mut NodeBuilder, pointers: &[i64]) -> Result<i64> {
        for (index, &pointer) in pointers.iter().enumerate() {
            builder
                .nodes
                .set_link(pointer, pointers.get(index + 1).copied().unwrap_or(C::NULL))?;
        }
        Ok(pointers.first().copied().unwrap_or(C::NULL))
    }
    pub fn prune(
        &mut self,
        context: &mut TypesettingContext<'_>,
        pointers: &[i64],
        spec: GlueSpec,
        save: bool,
    ) -> Result<Vec<i64>> {
        let mut result = Vec::new();
        for (index, &pointer) in pointers.iter().enumerate() {
            let node = context.builder.nodes.shape(pointer)?;
            if is_box(node.node_type) {
                let spec = GlueSpec {
                    width: (spec.width - node.height).max(0),
                    ..spec
                };
                result.push(
                    context
                        .builder
                        .new_glue(spec, "normal", context.fonts.current)?,
                );
                result.extend_from_slice(&pointers[index..]);
                break;
            }
            if discardable(node.node_type) {
                context.builder.node_mut(pointer)?.link = C::NULL;
                if save {
                    self.save_discard(context, DiscardKind::Split, pointer)?;
                } else {
                    context.builder.release(pointer, false)?;
                }
            } else {
                result.push(pointer);
            }
        }
        Ok(result)
    }
    pub fn split(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        number: i64,
        goal: i64,
    ) -> Result<i64> {
        #[cfg(feature = "latency-diagnostics")]
        crate::local_update_audit::consumer("vsplit");
        self.clear_discards(context.builder, DiscardKind::Split)?;
        self.record_split(context.builder, &[])?;
        let owner = context
            .env
            .box_registers
            .get(&number)
            .copied()
            .unwrap_or(C::NULL);
        if owner == C::NULL {
            return Ok(C::NULL);
        }
        let original = context.builder.node(owner)?;
        if original.node_type != NodeKind::Vlist {
            return Err(Error::value("vsplit needs a vbox")
                .with_issue(crate::core::recovery::Issue::BoxOperand));
        }
        let pointers = context.builder.siblings(original.list_ptr)?;
        let max_depth = parameter(context.env, "splitmaxdepth", MAX_DIMEN);
        let (index, _) = Self::break_list(context.builder, &pointers, goal, max_depth)?;
        let first = &pointers[..index];
        self.record_split(context.builder, first)?;
        let spec = glue_parameter(context.env, "splittopskip");
        let save = parameter(context.env, "savingvdiscards", 0) > 0;
        let rest = self.prune(context, &pointers[index..], spec, save)?;
        context.builder.node_mut(owner)?.list_ptr = C::NULL;
        context.builder.release(owner, false)?;
        if rest.is_empty() {
            context.env.box_registers.remove(&number);
        } else {
            let head = Self::link(context.builder, &rest)?;
            let remainder = layout.vpack(context, head, false, None, false, MAX_DIMEN)?;
            context.env.box_registers.insert(number, remainder);
        }
        let head = Self::link(context.builder, first)?;
        layout.vpack(context, head, false, Some(goal), false, max_depth)
    }
}
