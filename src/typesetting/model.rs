//! Arena nodes, box captures and retained typed effects (`typesetting/model.py`).
use crate::core::constants::Constants as C;
use crate::core::enums::{NodeKind, WriteDestination};
use crate::core::source::SourceSpan;
use crate::core::values::GlueSpec;
use crate::fonts::unicode::ShapedGlyph;
use crate::language::character_tokens::Token;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct BoxScanContext {
    pub fresh_hbox: bool,
    pub prev_depth: Option<i64>,
    pub split_top_skip: GlueSpec,
    pub split_max_depth: i64,
    pub floating_penalty: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamDestination {
    File(u8),
    Named(WriteDestination),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum IoEffect {
    Open {
        stream: u8,
        #[cfg_attr(
            target_arch = "wasm32",
            serde(with = "crate::serialization::os_string")
        )]
        filename: OsString,
    },
    Close(StreamDestination),
    Write {
        destination: StreamDestination,
        tokens: Arc<[Token]>,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LiteralMode {
    Special,
    Direct,
    Page,
    Origin,
}
impl LiteralMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Special => "special",
            Self::Direct => "direct",
            Self::Page => "page",
            Self::Origin => "origin",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpecialEffect {
    pub late: bool,
    pub mode: LiteralMode,
    pub tokens: Arc<[Token]>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorAction {
    Set,
    Push,
    Pop,
    Current,
}
impl ColorAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Push => "push",
            Self::Pop => "pop",
            Self::Current => "current",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PdfEffect {
    RefObject(usize),
    SavePosition,
    RefForm(usize),
    RefImage(usize),
    Save,
    Restore,
    SetMatrix(String),
    ColorStack {
        number: usize,
        action: ColorAction,
        text: String,
    },
    SnapReferencePoint,
    SnapY(GlueSpec),
    SnapCompensation(i64),
    ExtractionOn,
    ExtractionOff,
    FakeSpace,
    Annotation(usize),
    StartLink(usize),
    EndLink,
    Destination(usize),
    Thread(usize),
    StartThread(usize),
    EndThread,
    RunningLinkOn,
    RunningLinkOff,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub enum NodePayload {
    #[default]
    None,
    CharacterPolicy {
        font: usize,
        code: u32,
    },
    Mark {
        number: i64,
        tokens: Arc<[Token]>,
    },
    Insertion {
        number: i64,
        split_top_skip: GlueSpec,
        floating_penalty: i64,
    },
    Adjustment {
        pre: bool,
    },
    Io(IoEffect),
    Special(SpecialEffect),
    Pdf(PdfEffect),
}
/// Independent graph links with immutable, copy-on-write semantic contents.
/// Rebuilding or consuming links never detaches contents. Scalar/payload writes
/// detach one node through DerefMut, including writes on the original owner.
#[derive(Debug)]
pub struct Node {
    // Presentation ownership is copied with native material, but never serialized
    // into formats or interpreted as a dependency/equivalence certificate.
    pub(crate) preview_owner: Option<crate::paragraph_preview::Owner>,
    pub link: i64,
    pub list_ptr: i64,
    pub pre_break: i64,
    pub post_break: i64,
    pub leader_ptr: i64,
    data: Arc<NodeData>,
}

// Unique material generations avoid retaining execution allocations or minting
// an Arc allocation for every placement. Cloning immutable contents preserves the
// generation; DerefMut clears it after copy-on-write isolation.
#[derive(Debug, Default)]
struct MapIdentity(std::sync::atomic::AtomicU64);
impl Clone for MapIdentity {
    fn clone(&self) -> Self {
        Self(std::sync::atomic::AtomicU64::new(
            self.0.load(std::sync::atomic::Ordering::Relaxed),
        ))
    }
}
impl MapIdentity {
    fn get(&self) -> std::num::NonZeroU64 {
        use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let current = self.0.load(Relaxed);
        if let Some(id) = std::num::NonZeroU64::new(current) {
            return id;
        }
        let id = NEXT
            .try_update(Relaxed, Relaxed, |id| id.checked_add(1))
            .expect("material identity exhausted");
        std::num::NonZeroU64::new(
            self.0
                .compare_exchange(0, id, Relaxed, Relaxed)
                .map(|_| id)
                .unwrap_or_else(|winner| winner),
        )
        .unwrap()
    }
}
#[derive(Clone, Debug)]
pub struct NodeData {
    map_identity: MapIdentity,
    pub node_type: NodeKind,
    pub width: i64,
    pub height: i64,
    pub depth: i64,
    pub font: usize,
    pub char: u32,
    pub shaped: Option<Arc<ShapedGlyph>>,
    pub penalty: i64,
    pub line_number: usize,
    pub discretionary_break: bool,
    pub automatic: bool,
    pub hyphen_position: usize,
    pub language: i64,
    pub left_hyphen_min: i64,
    pub right_hyphen_min: i64,
    pub components: Vec<u32>,
    pub left_boundary: bool,
    pub right_boundary: bool,
    pub kern_kind: String,
    pub snap_adjustment: Option<i64>,
    pub shift: i64,
    pub running_width: bool,
    pub running_height: bool,
    pub running_depth: bool,
    pub origins: crate::core::source::NodeOrigins,
    pub glue: Option<GlueSpec>,
    pub leader_kind: String,
    pub glue_kind: String,
    pub whatsit: String,
    pub payload: NodePayload,
    pub glue_sign: i64,
    pub glue_order: u8,
    pub glue_numerator: i64,
    pub glue_denominator: i64,
    pub natural_size: Option<i64>,
    pub badness: i64,
    pub overfull: i64,
    pub box_warning: Option<Arc<str>>,
}
/// Construction and wire-format fields in the original serialized order.
/// Allocation moves these values into a node; serialization keeps sharing an
/// implementation detail so existing initialized formats remain compatible.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeFields {
    pub node_type: NodeKind,
    pub link: i64,
    pub width: i64,
    pub height: i64,
    pub depth: i64,
    pub font: usize,
    pub char: u32,
    pub shaped: Option<Arc<ShapedGlyph>>,
    pub list_ptr: i64,
    pub pre_break: i64,
    pub post_break: i64,
    pub penalty: i64,
    pub line_number: usize,
    pub discretionary_break: bool,
    pub automatic: bool,
    pub hyphen_position: usize,
    pub language: i64,
    pub left_hyphen_min: i64,
    pub right_hyphen_min: i64,
    pub components: Vec<u32>,
    pub left_boundary: bool,
    pub right_boundary: bool,
    pub kern_kind: String,
    pub snap_adjustment: Option<i64>,
    pub shift: i64,
    pub running_width: bool,
    pub running_height: bool,
    pub running_depth: bool,
    pub origins: Vec<Option<Arc<SourceSpan>>>,
    pub glue: Option<GlueSpec>,
    pub leader_ptr: i64,
    pub leader_kind: String,
    pub glue_kind: String,
    pub whatsit: String,
    pub payload: NodePayload,
    pub glue_sign: i64,
    pub glue_order: u8,
    pub glue_numerator: i64,
    pub glue_denominator: i64,
    pub natural_size: Option<i64>,
    pub badness: i64,
    pub overfull: i64,
    pub box_warning: Option<Arc<str>>,
}
impl Default for NodeFields {
    fn default() -> Self {
        Self {
            node_type: NodeKind::Hlist,
            link: C::NULL,
            width: 0,
            height: 0,
            depth: 0,
            font: 0,
            char: 0,
            shaped: None,
            list_ptr: C::NULL,
            pre_break: C::NULL,
            post_break: C::NULL,
            penalty: 0,
            line_number: 0,
            discretionary_break: false,
            automatic: false,
            hyphen_position: 0,
            language: 0,
            left_hyphen_min: 1,
            right_hyphen_min: 1,
            components: Vec::new(),
            left_boundary: false,
            right_boundary: false,
            kern_kind: "normal".into(),
            snap_adjustment: None,
            shift: 0,
            running_width: false,
            running_height: false,
            running_depth: false,
            origins: Vec::new(),
            glue: None,
            leader_ptr: C::NULL,
            leader_kind: String::new(),
            glue_kind: "normal".into(),
            whatsit: String::new(),
            payload: NodePayload::None,
            glue_sign: 0,
            glue_order: 0,
            glue_numerator: 0,
            glue_denominator: 1,
            natural_size: None,
            badness: 0,
            overfull: 0,
            box_warning: None,
        }
    }
}
impl From<NodeFields> for Node {
    fn from(fields: NodeFields) -> Self {
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::count("node_data_allocations", 1);
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::count(
            "node_data_allocation_record_bytes",
            std::mem::size_of::<NodeData>(),
        );
        Self {
            link: fields.link,
            list_ptr: fields.list_ptr,
            pre_break: fields.pre_break,
            post_break: fields.post_break,
            leader_ptr: fields.leader_ptr,
            preview_owner: None,
            data: Arc::new(NodeData {
                map_identity: MapIdentity::default(),
                node_type: fields.node_type,
                width: fields.width,
                height: fields.height,
                depth: fields.depth,
                font: fields.font,
                char: fields.char,
                shaped: fields.shaped,
                penalty: fields.penalty,
                line_number: fields.line_number,
                discretionary_break: fields.discretionary_break,
                automatic: fields.automatic,
                hyphen_position: fields.hyphen_position,
                language: fields.language,
                left_hyphen_min: fields.left_hyphen_min,
                right_hyphen_min: fields.right_hyphen_min,
                components: fields.components,
                left_boundary: fields.left_boundary,
                right_boundary: fields.right_boundary,
                kern_kind: fields.kern_kind,
                snap_adjustment: fields.snap_adjustment,
                shift: fields.shift,
                running_width: fields.running_width,
                running_height: fields.running_height,
                running_depth: fields.running_depth,
                origins: fields.origins.into(),
                glue: fields.glue,
                leader_kind: fields.leader_kind,
                glue_kind: fields.glue_kind,
                whatsit: fields.whatsit,
                payload: fields.payload,
                glue_sign: fields.glue_sign,
                glue_order: fields.glue_order,
                glue_numerator: fields.glue_numerator,
                glue_denominator: fields.glue_denominator,
                natural_size: fields.natural_size,
                badness: fields.badness,
                overfull: fields.overfull,
                box_warning: fields.box_warning,
            }),
        }
    }
}
impl Default for Node {
    fn default() -> Self {
        NodeFields::default().into()
    }
}
impl Node {
    pub(crate) fn map_material(&self) -> std::num::NonZeroU64 {
        self.data.map_identity.get()
    }
    /// Only validated execution imports may replace source coordinates without
    /// changing semantic material identity. Keep this operation on the contents
    /// so every node/checkpoint need not carry a separate presentation override.
    pub(crate) fn rebase_map_origins(&mut self, origins: Vec<Option<Arc<SourceSpan>>>) {
        let material = self.map_material();
        let data = Arc::make_mut(&mut self.data);
        data.origins = origins.into();
        data.map_identity
            .0
            .store(material.get(), std::sync::atomic::Ordering::Relaxed);
    }
    pub(crate) fn shares_contents_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data, &other.data)
    }
}
impl Clone for Node {
    fn clone(&self) -> Self {
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::count("shared_node_data_references", 1);
        Self {
            link: self.link,
            list_ptr: self.list_ptr,
            pre_break: self.pre_break,
            post_break: self.post_break,
            leader_ptr: self.leader_ptr,
            data: self.data.clone(),
            preview_owner: self.preview_owner.clone(),
        }
    }
}
impl std::ops::Deref for Node {
    type Target = NodeData;
    fn deref(&self) -> &NodeData {
        &self.data
    }
}
impl std::ops::DerefMut for Node {
    fn deref_mut(&mut self) -> &mut NodeData {
        #[cfg(feature = "latency-diagnostics")]
        let shared = Arc::strong_count(&self.data) > 1 || Arc::weak_count(&self.data) > 0;
        #[cfg(feature = "latency-diagnostics")]
        let _diag = shared
            .then(|| crate::latency_diagnostics::phase("node_data_materialization", "", None));
        #[cfg(feature = "latency-diagnostics")]
        if shared {
            crate::latency_diagnostics::count("node_data_materializations", 1);
            crate::latency_diagnostics::count(
                "node_data_materialization_record_bytes",
                std::mem::size_of::<NodeData>(),
            );
        }
        let data = Arc::make_mut(&mut self.data);
        data.map_identity
            .0
            .store(0, std::sync::atomic::Ordering::Relaxed);
        data
    }
}
// Preserve the original field sequence and shape, including bincode formats.
impl Serialize for Node {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("Node", 43)?;
        state.serialize_field("node_type", &self.node_type)?;
        state.serialize_field("link", &self.link)?;
        state.serialize_field("width", &self.width)?;
        state.serialize_field("height", &self.height)?;
        state.serialize_field("depth", &self.depth)?;
        state.serialize_field("font", &self.font)?;
        state.serialize_field("char", &self.char)?;
        state.serialize_field("shaped", &self.shaped)?;
        state.serialize_field("list_ptr", &self.list_ptr)?;
        state.serialize_field("pre_break", &self.pre_break)?;
        state.serialize_field("post_break", &self.post_break)?;
        state.serialize_field("penalty", &self.penalty)?;
        state.serialize_field("line_number", &self.line_number)?;
        state.serialize_field("discretionary_break", &self.discretionary_break)?;
        state.serialize_field("automatic", &self.automatic)?;
        state.serialize_field("hyphen_position", &self.hyphen_position)?;
        state.serialize_field("language", &self.language)?;
        state.serialize_field("left_hyphen_min", &self.left_hyphen_min)?;
        state.serialize_field("right_hyphen_min", &self.right_hyphen_min)?;
        state.serialize_field("components", &self.components)?;
        state.serialize_field("left_boundary", &self.left_boundary)?;
        state.serialize_field("right_boundary", &self.right_boundary)?;
        state.serialize_field("kern_kind", &self.kern_kind)?;
        state.serialize_field("snap_adjustment", &self.snap_adjustment)?;
        state.serialize_field("shift", &self.shift)?;
        state.serialize_field("running_width", &self.running_width)?;
        state.serialize_field("running_height", &self.running_height)?;
        state.serialize_field("running_depth", &self.running_depth)?;
        state.serialize_field("origins", &self.origins)?;
        state.serialize_field("glue", &self.glue)?;
        state.serialize_field("leader_ptr", &self.leader_ptr)?;
        state.serialize_field("leader_kind", &self.leader_kind)?;
        state.serialize_field("glue_kind", &self.glue_kind)?;
        state.serialize_field("whatsit", &self.whatsit)?;
        state.serialize_field("payload", &self.payload)?;
        state.serialize_field("glue_sign", &self.glue_sign)?;
        state.serialize_field("glue_order", &self.glue_order)?;
        state.serialize_field("glue_numerator", &self.glue_numerator)?;
        state.serialize_field("glue_denominator", &self.glue_denominator)?;
        state.serialize_field("natural_size", &self.natural_size)?;
        state.serialize_field("badness", &self.badness)?;
        state.serialize_field("overfull", &self.overfull)?;
        state.serialize_field("box_warning", &self.box_warning)?;
        state.end()
    }
}
impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Ok(NodeFields::deserialize(deserializer)?.into())
    }
}
impl Node {
    pub fn sized_whatsit(&self) -> bool {
        self.whatsit == "pdf"
            && matches!(
                self.payload,
                NodePayload::Pdf(PdfEffect::RefForm(_) | PdfEffect::RefImage(_))
            )
    }
    pub fn glyph(&self) -> bool {
        matches!(self.node_type, NodeKind::Char | NodeKind::Ligature)
    }
}

#[cfg(test)]
mod sharing_tests {
    use super::*;

    #[test]
    fn link_writes_share_contents_and_payload_writes_detach_in_both_directions() {
        let mut original = Node::from(NodeFields {
            components: vec![65, 66],
            origins: vec![Some(Arc::new(SourceSpan::new("owner.tex", 4, 2)))],
            payload: NodePayload::Pdf(PdfEffect::SetMatrix("1 0 0 1".into())),
            ..NodeFields::default()
        });
        let mut copied = original.clone();
        copied.link = 42;
        copied.list_ptr = 43;
        copied.pre_break = 44;
        copied.post_break = 45;
        copied.leader_ptr = 46;
        assert!(Arc::ptr_eq(&original.data, &copied.data));
        original.origins.clear();
        assert!(!Arc::ptr_eq(&original.data, &copied.data));
        assert_eq!(
            copied.origins[0].as_ref().unwrap().file.as_ref(),
            "owner.tex"
        );
        copied.components.push(67);
        assert_eq!(original.components, [65, 66]);
        assert_eq!(original.link, C::NULL);
        original.width = 123;
        assert_eq!(copied.width, 0);
    }

    #[test]
    fn shared_nodes_keep_the_original_binary_and_json_format_shape() {
        let fields = NodeFields {
            link: 7,
            list_ptr: 8,
            pre_break: 9,
            post_break: 10,
            leader_ptr: 11,
            width: 123,
            height: 456,
            depth: 789,
            components: vec![65, 66],
            origins: vec![Some(Arc::new(SourceSpan::new("format.tex", 3, 1)))],
            payload: NodePayload::Pdf(PdfEffect::SetMatrix("1 0 0 1".into())),
            ..NodeFields::default()
        };
        let node = Node::from(fields.clone());
        let bytes = bincode::serialize(&fields).unwrap();
        assert_eq!(bincode::serialize(&node).unwrap(), bytes);
        assert_eq!(
            serde_json::to_value(&node).unwrap(),
            serde_json::to_value(&fields).unwrap()
        );
        let restored: Node = bincode::deserialize(&bytes).unwrap();
        assert_eq!(bincode::serialize(&restored).unwrap(), bytes);
        let mut copied = restored.clone();
        copied.depth = 999;
        assert_eq!(restored.depth, 789);
        assert_eq!(copied.list_ptr, 8);
    }

    #[test]
    fn checkpoints_keep_contents_alive_only_until_the_last_owner_is_dropped() {
        use crate::core::capacities::Capacities;
        use crate::typesetting::nodes::NodeBuilder;
        let mut builder = NodeBuilder::new(Capacities::default());
        let root = builder.new_null_box().unwrap();
        let child = builder.new_null_box().unwrap();
        builder.node_mut(root).unwrap().list_ptr = child;
        builder.node_mut(child).unwrap().components = vec![65, 66];
        let weak = Arc::downgrade(&builder.node(child).unwrap().data);
        let copied = builder.copy_box(root).unwrap();
        let saved = builder.checkpoint_clone();
        builder.release(root, false).unwrap();
        builder.release(copied, false).unwrap();
        assert!(builder.nodes.is_empty());
        assert!(weak.upgrade().is_some());
        let mut restored = saved.checkpoint_clone();
        drop(saved);
        let copied_child = restored.node(copied).unwrap().list_ptr;
        restored.node_mut(copied_child).unwrap().components.push(67);
        assert_eq!(restored.node(child).unwrap().components, [65, 66]);
        restored.release(root, false).unwrap();
        restored.release(copied, false).unwrap();
        assert!(weak.upgrade().is_none());
    }
}
