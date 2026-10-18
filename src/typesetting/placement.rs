//! Explicit shipment traversal; callbacks may reenter the interpreter.
use super::display::{LineDisplayList, PackedNode, Placement};
use super::layout::{Layout, TypesettingContext};
use super::model::{Node, NodePayload, PdfEffect};
use super::nodes::parameter;
use super::shipout::ShipoutServices;
use crate::core::constants::Constants as C;
use crate::core::enums::NodeKind;
use crate::core::errors::{Error, Result};
use crate::core::values::round_ratio;
use crate::fonts::formats::vf::PaintRecord;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
struct Visit {
    pointer: i64,
    x: i64,
    y: i64,
    parent: Option<usize>,
    vertical: bool,
    effective: Option<i64>,
    enclosing: Option<Node>,
    origin: (i64, i64),
    leader_extent: Option<i64>,
    leader_horizontal: bool,
    doing_leaders: bool,
}
struct Children {
    visit: Visit,
    node: Node,
    identifier: usize,
    pointer: i64,
    x: i64,
    y: i64,
    origin: (i64, i64),
    vertical: bool,
    seen: BTreeSet<i64>,
    total: i128,
    previous: i128,
    pending: Option<(i64, i64)>,
    displacement: i64,
}
enum Frame {
    Begin(Visit),
    AfterPayload(Prepared),
    Children(Children),
    Leaders {
        children: Vec<Visit>,
        next: usize,
        pointer: i64,
    },
    Finish {
        pointer: i64,
        entered: bool,
        displacement: i64,
    },
}

fn kind(node: &Node) -> &'static str {
    match node.node_type {
        NodeKind::Char | NodeKind::Ligature => "glyph",
        NodeKind::Glue => "glue",
        NodeKind::Kern => "kern",
        NodeKind::MarginKern => "margin_kern",
        NodeKind::Hlist => "hbox",
        NodeKind::Vlist => "vbox",
        NodeKind::Rule => "rule",
        NodeKind::Disc => "discretionary",
        NodeKind::Penalty => "penalty",
        NodeKind::Whatsit => "language",
        NodeKind::Math => "math",
        NodeKind::Adjust => "adjustment",
        _ => "unknown",
    }
}
fn record(
    layout: &mut Layout,
    context: &mut TypesettingContext<'_>,
    services: &mut Option<&mut dyn ShipoutServices>,
    records: &mut Vec<Placement>,
    placement: Placement,
) -> Result<()> {
    let id = placement.placement_id;
    let items = if let Some(services) = services.as_deref_mut() {
        services.capture_paint(layout, context, placement)?
    } else {
        vec![placement]
    };
    for mut item in items {
        if item.placement_id != id {
            item.placement_id = layout.placement_count;
            layout.placement_count += 1;
        }
        records.push(item);
    }
    Ok(())
}
impl Layout {
    pub fn place_line(
        &mut self,
        context: &mut TypesettingContext<'_>,
        pointer: i64,
        x: Option<i64>,
        baseline: Option<i64>,
        services: Option<&mut dyn ShipoutServices>,
        vertical: bool,
        enclosing: Option<Node>,
        origin: Option<(i64, i64)>,
        effective: Option<i64>,
    ) -> Result<LineDisplayList> {
        self.place_line_mode(
            context, pointer, x, baseline, services, vertical, enclosing, origin, effective, false,
        )
    }
    pub(crate) fn place_checked_line(
        &mut self,
        context: &mut TypesettingContext<'_>,
        pointer: i64,
        x: i64,
        baseline: i64,
    ) -> Result<LineDisplayList> {
        self.place_line_mode(
            context,
            pointer,
            Some(x),
            Some(baseline),
            None,
            true,
            None,
            None,
            None,
            true,
        )
    }
    fn place_line_mode(
        &mut self,
        context: &mut TypesettingContext<'_>,
        pointer: i64,
        x: Option<i64>,
        baseline: Option<i64>,
        mut services: Option<&mut dyn ShipoutServices>,
        vertical: bool,
        enclosing: Option<Node>,
        origin: Option<(i64, i64)>,
        effective: Option<i64>,
        local_paint: bool,
    ) -> Result<LineDisplayList> {
        let mut execution = PlaceLineExecution::new(
            self, pointer, x, baseline, vertical, enclosing, origin, effective,
        );
        execution.local_paint = local_paint;
        let mut returned = None;
        loop {
            let step = execution.resume(self, context, &mut services, returned.take())?;
            match step {
                PlaceStep::Continue => {}
                PlaceStep::Complete(line) => return Ok(line),
                PlaceStep::Yield(action) => {
                    let service = services
                        .as_deref_mut()
                        .ok_or_else(|| Error::runtime("Missing shipment services"))?;
                    returned = Some(match action {
                        PlaceAction::Io(effect) => {
                            service.perform_io(self, context, &effect).map(|_| None)
                        }
                        PlaceAction::Special(effect) => {
                            service.ship_special(self, context, &effect)
                        }
                    });
                }
            }
        }
    }
}

pub(crate) enum PlaceAction {
    Io(super::model::IoEffect),
    Special(super::model::SpecialEffect),
}
pub(crate) enum PlaceStep {
    Continue,
    Yield(PlaceAction),
    Complete(LineDisplayList),
}
struct Prepared {
    visit: Visit,
    node: Node,
    identifier: usize,
    kind: String,
    literal: Option<(String, String)>,
    displacement: i64,
}
pub(crate) struct PlaceLineExecution {
    pointer: i64,
    baseline: i64,
    records: Vec<Placement>,
    snapshots: BTreeMap<i64, PackedNode>,
    snapshot_order: Vec<i64>,
    ancestors: BTreeSet<i64>,
    entered: Vec<i64>,
    frames: Vec<Frame>,
    returned: i64,
    awaiting: bool,
    local_paint: bool,
}
impl PlaceLineExecution {
    pub(crate) fn new(
        layout: &Layout,
        pointer: i64,
        x: Option<i64>,
        baseline: Option<i64>,
        vertical: bool,
        enclosing: Option<Node>,
        origin: Option<(i64, i64)>,
        effective: Option<i64>,
    ) -> Self {
        let baseline = baseline.unwrap_or(C::page_margin() + layout.cur_v);
        let x = x.unwrap_or(C::page_margin());
        let records = Vec::new();
        let snapshots = BTreeMap::new();
        let snapshot_order = Vec::new();
        let ancestors = BTreeSet::new();
        let entered = Vec::new();
        let frames = vec![Frame::Begin(Visit {
            pointer,
            x,
            y: baseline,
            parent: None,
            vertical,
            effective,
            enclosing,
            origin: origin.unwrap_or((x, baseline)),
            leader_extent: None,
            leader_horizontal: false,
            doing_leaders: false,
        })];

        Self {
            pointer,
            baseline,
            records,
            snapshots,
            snapshot_order,
            ancestors,
            entered,
            frames,
            returned: 0,
            awaiting: false,
            local_paint: false,
        }
    }
    pub(crate) fn resume(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut Option<&mut dyn ShipoutServices>,
        returned: Option<Result<Option<(String, String)>>>,
    ) -> Result<PlaceStep> {
        let result = self.step(layout, context, services, returned);
        if result.is_err() {
            let mut cleanup_error = None;
            while self.entered.pop().is_some() {
                if let Some(service) = services.as_deref_mut() {
                    if let Err(error) = service.leave_box(layout, context) {
                        cleanup_error = Some(error);
                    }
                }
            }
            if let Some(error) = cleanup_error {
                return Err(error);
            }
        }
        result
    }
    fn step(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut Option<&mut dyn ShipoutServices>,
        returned: Option<Result<Option<(String, String)>>>,
    ) -> Result<PlaceStep> {
        if !self.awaiting && returned.is_some() {
            return Err(Error::runtime("Unrequested shipment effect result"));
        }
        if let Some(frame) = self.frames.pop() {
            match frame {
                Frame::Begin(visit) => {
                    if !self.ancestors.insert(visit.pointer) {
                        return Err(Error::value("Cycle in box hierarchy"));
                    }
                    let node = context.builder.nodes.paint_node(visit.pointer)?;
                    if !self.snapshots.contains_key(&visit.pointer) {
                        self.snapshot_order.push(visit.pointer);
                    }
                    self.snapshots
                        .insert(visit.pointer, PackedNode::freeze(visit.pointer, &node));
                    let identifier = layout.placement_count;
                    layout.placement_count += 1;
                    let mut kind = kind(&node).to_string();
                    let mut literal = None;
                    let mut displacement = 0;
                    let mut action = None;
                    match &node.payload {
                        NodePayload::Io(effect) => {
                            kind = "io".into();
                            if !visit.doing_leaders {
                                if services.is_some() {
                                    action = Some(PlaceAction::Io(effect.clone()));
                                }
                            }
                        }
                        NodePayload::Special(effect) => {
                            kind = "special".into();
                            if services.is_some() {
                                action = Some(PlaceAction::Special(effect.clone()));
                            }
                        }
                        NodePayload::Pdf(effect) => {
                            kind = "pdf_extension".into();
                            if services.is_some() {
                                if matches!(
                                    effect,
                                    PdfEffect::SnapReferencePoint
                                        | PdfEffect::SnapY(_)
                                        | PdfEffect::SnapCompensation(_)
                                ) {
                                    displacement = super::snapping::apply(
                                        layout,
                                        context,
                                        &node,
                                        visit.pointer,
                                        visit.y,
                                        visit.vertical,
                                        visit.enclosing.as_ref(),
                                    )?;
                                } else {
                                    literal = services.as_deref_mut().unwrap().ship_extension(
                                        layout,
                                        context,
                                        effect,
                                        visit.x,
                                        visit.y,
                                        visit.doing_leaders,
                                        &node,
                                    )?;
                                }
                            }
                        }
                        _ => {}
                    }
                    self.frames.push(Frame::AfterPayload(Prepared {
                        visit,
                        node,
                        identifier,
                        kind,
                        literal,
                        displacement,
                    }));
                    if let Some(action) = action {
                        self.awaiting = true;
                        return Ok(PlaceStep::Yield(action));
                    }
                    return Ok(PlaceStep::Continue);
                }
                Frame::AfterPayload(Prepared {
                    mut visit,
                    node,
                    identifier,
                    mut kind,
                    mut literal,
                    displacement,
                }) => {
                    if self.awaiting {
                        self.awaiting = false;
                        literal = returned
                            .ok_or_else(|| Error::runtime("Missing shipment effect result"))??;
                    }
                    if literal.is_some() {
                        kind = "literal".into();
                    }
                    if kind == "glyph" && services.is_some() {
                        context.fonts.bind(
                            node.font,
                            parameter(context.env, "pdfsuppresswarningdupmap", 0) != 0,
                            context.warn,
                        )?;
                    }
                    let virtual_font =
                        if kind == "glyph" && (services.is_some() || self.local_paint) {
                            context.fonts.items[node.font].virtual_font.clone()
                        } else {
                            None
                        };
                    if kind == "glyph" {
                        if virtual_font.is_some() {
                            kind = "virtual_glyph".into();
                        } else if let Some(service) = services.as_deref_mut() {
                            service.use_font(layout, context, node.font)?;
                        }
                    }
                    if visit.vertical && matches!(kind.as_str(), "glue" | "kern") {
                        kind.insert(0, 'v');
                    }
                    let (mut name, mapping) = if kind == "glyph" {
                        context.fonts.glyph_name(node.font, node.char)
                    } else {
                        (None, "none")
                    };
                    let mut mapping = mapping.to_string();
                    let mut width = visit.effective.unwrap_or(node.width);
                    let mut height = node.height;
                    let mut depth = node.depth;
                    if kind == "rule" {
                        if let Some(enclosing) = &visit.enclosing {
                            if node.running_width {
                                width = enclosing.width;
                            }
                            if node.running_height {
                                height = enclosing.height;
                            }
                            if node.running_depth {
                                depth = enclosing.depth;
                            }
                        }
                    }
                    if let Some(extent) = visit.leader_extent {
                        if visit.leader_horizontal {
                            width = extent;
                        } else {
                            height = extent;
                            depth = 0;
                        }
                    }
                    if let Some(shaped) = &node.shaped {
                        visit.x += shaped.x_offset_sp;
                        visit.y -= shaped.y_offset_sp;
                        let font = context.fonts.items[node.font]
                            .unicode_font
                            .as_ref()
                            .ok_or_else(|| Error::value("Shaped glyph without Unicode font"))?;
                        name = Some(font.glyph_names[shaped.glyph_id as usize].clone());
                        mapping = "unicode_shaped_cluster".into();
                    }
                    let (literal_mode, literal_text) = literal.unwrap_or_default();
                    record(
                        layout,
                        context,
                        services,
                        &mut self.records,
                        Placement {
                            map_material: Some(node.map_material()),
                            preview_owner: node.preview_owner.clone(),
                            parameter: context
                                .builder
                                .nodes
                                .observer
                                .as_ref()
                                .and_then(|o| o.borrow().tags.get(&visit.pointer).copied()),
                            placement_id: identifier,
                            node_id: visit.pointer,
                            parent_id: visit.parent,
                            kind,
                            x_sp: visit.x,
                            baseline_y_sp: visit.y,
                            width_sp: width,
                            height_sp: height,
                            depth_sp: depth,
                            font: node.font,
                            code: node.char,
                            glyph_name: name,
                            mapping,
                            origins: node.origins.clone(),
                            box_warning: node.box_warning.clone(),
                            components: node.components.clone(),
                            left_boundary: node.left_boundary,
                            right_boundary: node.right_boundary,
                            glue_kind: node.glue_kind.clone(),
                            literal_mode,
                            literal_text,
                            shaped: node.shaped.clone(),
                        },
                    )?;
                    if let Some(virtual_font) = virtual_font {
                        if self.local_paint
                            && !checked_packet(context.fonts, &virtual_font, node.char)
                        {
                            return Err(Error::value("exact-local:unsupported_virtual_packet"));
                        }
                        let parameter = context
                            .builder
                            .nodes
                            .observer
                            .as_ref()
                            .and_then(|o| o.borrow().tags.get(&visit.pointer).copied());
                        let mut paints = virtual_font.paint(
                            node.char,
                            context.fonts.items[node.font].size,
                            visit.x,
                            visit.y,
                        )?;
                        while let Some(paint) = paints.next(context.fonts, context.warn)? {
                            let mut placement = Placement {
                                map_material: Some(node.map_material()),
                                preview_owner: node.preview_owner.clone().map(|mut o| {
                                    o.root = false;
                                    o
                                }),
                                parameter,
                                placement_id: layout.placement_count,
                                node_id: visit.pointer,
                                parent_id: Some(identifier),
                                origins: node.origins.clone(),
                                mapping: "virtual_packet".into(),
                                ..Placement::default()
                            };
                            match paint {
                                PaintRecord::Glyph { x, y, font, code } => {
                                    if let Some(service) = services.as_deref_mut() {
                                        service.use_font(layout, context, font)?;
                                    }
                                    let physical = &context.fonts.items[font];
                                    placement.kind = "glyph".into();
                                    placement.x_sp = x;
                                    placement.baseline_y_sp = y;
                                    placement.font = font;
                                    placement.code = code;
                                    placement.width_sp = physical.get_char_width(code);
                                    placement.height_sp = physical.get_char_height(code);
                                    placement.depth_sp = physical.get_char_depth(code);
                                    let (name, source) = context.fonts.glyph_name(font, code);
                                    placement.glyph_name = name;
                                    placement.mapping = source.into();
                                }
                                PaintRecord::Rule {
                                    x,
                                    y,
                                    width,
                                    height,
                                } => {
                                    placement.kind = "rule".into();
                                    placement.x_sp = x;
                                    placement.baseline_y_sp = y;
                                    placement.width_sp = width;
                                    placement.height_sp = height;
                                }
                                PaintRecord::Special { x, y, text } => {
                                    if let (Some(observer), Some(parameter)) =
                                        (&context.builder.nodes.observer, parameter)
                                    {
                                        observer
                                            .borrow_mut()
                                            .misses
                                            .insert(parameter.candidate, "virtual_packet_effect");
                                    }
                                    if self.local_paint {
                                        return Err(Error::value(
                                            "exact-local:virtual_packet_effect",
                                        ));
                                    }
                                    placement.x_sp = x;
                                    placement.baseline_y_sp = y;
                                    let literal = services
                                        .as_deref_mut()
                                        .unwrap()
                                        .ship_special_text(layout, context, &text)?;
                                    placement.kind = if literal.is_some() {
                                        "literal"
                                    } else {
                                        "special"
                                    }
                                    .into();
                                    let (mode, text) = literal.unwrap_or_default();
                                    placement.literal_mode = mode;
                                    placement.literal_text = text;
                                }
                            }
                            record(layout, context, services, &mut self.records, placement)?;
                            layout.placement_count += 1;
                        }
                    }
                    if node.leader_ptr != C::NULL {
                        let leader = context.builder.nodes.paint_node(node.leader_ptr)?;
                        let mut children = Vec::new();
                        if leader.node_type == NodeKind::Rule {
                            children.push(Visit {
                                pointer: node.leader_ptr,
                                x: visit.x,
                                y: visit.y + if visit.vertical { width } else { 0 },
                                parent: Some(identifier),
                                vertical: false,
                                effective: None,
                                enclosing: visit.enclosing.clone(),
                                origin: visit.origin,
                                leader_extent: Some(width),
                                leader_horizontal: !visit.vertical,
                                doing_leaders: true,
                            });
                        } else {
                            let size = if visit.vertical {
                                leader.height + leader.depth
                            } else {
                                leader.width
                            };
                            let origin = if visit.vertical {
                                visit.origin.1
                            } else {
                                visit.origin.0
                            };
                            let start = if visit.vertical { visit.y } else { visit.x };
                            for position in super::classic_lists::leader_positions(
                                start,
                                origin,
                                width,
                                size,
                                &node.leader_kind,
                            )? {
                                children.push(Visit {
                                    pointer: node.leader_ptr,
                                    x: if visit.vertical {
                                        visit.x + leader.shift
                                    } else {
                                        position
                                    },
                                    y: if visit.vertical {
                                        position + leader.height
                                    } else {
                                        visit.y + leader.shift
                                    },
                                    parent: Some(identifier),
                                    vertical: false,
                                    effective: None,
                                    enclosing: visit.enclosing.clone(),
                                    origin: visit.origin,
                                    leader_extent: None,
                                    leader_horizontal: false,
                                    doing_leaders: true,
                                });
                            }
                        }
                        self.frames.push(Frame::Leaders {
                            children,
                            next: 0,
                            pointer: visit.pointer,
                        });
                        self.returned = 0;
                        return Ok(PlaceStep::Continue);
                    }
                    let box_node = matches!(node.node_type, NodeKind::Hlist | NodeKind::Vlist);
                    if box_node {
                        if let Some(service) = services.as_deref_mut() {
                            service.enter_box(
                                layout,
                                context,
                                &node,
                                visit.x,
                                visit.y,
                                visit.doing_leaders,
                            )?;
                            self.entered.push(visit.pointer);
                        }
                    }
                    let vertical = node.node_type == NodeKind::Vlist;
                    let y = visit.y - if vertical { node.height } else { 0 };
                    let origin = (visit.x, y);
                    let child = if matches!(node.node_type, NodeKind::Insert | NodeKind::Adjust) {
                        C::NULL
                    } else {
                        node.list_ptr
                    };
                    self.frames.push(Frame::Finish {
                        pointer: visit.pointer,
                        entered: box_node && services.is_some(),
                        displacement,
                    });
                    self.frames.push(Frame::Children(Children {
                        x: visit.x,
                        y,
                        visit,
                        node,
                        identifier,
                        pointer: child,
                        origin,
                        vertical,
                        seen: BTreeSet::new(),
                        total: 0,
                        previous: 0,
                        pending: None,
                        displacement,
                    }));
                    self.returned = 0;
                }
                Frame::Children(mut frame) => {
                    if let Some((pointer, advance)) = frame.pending.take() {
                        if frame.vertical {
                            frame.y += advance + self.returned;
                        } else {
                            frame.x += advance;
                        }
                        frame.pointer = context.builder.nodes.paint_node(pointer)?.link;
                    }
                    if frame.pointer == C::NULL {
                        self.returned = frame.displacement;
                        return Ok(PlaceStep::Continue);
                    }
                    if !frame.seen.insert(frame.pointer) {
                        return Err(Error::value("Cycle in node list"));
                    }
                    let child = context.builder.nodes.paint_node(frame.pointer)?;
                    let mut width = child.width;
                    if let Some(spec) = child.glue.filter(|_| frame.node.glue_sign != 0) {
                        let (order, amount) = if frame.node.glue_sign > 0 {
                            (spec.stretch_order, spec.stretch)
                        } else {
                            (spec.shrink_order, spec.shrink)
                        };
                        if order == frame.node.glue_order {
                            frame.total += amount as i128;
                            let mut adjustment = round_ratio(
                                frame.total * frame.node.glue_numerator as i128,
                                frame.node.glue_denominator as i128,
                            )?;
                            if layout.exact_repro {
                                adjustment = adjustment.clamp(
                                    -(super::layout::HISTORICAL_GLUE_BOUND_SP as i128),
                                    super::layout::HISTORICAL_GLUE_BOUND_SP as i128,
                                );
                            }
                            width += (frame.node.glue_sign as i128 * (adjustment - frame.previous))
                                as i64;
                            frame.previous = adjustment;
                        }
                    }
                    let is_box = matches!(
                        child.node_type,
                        NodeKind::Hlist | NodeKind::Vlist | NodeKind::Rule
                    ) || child.sized_whatsit();
                    if frame.vertical && is_box {
                        frame.y += child.height;
                    }
                    let visit = Visit {
                        pointer: frame.pointer,
                        x: frame.x + if frame.vertical { child.shift } else { 0 },
                        y: frame.y + if frame.vertical { 0 } else { child.shift },
                        parent: Some(frame.identifier),
                        vertical: frame.vertical,
                        effective: Some(width),
                        enclosing: if !frame.vertical && frame.node.node_type == NodeKind::Disc {
                            frame.visit.enclosing.clone()
                        } else {
                            Some(frame.node.clone())
                        },
                        origin: frame.origin,
                        leader_extent: None,
                        leader_horizontal: false,
                        doing_leaders: frame.visit.doing_leaders,
                    };
                    frame.pending = Some((
                        frame.pointer,
                        if frame.vertical && is_box {
                            child.depth
                        } else {
                            width
                        },
                    ));
                    self.frames.push(Frame::Children(frame));
                    self.frames.push(Frame::Begin(visit));
                    self.returned = 0;
                }
                Frame::Leaders {
                    children,
                    mut next,
                    pointer,
                } => {
                    if next == children.len() {
                        self.ancestors.remove(&pointer);
                        self.returned = 0;
                        return Ok(PlaceStep::Continue);
                    }
                    let visit = children[next].clone();
                    next += 1;
                    self.frames.push(Frame::Leaders {
                        children,
                        next,
                        pointer,
                    });
                    self.frames.push(Frame::Begin(visit));
                    self.returned = 0;
                }
                Frame::Finish {
                    pointer,
                    entered: was_entered,
                    displacement,
                } => {
                    if was_entered {
                        self.entered.pop();
                        services
                            .as_deref_mut()
                            .unwrap()
                            .leave_box(layout, context)?;
                    }
                    self.ancestors.remove(&pointer);
                    self.returned = displacement;
                }
            }
            return Ok(PlaceStep::Continue);
        }
        let mut pending: Vec<_> = self
            .snapshots
            .values()
            .flat_map(|node| [node.child, node.pre_break, node.post_break, node.leader_ptr])
            .collect();
        while let Some(child) = pending.pop() {
            if child == C::NULL || self.snapshots.contains_key(&child) {
                continue;
            }
            let frozen = PackedNode::freeze(child, &context.builder.nodes.paint_node(child)?);
            pending.extend([
                frozen.link,
                frozen.child,
                frozen.pre_break,
                frozen.post_break,
                frozen.leader_ptr,
            ]);
            self.snapshot_order.push(child);
            self.snapshots.insert(child, frozen);
        }
        let root = context.builder.nodes.paint_node(self.pointer)?;
        Ok(PlaceStep::Complete(LineDisplayList {
            source_geometry: Vec::new(),
            preview_spans: Vec::new(),
            local_spans: Vec::new(),
            root_node: self.pointer,
            baseline_y_sp: self.baseline,
            placements: std::mem::take(&mut self.records),
            nodes: std::mem::take(&mut self.snapshot_order)
                .into_iter()
                .map(|pointer| self.snapshots.remove(&pointer).unwrap())
                .collect(),
            natural_width_sp: root.natural_size.unwrap_or(root.width),
            target_width_sp: if root.line_number != 0 {
                root.width
            } else {
                layout.hsize
            },
            overfull_sp: if root.overfull != 0 {
                root.overfull
            } else {
                (root.width - layout.hsize).max(0)
            },
            operators: Vec::new(),
            emitted: Vec::new(),
            position_adjustment_sp: self.returned,
        }))
    }
}

/// Bounded native paint packet contract. Only existing, fully bound physical
/// fonts may be selected. No nested packet, special, callback or new resource
/// is executed by preview replacement. Ordinary shipment remains unrestricted.
fn checked_packet(
    fonts: &crate::fonts::catalog::FontCatalog,
    vf: &crate::fonts::formats::vf::VirtualFont,
    code: u32,
) -> bool {
    use crate::fonts::formats::vf::VirtualOperation as Op;
    let Some(ops) = vf.packets.get(&code) else {
        return false;
    };
    if ops.len() > 64 {
        return false;
    }
    let mut current = vf
        .font_order
        .first()
        .and_then(|key| vf.fonts.get(key))
        .copied();
    let mut depth = 0usize;
    for op in ops.iter() {
        match op {
            Op::Font(number) => current = Some(*number),
            Op::Char { code, .. } => {
                if *code > 255
                    || !current.and_then(|n| fonts.items.get(n)).is_some_and(|f| {
                        f.virtual_checked
                            && f.virtual_font.is_none()
                            && f.unicode_font.is_none()
                            && f.materialization.resource_ready
                            && f.materialization.pdf_kind == "scalable"
                    })
                {
                    return false;
                }
            }
            Op::Special(_) => return false,
            Op::Push => {
                depth += 1;
                if depth > 16 {
                    return false;
                }
            }
            Op::Pop => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            Op::Rule { .. }
            | Op::Right(_)
            | Op::Down(_)
            | Op::W(_)
            | Op::X(_)
            | Op::Y(_)
            | Op::Z(_) => {}
        }
    }
    depth == 0
}
