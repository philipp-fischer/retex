//! Checked native consumers of a parameterized, pure paragraph.
//!
//! Whole-node access is denied by default while evidence is being constructed.
//! The few grants below belong to non-interpreting native algorithms. They do
//! not grant permissions to TeX, callbacks, or arbitrary callers of NodeTable.
use crate::{
    core::{constants::Constants as C, enums::NodeKind, values::GlueSpec},
    typesetting::model::{Node, NodePayload},
};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Consumer {
    Raw,
    Shape,
    Link,
    Copy,
    Migrations,
    Ownership,
    Paint,
    Preparation,
    Validation,
    EffectIdentity,
    TailKind,
    TailLinks,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Parameter {
    pub candidate: usize,
    pub line: usize,
    pub root: bool,
}
#[derive(Default)]
pub(crate) struct Evidence {
    pub tags: BTreeMap<i64, Parameter>,
    pub last_line_observed: std::collections::BTreeSet<usize>,
    pub badness_observed: std::collections::BTreeSet<usize>,
    /// Immutable memo snapshots keep their own parameter identities after the
    /// live arena releases nodes. Observations still reject the parent recipe.
    pub snapshot_parent: Option<Observer>,
    pub tail_open: std::collections::BTreeSet<usize>,
    pub tail_reads: BTreeMap<usize, Vec<TailObservation>>,
    pub misses: BTreeMap<usize, &'static str>,
    pub consumers: BTreeMap<(usize, &'static str), usize>,
    pub frozen: bool,
    pub paint: Vec<PaintSite>,
    pub paint_epoch: u64,
    pub unknown_sites: BTreeMap<(usize, String), usize>,
    pub root_copies: BTreeMap<usize, usize>,
    grant: Option<Consumer>,
}
#[derive(Clone)]
pub(crate) struct PaintSite {
    pub parameter: Parameter,
    pub placement: usize,
    pub entry_epoch: u64,
    pub exit_epoch: u64,
    pub page: usize,
    pub entry: crate::pdf::extraction::ExtractionSpaces,
    pub exit: crate::pdf::extraction::ExtractionSpaces,
}
pub(crate) type Observer = Rc<RefCell<Evidence>>;
pub(crate) struct Grant {
    observer: Option<Observer>,
    previous: Option<Consumer>,
}
impl Drop for Grant {
    fn drop(&mut self) {
        if let Some(o) = &self.observer {
            o.borrow_mut().grant = self.previous;
        }
    }
}
impl Evidence {
    pub fn grant(observer: &Option<Observer>, consumer: Consumer) -> Grant {
        let previous = observer
            .as_ref()
            .and_then(|o| o.borrow_mut().grant.replace(consumer));
        Grant {
            observer: observer.clone(),
            previous,
        }
    }
    #[track_caller]
    pub fn access(&mut self, id: i64, access: Consumer) {
        if self.frozen {
            return;
        }
        let Some(tag) = self.tags.get(&id).copied() else {
            return;
        };
        let access = if access == Consumer::Raw
            || (access == Consumer::Link && !tag.root && self.grant.is_some())
            || (access == Consumer::Shape && !tag.root && self.grant == Some(Consumer::Preparation))
        {
            self.grant.unwrap_or(Consumer::Raw)
        } else {
            access
        };
        let name = match access {
            Consumer::Shape if tag.root => "root_geometry",
            Consumer::Link if tag.root => "outer_links",
            Consumer::Copy => "independent_copy",
            Consumer::Migrations => "pure_migration_scan",
            Consumer::Ownership => "ownership_only",
            Consumer::Paint => "paint_parameter",
            // This grant may inspect only the material still being computed.
            // It cannot hide a read of an earlier, already prepared parameter.
            Consumer::Preparation if self.tail_open.contains(&tag.candidate) => {
                "real_paragraph_preparation"
            }
            Consumer::Validation => "material_validation",
            Consumer::EffectIdentity => "effect_identity_projection",
            Consumer::TailKind if tag.root || self.tail_open.contains(&tag.candidate) => {
                "checked_tail_kind"
            }
            Consumer::TailLinks if tag.root || self.tail_open.contains(&tag.candidate) => {
                "tail_list_transform"
            }
            _ => {
                if self.unknown_sites.len() < 64 {
                    let site = std::panic::Location::caller();
                    *self
                        .unknown_sites
                        .entry((
                            tag.candidate,
                            format!("{}:{}:{access:?}", site.file(), site.line()),
                        ))
                        .or_default() += 1;
                }
                self.reject(tag.candidate, "unclassified_material_observation");
                return;
            }
        };
        *self.consumers.entry((tag.candidate, name)).or_default() += 1;
    }
    /// Rendering may change glyph-use and font-resource allocation state. It
    /// is not invariant TeX control input. Observing it after parameter paint
    /// invalidates the page recipe; the live interpreter still runs normally.
    pub fn paint_resource_observation(&mut self) {
        if self.frozen {
            return;
        }
        for site in &self.paint {
            self.misses
                .entry(site.parameter.candidate)
                .or_insert("paint_resource_used_by_interpreter");
        }
    }
    fn reject(&mut self, candidate: usize, reason: &'static str) {
        self.misses.entry(candidate).or_insert(reason);
        if let Some(parent) = &self.snapshot_parent {
            parent.borrow_mut().reject(candidate, reason);
        }
    }
    #[track_caller]
    pub fn unknown_iteration(&mut self) {
        if self.frozen || self.grant == Some(Consumer::Ownership) {
            return;
        }
        let candidates: std::collections::BTreeSet<_> =
            self.tags.values().map(|t| t.candidate).collect();
        for candidate in candidates {
            if self.unknown_sites.len() < 64 {
                let site = std::panic::Location::caller();
                *self
                    .unknown_sites
                    .entry((candidate, format!("{}:{}:bulk", site.file(), site.line())))
                    .or_default() += 1;
            }
            self.reject(candidate, "unclassified_node_table_iteration");
        }
    }
    pub fn copied(&mut self, from: i64, to: i64) {
        if self.frozen {
            return;
        }
        if let Some(tag) = self.tags.get(&from).copied() {
            if tag.root {
                *self.root_copies.entry(tag.candidate).or_default() += 1;
            }
            if self.tags.len() >= 65536 {
                self.misses
                    .entry(tag.candidate)
                    .or_insert("material_identity_bound");
            } else {
                self.tags.insert(to, tag);
            }
        }
    }
}

/// Exact projection consumed by last-item queries, with no list or glyph data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TailSummary {
    pub node_type: NodeKind,
    pub width: i64,
    pub penalty: i64,
    pub glue: Option<GlueSpec>,
    pub glue_kind: String,
}
impl TailSummary {
    pub fn new(n: &Node) -> Self {
        Self {
            node_type: n.node_type,
            width: if n.node_type == NodeKind::Kern {
                n.width
            } else {
                0
            },
            penalty: if n.node_type == NodeKind::Penalty {
                n.penalty
            } else {
                0
            },
            glue: if n.node_type == NodeKind::Glue {
                n.glue
            } else {
                None
            },
            glue_kind: if n.node_type == NodeKind::Glue {
                n.glue_kind.clone()
            } else {
                String::new()
            },
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TailObservation {
    Kind(NodeKind),
    Summary(TailSummary),
}

/// Exactly the observations used by vertical construction. In particular this
/// type exposes no glyph, font, origin, list contents, glue-set ratio or badness.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Shape {
    pub node_type: NodeKind,
    pub width: i64,
    pub height: i64,
    pub depth: i64,
    pub shift: i64,
    pub glue: Option<GlueSpec>,
    pub penalty: i64,
    pub leader_ptr: i64,
    pub whatsit: String,
    pub running_width: bool,
    pub running_height: bool,
    pub running_depth: bool,
    sized: bool,
}
impl Shape {
    pub fn new(n: &Node) -> Self {
        Self {
            node_type: n.node_type,
            width: n.width,
            height: n.height,
            depth: n.depth,
            shift: n.shift,
            glue: n.glue,
            penalty: n.penalty,
            leader_ptr: n.leader_ptr,
            whatsit: n.whatsit.clone(),
            running_width: n.running_width,
            running_height: n.running_height,
            running_depth: n.running_depth,
            sized: n.sized_whatsit(),
        }
    }
    pub fn sized_whatsit(&self) -> bool {
        self.sized
    }
}
pub(crate) fn pure(n: &Node) -> bool {
    matches!(
        n.node_type,
        NodeKind::Char
            | NodeKind::Ligature
            | NodeKind::Hlist
            | NodeKind::Glue
            | NodeKind::Kern
            | NodeKind::Penalty
    ) && matches!(n.payload, NodePayload::None)
        && n.whatsit.is_empty()
        && n.shaped.is_none()
        && n.leader_ptr == C::NULL
        && n.pre_break == C::NULL
        && n.post_break == C::NULL
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unchecked_contents_cannot_acquire_a_geometry_contract() {
        let mut e = Evidence::default();
        e.tags.insert(
            1,
            Parameter {
                candidate: 0,
                line: 0,
                root: true,
            },
        );
        e.tags.insert(
            2,
            Parameter {
                candidate: 0,
                line: 0,
                root: false,
            },
        );
        e.access(1, Consumer::Shape);
        assert!(e.misses.is_empty());
        e.access(2, Consumer::Shape);
        assert!(!e.misses.is_empty());
        e.misses.clear();
        e.copied(2, 3);
        e.access(3, Consumer::Raw);
        assert!(!e.misses.is_empty());
    }
    #[test]
    fn copying_propagates_the_parameter_and_owns_an_independent_graph() {
        use crate::typesetting::{model::NodeFields, nodes::NodeBuilder};
        let mut builder = NodeBuilder::new(Default::default());
        let glyph = builder
            .allocate(Node::from(NodeFields {
                node_type: NodeKind::Char,
                char: 65,
                width: 10,
                ..Default::default()
            }))
            .unwrap();
        let root = builder
            .allocate(Node::from(NodeFields {
                node_type: NodeKind::Hlist,
                list_ptr: glyph,
                width: 100,
                height: 10,
                ..Default::default()
            }))
            .unwrap();
        let observer = Observer::default();
        observer.borrow_mut().tags.insert(
            root,
            Parameter {
                candidate: 0,
                line: 0,
                root: true,
            },
        );
        observer.borrow_mut().tags.insert(
            glyph,
            Parameter {
                candidate: 0,
                line: 0,
                root: false,
            },
        );
        builder.nodes.observer = Some(observer.clone());
        let copy = builder.copy_box(root).unwrap();
        assert!(observer.borrow().misses.is_empty());
        assert!(observer.borrow().tags[&copy].root);
        // An ordinary caller can still inspect or mutate material, but that
        // observation disqualifies the local recipe rather than the TeX job.
        let child = builder.node(copy).unwrap().list_ptr;
        assert_ne!(child, glyph);
        assert!(!observer.borrow().tags[&child].root);
        builder.node_mut(child).unwrap().char = 66;
        assert_eq!(builder.node(glyph).unwrap().char, 65);
        assert!(observer.borrow().misses.contains_key(&0));
    }
    #[test]
    fn preparation_permission_ends_at_ready_and_does_not_hide_later_reads() {
        let observer = Observer::default();
        observer.borrow_mut().tags.insert(
            1,
            Parameter {
                candidate: 0,
                line: 0,
                root: false,
            },
        );
        observer.borrow_mut().tags.insert(
            2,
            Parameter {
                candidate: 0,
                line: 0,
                root: true,
            },
        );
        observer.borrow_mut().tail_open.insert(0);
        let _grant = Evidence::grant(&Some(observer.clone()), Consumer::Preparation);
        observer.borrow_mut().access(1, Consumer::Raw);
        assert!(observer.borrow().misses.is_empty());
        observer.borrow_mut().tail_open.remove(&0);
        observer.borrow_mut().access(2, Consumer::Shape);
        observer.borrow_mut().access(2, Consumer::Link);
        assert!(observer.borrow().misses.is_empty());
        observer.borrow_mut().access(1, Consumer::Raw);
        assert!(observer.borrow().misses.contains_key(&0));
    }
    #[test]
    fn packing_badness_copies_observe_and_native_overwrites_end_dependency() {
        let observer = Observer::default();
        let mut layout = crate::typesetting::layout::Layout::default();
        layout.set_last_badness(10);
        layout.parameterize_badness(observer.clone(), 0);
        let mut copy = layout.clone();
        assert_eq!(copy.last_badness(), 10);
        assert!(observer.borrow().badness_observed.contains(&0));
        observer.borrow_mut().badness_observed.clear();
        copy.set_last_badness(20);
        assert_eq!(copy.last_badness(), 20);
        assert!(observer.borrow().badness_observed.is_empty());
        assert_eq!(layout.last_badness(), 10);
        assert!(observer.borrow().badness_observed.contains(&0));
    }
    #[test]
    fn scalar_copies_preserve_the_parameter_and_writes_end_its_dependency() {
        let observer = Observer::default();
        let mut state = crate::typesetting::math_atoms::MathState::default();
        state.set_last_line(Some(10));
        state.parameterize_last_line(observer.clone(), 0);
        let mut copy = state.clone();
        assert_eq!(copy.last_line(), Some(10));
        assert!(observer.borrow().last_line_observed.contains(&0));
        observer.borrow_mut().last_line_observed.clear();
        copy.set_last_line(Some(20));
        assert_eq!(copy.last_line(), Some(20));
        assert!(observer.borrow().last_line_observed.is_empty());
        assert_eq!(state.last_line(), Some(10));
        assert!(observer.borrow().last_line_observed.contains(&0));
    }
    #[test]
    fn memo_snapshot_keeps_parameter_identity_after_live_release() {
        use crate::typesetting::{model::NodeFields, nodes::NodeBuilder};
        let mut builder = NodeBuilder::new(Default::default());
        let id = builder
            .allocate(Node::from(NodeFields {
                node_type: NodeKind::Char,
                char: 65,
                ..Default::default()
            }))
            .unwrap();
        let observer = Observer::default();
        observer.borrow_mut().tags.insert(
            id,
            Parameter {
                candidate: 0,
                line: 0,
                root: false,
            },
        );
        builder.nodes.observer = Some(observer.clone());
        let snapshot = builder.nodes.memo_snapshot();
        assert!(observer.borrow().misses.is_empty());
        builder.nodes.remove(&id);
        observer.borrow_mut().misses.clear();
        assert!(!observer.borrow().tags.contains_key(&id));
        assert_eq!(snapshot.get(&id).unwrap().as_ref().unwrap().char, 65);
        assert!(observer.borrow().misses.contains_key(&0));
    }
    #[test]
    fn vertical_break_reads_checked_roots_but_unclassified_bulk_reads_reject() {
        use crate::typesetting::{model::NodeFields, nodes::NodeBuilder, vertical::Vertical};
        let mut builder = NodeBuilder::new(Default::default());
        let root = builder
            .allocate(Node::from(NodeFields {
                node_type: NodeKind::Hlist,
                width: 100,
                height: 10,
                depth: 2,
                ..Default::default()
            }))
            .unwrap();
        let penalty = builder
            .allocate(Node::from(NodeFields {
                node_type: NodeKind::Penalty,
                penalty: -10000,
                ..Default::default()
            }))
            .unwrap();
        let observer = Observer::default();
        observer.borrow_mut().tags.insert(
            root,
            Parameter {
                candidate: 0,
                line: 0,
                root: true,
            },
        );
        builder.nodes.observer = Some(observer.clone());
        assert_eq!(
            Vertical::break_list(&builder, &[root, penalty], 20, 3).unwrap(),
            (1, 12)
        );
        assert!(observer.borrow().misses.is_empty());
        assert!(
            observer
                .borrow()
                .consumers
                .contains_key(&(0, "root_geometry"))
        );
        let _count = builder.nodes.len();
        assert_eq!(
            observer.borrow().misses[&0],
            "unclassified_node_table_iteration"
        );
    }
}
