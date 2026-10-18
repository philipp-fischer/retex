//! Immutable node and paint snapshots (`typesetting/display.py`).
use super::model::{Node, NodePayload};
use crate::core::constants::Constants as C;
use crate::core::enums::NodeKind;
use crate::core::values::GlueSpec;
use crate::fonts::unicode::ShapedGlyph;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackedNode {
    pub node_id: i64,
    pub kind: NodeKind,
    pub link: i64,
    pub child: i64,
    pub width_sp: i64,
    pub height_sp: i64,
    pub depth_sp: i64,
    pub font: usize,
    pub code: u32,
    pub origins: crate::core::source::NodeOrigins,
    pub box_warning: Option<Arc<str>>,
    pub glue: Option<GlueSpec>,
    pub glue_sign: i64,
    pub glue_order: u8,
    pub glue_numerator: i64,
    pub glue_denominator: i64,
    pub natural_size: Option<i64>,
    pub badness: i64,
    pub overfull: i64,
    pub shift_sp: i64,
    pub running_width: bool,
    pub running_height: bool,
    pub running_depth: bool,
    pub components: Vec<u32>,
    pub left_boundary: bool,
    pub right_boundary: bool,
    pub kern_kind: String,
    pub pre_break: i64,
    pub post_break: i64,
    pub penalty: i64,
    pub line_number: usize,
    pub discretionary_break: bool,
    pub glue_kind: String,
    pub automatic: bool,
    pub hyphen_position: usize,
    pub language: i64,
    pub left_hyphen_min: i64,
    pub right_hyphen_min: i64,
    pub leader_ptr: i64,
    pub leader_kind: String,
    pub whatsit: String,
    pub payload: NodePayload,
    pub shaped: Option<Arc<ShapedGlyph>>,
}
impl PackedNode {
    pub fn freeze(node_id: i64, node: &Node) -> Self {
        Self {
            node_id,
            kind: node.node_type,
            link: node.link,
            child: node.list_ptr,
            width_sp: node.width,
            height_sp: node.height,
            depth_sp: node.depth,
            font: node.font,
            code: node.char,
            origins: node.origins.clone(),
            box_warning: node.box_warning.clone(),
            glue: node.glue,
            glue_sign: node.glue_sign,
            glue_order: node.glue_order,
            glue_numerator: node.glue_numerator,
            glue_denominator: node.glue_denominator,
            natural_size: node.natural_size,
            badness: node.badness,
            overfull: node.overfull,
            shift_sp: node.shift,
            running_width: node.running_width,
            running_height: node.running_height,
            running_depth: node.running_depth,
            components: node.components.clone(),
            left_boundary: node.left_boundary,
            right_boundary: node.right_boundary,
            kern_kind: node.kern_kind.clone(),
            pre_break: node.pre_break,
            post_break: node.post_break,
            penalty: node.penalty,
            line_number: node.line_number,
            discretionary_break: node.discretionary_break,
            glue_kind: node.glue_kind.clone(),
            automatic: node.automatic,
            hyphen_position: node.hyphen_position,
            language: node.language,
            left_hyphen_min: node.left_hyphen_min,
            right_hyphen_min: node.right_hyphen_min,
            leader_ptr: node.leader_ptr,
            leader_kind: node.leader_kind.clone(),
            whatsit: node.whatsit.clone(),
            payload: node.payload.clone(),
            shaped: node.shaped.clone(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Placement {
    #[serde(skip)]
    pub(crate) map_material: Option<std::num::NonZeroU64>,
    #[serde(skip)]
    pub(crate) parameter: Option<crate::checked_material::Parameter>,
    #[serde(skip)]
    pub(crate) preview_owner: Option<crate::paragraph_preview::Owner>,
    pub placement_id: usize,
    pub node_id: i64,
    pub parent_id: Option<usize>,
    pub kind: String,
    pub x_sp: i64,
    pub baseline_y_sp: i64,
    pub width_sp: i64,
    pub height_sp: i64,
    pub depth_sp: i64,
    pub font: usize,
    pub code: u32,
    pub glyph_name: Option<String>,
    pub mapping: String,
    pub origins: crate::core::source::NodeOrigins,
    pub box_warning: Option<Arc<str>>,
    pub components: Vec<u32>,
    pub left_boundary: bool,
    pub right_boundary: bool,
    pub glue_kind: String,
    pub literal_mode: String,
    pub literal_text: String,
    pub shaped: Option<Arc<ShapedGlyph>>,
}
impl Default for Placement {
    fn default() -> Self {
        Self {
            map_material: None,
            parameter: None,
            preview_owner: None,
            placement_id: 0,
            node_id: C::NULL,
            parent_id: None,
            kind: String::new(),
            x_sp: 0,
            baseline_y_sp: 0,
            width_sp: 0,
            height_sp: 0,
            depth_sp: 0,
            font: 0,
            code: 0,
            glyph_name: None,
            mapping: String::new(),
            origins: Default::default(),
            box_warning: None,
            components: Vec::new(),
            left_boundary: false,
            right_boundary: false,
            glue_kind: "normal".into(),
            literal_mode: String::new(),
            literal_text: String::new(),
            shaped: None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmittedGlyph {
    pub placement_id: usize,
    pub code: Option<u32>,
    pub font: usize,
    pub size_bp: f64,
    pub x_bp: Option<f64>,
    pub baseline_y_bp: f64,
    pub advance_bp: Option<f64>,
    pub position_quality: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LineDisplayList {
    #[serde(default, skip_serializing)]
    pub(crate) source_geometry: Vec<crate::source_map::Geometry>,
    #[serde(skip)]
    pub(crate) preview_spans: Vec<crate::paragraph_preview::Span>,
    #[serde(skip)]
    pub(crate) local_spans: Vec<crate::pdf::emission::LocalSpan>,
    pub root_node: i64,
    pub baseline_y_sp: i64,
    pub placements: Vec<Placement>,
    pub nodes: Vec<PackedNode>,
    pub natural_width_sp: i64,
    pub target_width_sp: i64,
    pub overfull_sp: i64,
    pub operators: Vec<String>,
    pub emitted: Vec<EmittedGlyph>,
    pub position_adjustment_sp: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PageDisplayList {
    pub page: usize,
    pub width_sp: i64,
    pub height_sp: i64,
    pub lines: Vec<LineDisplayList>,
    pub magnification: i64,
}

/// Immutable shipped pages shared by resident rollback states. A capture copies
/// one root; appending detaches page pointers, never accumulated glyph/paint data.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SharedPageList(std::sync::Arc<Vec<std::sync::Arc<PageDisplayList>>>);
impl std::ops::Deref for SharedPageList {
    type Target = [std::sync::Arc<PageDisplayList>];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl SharedPageList {
    pub fn push(&mut self, page: PageDisplayList) {
        std::sync::Arc::make_mut(&mut self.0).push(std::sync::Arc::new(page));
    }
    pub fn shares_storage_with(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}
impl From<Vec<PageDisplayList>> for SharedPageList {
    fn from(pages: Vec<PageDisplayList>) -> Self {
        Self(std::sync::Arc::new(
            pages.into_iter().map(std::sync::Arc::new).collect(),
        ))
    }
}
