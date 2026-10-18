//! Packing dimensions, cumulative glue and ordered placements (`layout.py`).
use super::display::LineDisplayList;
use super::model::Node;
use super::nodes::{NodeBuilder, parameter};
use super::shipout::ShipoutServices;
use crate::core::constants::Constants as C;
use crate::core::enums::NodeKind;
use crate::core::errors::{Error, Result};
use crate::core::values::{badness, checked, round_ratio};
use crate::fonts::catalog::FontCatalog;
use crate::language::state::Environment;
use crate::typesetting::model::NodeFields;
pub const HISTORICAL_GLUE_BOUND_SP: i64 = 1_000_000_000;

pub struct TypesettingContext<'a> {
    pub builder: &'a mut NodeBuilder,
    pub fonts: &'a mut FontCatalog,
    pub env: &'a mut Environment,
    pub warn: &'a mut dyn FnMut(&str),
}
#[derive(Clone, Debug)]
pub enum LayoutEvent {
    Paragraph(super::paragraphs::ParagraphEvent),
    GlueSet {
        natural: i64,
        desired: i64,
        sign: i64,
        order: u8,
        numerator: i64,
        denominator: i64,
        badness: i64,
        overfull: i64,
    },
    OverfullHbox {
        natural: i64,
        target: i64,
        excess: i64,
    },
    Shipout {
        page: usize,
        width: i64,
        height: i64,
        x: i64,
        y: i64,
    },
}
#[derive(Clone)]
pub struct Layout {
    /// Internal write observation for grouped construction replay.
    pub(crate) pack_serial: usize,
    pub pages: super::display::SharedPageList,
    pub placement_count: usize,
    last_badness: i64,
    badness_parameter: Option<(crate::checked_material::Observer, usize)>,
    pub page_contents: Vec<LineDisplayList>,
    pub hsize: i64,
    pub vsize: i64,
    pub cur_v: i64,
    pub paragraph_count: usize,
    pub page_depth: i64,
    pub page_width: i64,
    pub page_goal: Option<i64>,
    pub page_max_depth: Option<i64>,
    pub greedy_paragraphs: bool,
    pub magnification: Option<i64>,
    pub exact_repro: bool,
    pub recovery_placeholders: bool,
    pub snap_reference_y: i64,
    pub events: Vec<LayoutEvent>,
    pub tracing: bool,
}
impl Default for Layout {
    fn default() -> Self {
        Self {
            pack_serial: 0,
            pages: Default::default(),
            placement_count: 0,
            last_badness: 0,
            badness_parameter: None,
            page_contents: Vec::new(),
            hsize: C::page_width() - 2 * C::page_margin(),
            vsize: C::page_height() - 2 * C::page_margin(),
            cur_v: 0,
            paragraph_count: 0,
            page_depth: 0,
            page_width: 0,
            page_goal: None,
            page_max_depth: None,
            greedy_paragraphs: false,
            magnification: None,
            exact_repro: false,
            recovery_placeholders: false,
            snap_reference_y: 0,
            events: Vec::new(),
            tracing: false,
        }
    }
}
impl Layout {
    pub fn last_badness(&self) -> i64 {
        if let Some((observer, candidate)) = &self.badness_parameter {
            observer.borrow_mut().badness_observed.insert(*candidate);
        }
        self.last_badness
    }
    pub(crate) fn last_badness_value(&self) -> i64 {
        self.last_badness
    }
    pub(crate) fn set_last_badness(&mut self, value: i64) {
        self.last_badness = value;
        self.badness_parameter = None;
    }
    pub(crate) fn parameterize_badness(
        &mut self,
        observer: crate::checked_material::Observer,
        candidate: usize,
    ) {
        self.badness_parameter = Some((observer, candidate));
    }
    pub(crate) fn concrete_snapshot(&self) -> Self {
        let mut layout = self.clone();
        layout.badness_parameter = None;
        layout
    }

    pub fn pack_paragraph(
        &mut self,
        context: &mut TypesettingContext<'_>,
        hyphenation: &mut super::hyphenation::Hyphenation,
        profile: crate::core::characters::LanguageProfile,
        head: i64,
        prev_graf: i64,
    ) -> Result<Vec<i64>> {
        self.hsize = parameter(context.env, "hsize", self.hsize);
        if self.greedy_paragraphs {
            let heads = self.break_paragraph(context, head)?;
            heads
                .into_iter()
                .map(|head| self.hpack(context, head, None, false, false))
                .collect()
        } else {
            let mut paragraph = super::paragraphs::Paragraph::new(context, head, true)?;
            let result = paragraph.optimal(self, context, hyphenation, profile, prev_graf);
            self.events
                .extend(paragraph.events.into_iter().map(LayoutEvent::Paragraph));
            result
        }
    }
    pub fn break_paragraph(
        &mut self,
        context: &mut TypesettingContext<'_>,
        head: i64,
    ) -> Result<Vec<i64>> {
        self.hsize = parameter(context.env, "hsize", self.hsize);
        let mut paragraph = super::paragraphs::Paragraph::new(context, head, false)?;
        let lines = paragraph.greedy(context, self.hsize)?;
        self.events
            .extend(paragraph.events.into_iter().map(LayoutEvent::Paragraph));
        for &line in &lines {
            let width = context
                .builder
                .siblings(line)?
                .into_iter()
                .try_fold(0, |width, pointer| {
                    Ok::<_, Error>(width + context.builder.node(pointer)?.width)
                })?;
            let excess = (width - self.hsize).max(0);
            if excess > 0 {
                (context.warn)(&format!(
                    "Overfull hbox ({:.5}pt too wide)",
                    excess as f64 / C::UNITY as f64
                ));
                self.events.push(LayoutEvent::OverfullHbox {
                    natural: width,
                    target: self.hsize,
                    excess,
                });
            }
        }
        Ok(lines)
    }
    pub fn hpack(
        &mut self,
        context: &mut TypesettingContext<'_>,
        head: i64,
        target: Option<i64>,
        spread: bool,
        expand: bool,
    ) -> Result<i64> {
        if expand {
            if let Some(target) = target {
                super::font_expansion::pack(context, head, target)?;
            }
        }
        let mut total_width = 0;
        let mut max_height = 0;
        let mut max_depth = 0;
        for pointer in context.builder.siblings(head)? {
            let node = context.builder.nodes.shape(pointer)?;
            total_width += node.width;
            let shift = if matches!(node.node_type, NodeKind::Hlist | NodeKind::Vlist) {
                node.shift
            } else {
                0
            };
            if !matches!(
                node.node_type,
                NodeKind::Insert | NodeKind::Mark | NodeKind::Adjust
            ) {
                if !node.running_height {
                    max_height = max_height.max(node.height - shift);
                }
                if !node.running_depth {
                    max_depth = max_depth.max(node.depth + shift);
                }
            }
            if node.leader_ptr != C::NULL {
                let leader = context.builder.node(node.leader_ptr)?;
                if !leader.running_height {
                    max_height = max_height.max(leader.height);
                }
                if !leader.running_depth {
                    max_depth = max_depth.max(leader.depth);
                }
            }
        }
        let mut hbox = Node::from(NodeFields {
            node_type: NodeKind::Hlist,
            list_ptr: head,
            width: total_width,
            height: max_height,
            depth: max_depth,
            ..NodeFields::default()
        });
        self.set_glue(context, &mut hbox, total_width, target, spread, false)?;
        if self.recovery_placeholders
            && context.builder.siblings(head)?.into_iter().any(|p| {
                context.builder.node(p).is_ok_and(|n| {
                    n.origins.iter().flatten().any(|s| {
                        s.expansion
                            .as_deref()
                            .is_some_and(|e| e.starts_with("macro:RTerr"))
                    })
                })
            })
        {
            hbox.box_warning=Some("Recovered region: content was replaced by a placeholder; fix the highlighted source".into());
        }
        self.set_last_badness(hbox.badness);
        self.pack_serial = self.pack_serial.wrapping_add(1);
        context.builder.allocate(hbox)
    }
    pub fn vpack(
        &mut self,
        context: &mut TypesettingContext<'_>,
        head: i64,
        top: bool,
        target: Option<i64>,
        spread: bool,
        max_depth: i64,
    ) -> Result<i64> {
        let mut height = 0;
        let mut depth = 0;
        let mut width = 0;
        for pointer in context.builder.siblings(head)? {
            let node = context.builder.nodes.shape(pointer)?;
            if matches!(
                node.node_type,
                NodeKind::Hlist | NodeKind::Vlist | NodeKind::Rule
            ) || node.sized_whatsit()
            {
                height += depth + node.height;
                depth = node.depth;
                width = width.max(node.width + node.shift);
            } else if matches!(node.node_type, NodeKind::Glue | NodeKind::Kern) {
                height += depth + node.width;
                depth = 0;
                if node.leader_ptr != C::NULL {
                    let leader = context.builder.node(node.leader_ptr)?;
                    if !leader.running_width {
                        width = width.max(leader.width);
                    }
                }
            } else if !matches!(
                node.node_type,
                NodeKind::Penalty | NodeKind::Whatsit | NodeKind::Mark | NodeKind::Insert
            ) {
                return Err(Error::value("Character node in vertical list"));
            }
        }
        if depth > max_depth {
            height += depth - max_depth;
            depth = max_depth;
        }
        let mut vbox = Node::from(NodeFields {
            node_type: NodeKind::Vlist,
            list_ptr: head,
            width,
            height,
            depth,
            ..NodeFields::default()
        });
        self.set_glue(context, &mut vbox, height, target, spread, true)?;
        self.set_last_badness(vbox.badness);
        self.pack_serial = self.pack_serial.wrapping_add(1);
        if top {
            let height = vbox.height;
            vbox.height = if head != C::NULL {
                let first = context.builder.node(head)?;
                if matches!(
                    first.node_type,
                    NodeKind::Hlist | NodeKind::Vlist | NodeKind::Rule
                ) {
                    first.height
                } else {
                    0
                }
            } else {
                0
            };
            vbox.depth = height + depth - vbox.height;
        }
        context.builder.allocate(vbox)
    }
    pub fn set_glue(
        &mut self,
        context: &mut TypesettingContext<'_>,
        node: &mut Node,
        natural: i64,
        target: Option<i64>,
        spread: bool,
        vertical: bool,
    ) -> Result<()> {
        node.natural_size = Some(natural);
        let Some(target) = target else {
            return Ok(());
        };
        let desired = checked(
            if spread {
                natural as i128 + target as i128
            } else {
                target as i128
            },
            true,
        )?;
        if vertical {
            node.height = desired;
        } else {
            node.width = desired;
        }
        let excess = desired - natural;
        if excess == 0 {
            return Ok(());
        }
        let mut totals = [0_i64; 4];
        let pointers = context.builder.siblings(node.list_ptr)?;
        for &pointer in &pointers {
            if let Some(spec) = context.builder.nodes.shape(pointer)?.glue {
                let (order, amount) = if excess > 0 {
                    (spec.stretch_order, spec.stretch)
                } else {
                    (spec.shrink_order, spec.shrink)
                };
                totals[order as usize] += amount;
            }
        }
        let order = (0..4).rev().find(|&index| totals[index] != 0).unwrap_or(0);
        let amount = excess.abs();
        let available = totals[order];
        node.glue_order = order as u8;
        node.badness = if order != 0 {
            0
        } else {
            badness(amount, available)
        };
        node.glue_sign = if available == 0 {
            0
        } else if excess > 0 {
            1
        } else {
            -1
        };
        node.glue_numerator = if available > 0 {
            amount
        } else if available < 0 {
            -amount
        } else {
            0
        };
        node.glue_denominator = if available == 0 { 1 } else { available.abs() };
        let nonempty = node.list_ptr != C::NULL;
        let threshold = parameter(
            context.env,
            if vertical { "vbadness" } else { "hbadness" },
            1000,
        );
        let kind = if vertical { "vbox" } else { "hbox" };
        if excess < 0 && order == 0 && amount > available && nonempty {
            node.overfull = amount - available;
            node.glue_numerator = 1;
            node.glue_denominator = 1;
            node.badness = 1_000_000;
            let fuzz = parameter(context.env, if vertical { "vfuzz" } else { "hfuzz" }, 0);
            let rule_width = parameter(context.env, "overfullrule", 0);
            if !vertical && rule_width > 0 && node.overfull > fuzz {
                let rule = context.builder.allocate(Node::from(NodeFields {
                    node_type: NodeKind::Rule,
                    width: rule_width,
                    running_height: true,
                    running_depth: true,
                    ..NodeFields::default()
                }))?;
                context.builder.node_mut(*pointers.last().unwrap())?.link = rule;
            }
            if node.overfull > fuzz || threshold < 100 {
                let message = format!(
                    "Overfull {kind} ({:.5}pt too {})",
                    node.overfull as f64 / C::UNITY as f64,
                    if vertical { "high" } else { "wide" }
                );
                (context.warn)(&message);
                node.box_warning = Some(message.into());
            }
        } else if node.badness > threshold && nonempty {
            let message = format!(
                "{} {kind} (badness {})",
                if excess > 0 { "Underfull" } else { "Tight" },
                node.badness
            );
            (context.warn)(&message);
            node.box_warning = Some(message.into());
        }
        if !nonempty {
            node.badness = 0;
        }
        if self.tracing {
            self.events.push(LayoutEvent::GlueSet {
                natural,
                desired,
                sign: node.glue_sign,
                order: order as u8,
                numerator: node.glue_numerator,
                denominator: node.glue_denominator,
                badness: node.badness,
                overfull: node.overfull,
            });
        }
        Ok(())
    }
    pub fn effective_widths(
        &self,
        builder: &NodeBuilder,
        node: &Node,
        output: bool,
    ) -> Result<Vec<(i64, i64)>> {
        let mut result = Vec::new();
        let mut total = 0_i128;
        let mut previous = 0_i128;
        for pointer in builder.siblings(node.list_ptr)? {
            let child = builder.node(pointer)?;
            let mut width = child.width;
            if let Some(spec) = child.glue.filter(|_| node.glue_sign != 0) {
                let (order, amount) = if node.glue_sign > 0 {
                    (spec.stretch_order, spec.stretch)
                } else {
                    (spec.shrink_order, spec.shrink)
                };
                if order == node.glue_order {
                    total += amount as i128;
                    let mut adjustment = round_ratio(
                        total * node.glue_numerator as i128,
                        node.glue_denominator as i128,
                    )?;
                    if output && self.exact_repro {
                        adjustment = adjustment.clamp(
                            -(HISTORICAL_GLUE_BOUND_SP as i128),
                            HISTORICAL_GLUE_BOUND_SP as i128,
                        );
                    }
                    width += (node.glue_sign as i128 * (adjustment - previous)) as i64;
                    previous = adjustment;
                }
            }
            result.push((pointer, width));
        }
        Ok(result)
    }
    pub fn page_geometry(
        &self,
        env: &Environment,
        width: i64,
        height: i64,
    ) -> Result<(i64, i64, i64, i64)> {
        let x = parameter(env, "pdfhorigin", 0) + parameter(env, "hoffset", 0);
        let y = parameter(env, "pdfvorigin", 0) + parameter(env, "voffset", 0);
        let configured_width = parameter(env, "pdfpagewidth", 0);
        let configured_height = parameter(env, "pdfpageheight", 0);
        let page_width = if configured_width == 0 {
            width + 2 * x
        } else {
            configured_width
        };
        let page_height = if configured_height == 0 {
            height + 2 * y
        } else {
            configured_height
        };
        if page_width <= 0 || page_height <= 0 {
            return Err(Error::value("Nonpositive PDF page dimensions")
                .with_issue(crate::core::recovery::Issue::PageGeometry));
        }
        Ok((x, y, page_width, page_height))
    }
    pub fn prepare_magnification(&mut self, env: &Environment) -> Result<i64> {
        let value = parameter(env, "mag", 1000);
        if !(1..=32768).contains(&value) {
            return Err(Error::value("Illegal magnification")
                .with_issue(crate::core::recovery::Issue::PageGeometry));
        }
        if let Some(previous) = self.magnification.filter(|previous| *previous != value) {
            return Err(Error::value(format!(
                "Incompatible magnification: {value}; already using {previous}"
            )));
        }
        self.magnification = Some(value);
        Ok(value)
    }
    pub fn ship_box(
        &mut self,
        context: &mut TypesettingContext<'_>,
        pointer: i64,
        services: &mut dyn ShipoutServices,
    ) -> Result<()> {
        let hold = context.builder.ownership.hold([pointer]);
        let result = self.ship_box_owned(context, pointer, services);
        let cleanup = context.builder.release(pointer, false);
        drop(hold);
        result.and(cleanup)
    }
    pub fn ship_box_owned(
        &mut self,
        context: &mut TypesettingContext<'_>,
        pointer: i64,
        services: &mut dyn ShipoutServices,
    ) -> Result<()> {
        super::shipment_execution::ShipmentExecution::new(
            pointer,
            super::shipment_execution::ShipmentKind::Explicit,
        )
        .drive(self, context, services)
    }
}
