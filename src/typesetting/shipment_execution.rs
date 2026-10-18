//! Owned page/box traversal, retaining ordered shipment effects between slices.
use super::{
    display::{LineDisplayList, PageDisplayList},
    layout::{HISTORICAL_GLUE_BOUND_SP, Layout, TypesettingContext},
    model::{Node, NodePayload},
    placement::{PlaceAction, PlaceLineExecution, PlaceStep},
    shipout::ShipoutServices,
    vertical::is_box,
};
use crate::core::{
    constants::Constants as C,
    enums::NodeKind,
    errors::{Error, Result},
    values::round_ratio,
};
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
pub(crate) enum ShipmentKind {
    Explicit,
    Built,
}
pub(crate) enum ShipmentStep {
    Continue,
    Effect(PlaceAction),
    FinishPage(usize),
    Complete,
}
#[derive(Clone)]
enum Phase {
    Start,
    Walk,
    Place,
    Io,
    Finish,
    FinishedPage,
    Done,
}
pub(crate) struct ShipmentExecution {
    kind: ShipmentKind,
    root: i64,
    phase: Phase,
    node: Option<Node>,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    magnification: i64,
    page_use: usize,
    origin: (i64, i64),
    pointer: i64,
    total: i128,
    previous: i128,
    seen: BTreeSet<i64>,
    lines: Vec<LineDisplayList>,
    placement: Option<PlaceLineExecution>,
    advance: i64,
    adjustment: bool,
    entered: bool,
}
impl ShipmentExecution {
    pub(crate) fn checkpoint_ready(&self) -> bool {
        matches!(self.phase, Phase::FinishedPage | Phase::Done) && self.placement.is_none()
    }
    pub(crate) fn checkpoint_clone(&self) -> Option<Self> {
        if !self.checkpoint_ready() {
            return None;
        }
        Some(Self {
            kind: self.kind,
            root: self.root,
            phase: self.phase.clone(),
            node: self.node.clone(),
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            magnification: self.magnification,
            page_use: self.page_use,
            origin: self.origin,
            pointer: self.pointer,
            total: self.total,
            previous: self.previous,
            seen: self.seen.clone(),
            lines: self.lines.clone(),
            placement: None,
            advance: self.advance,
            adjustment: self.adjustment,
            entered: self.entered,
        })
    }
    pub(crate) fn new(root: i64, kind: ShipmentKind) -> Self {
        Self {
            kind,
            root,
            phase: Phase::Start,
            node: None,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            magnification: 1000,
            page_use: 0,
            origin: (0, 0),
            pointer: C::NULL,
            total: 0,
            previous: 0,
            seen: BTreeSet::new(),
            lines: Vec::new(),
            placement: None,
            advance: 0,
            adjustment: false,
            entered: false,
        }
    }
    pub(crate) fn drive(
        mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn ShipoutServices,
    ) -> Result<()> {
        let mut returned = None;
        loop {
            match self.resume(layout, context, services, returned.take())? {
                ShipmentStep::Continue => {}
                ShipmentStep::Complete => return Ok(()),
                ShipmentStep::Effect(action) => {
                    returned = Some(match action {
                        PlaceAction::Io(effect) => {
                            services.perform_io(layout, context, &effect).map(|_| None)
                        }
                        PlaceAction::Special(effect) => {
                            services.ship_special(layout, context, &effect)
                        }
                    })
                }
                ShipmentStep::FinishPage(page) => {
                    returned = Some(services.finish_page(layout, context, page).map(|_| None))
                }
            }
        }
    }
    pub(crate) fn resume(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn ShipoutServices,
        returned: Option<Result<Option<(String, String)>>>,
    ) -> Result<ShipmentStep> {
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase(
            "shipment_and_placement",
            "",
            Some(crate::latency_diagnostics::Region {
                file: "@shipment".into(),
                ..Default::default()
            }),
        );
        let result = self.step(layout, context, services, returned);
        if result.is_err() && self.entered {
            self.entered = false;
            services.leave_box(layout, context)?;
        }
        result
    }
    fn step(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn ShipoutServices,
        returned: Option<Result<Option<(String, String)>>>,
    ) -> Result<ShipmentStep> {
        match self.phase {
            Phase::Start => {
                if returned.is_some() {
                    return Err(Error::runtime("Unrequested shipment result"));
                }
                let node = context.builder.node(self.root)?.clone();
                (self.x, self.y, self.width, self.height) =
                    layout.page_geometry(context.env, node.width, node.height + node.depth)?;
                self.magnification = layout.prepare_magnification(context.env)?;
                self.page_use = services.begin_page(
                    layout,
                    context,
                    self.height,
                    self.x,
                    self.y + node.height,
                )?;
                self.origin = (self.x, self.y);
                self.node = Some(node.clone());
                match self.kind {
                    ShipmentKind::Explicit => {
                        self.placement = Some(PlaceLineExecution::new(
                            layout,
                            self.root,
                            Some(self.x),
                            Some(self.y + node.height),
                            false,
                            None,
                            None,
                            None,
                        ));
                        self.phase = Phase::Place;
                    }
                    ShipmentKind::Built => {
                        services.enter_box(
                            layout,
                            context,
                            &node,
                            self.x,
                            self.y + node.height,
                            false,
                        )?;
                        self.entered = true;
                        self.pointer = node.list_ptr;
                        self.phase = Phase::Walk;
                    }
                }
            }
            Phase::Walk => {
                if returned.is_some() {
                    return Err(Error::runtime("Unexpected shipment traversal result"));
                }
                if self.pointer == C::NULL {
                    self.entered = false;
                    services.leave_box(layout, context)?;
                    self.phase = Phase::Finish;
                    return Ok(ShipmentStep::Continue);
                }
                if !self.seen.insert(self.pointer) {
                    return Err(Error::value("Cycle in page list"));
                }
                let node = context.builder.nodes.shape(self.pointer)?;
                let root = self.node.as_ref().expect("shipment root");
                let mut effective = node.width;
                if let Some(spec) = node.glue.filter(|_| root.glue_sign != 0) {
                    let (order, amount) = if root.glue_sign > 0 {
                        (spec.stretch_order, spec.stretch)
                    } else {
                        (spec.shrink_order, spec.shrink)
                    };
                    if order == root.glue_order {
                        self.total += amount as i128;
                        let mut adjustment = round_ratio(
                            self.total * root.glue_numerator as i128,
                            root.glue_denominator as i128,
                        )?;
                        if layout.exact_repro {
                            adjustment = adjustment.clamp(
                                -(HISTORICAL_GLUE_BOUND_SP as i128),
                                HISTORICAL_GLUE_BOUND_SP as i128,
                            );
                        }
                        effective += (root.glue_sign as i128 * (adjustment - self.previous)) as i64;
                        self.previous = adjustment;
                    }
                }
                if is_box(node.node_type) || node.sized_whatsit() {
                    self.y += node.height;
                    if node.whatsit != "end" {
                        if node.running_width {
                            context.builder.node_mut(self.pointer)?.width = root.width;
                        }
                        self.placement = Some(PlaceLineExecution::new(
                            layout,
                            self.pointer,
                            Some(self.x + node.shift),
                            Some(self.y),
                            false,
                            None,
                            None,
                            None,
                        ));
                        self.advance = node.depth;
                        self.adjustment = false;
                        self.phase = Phase::Place;
                    } else {
                        self.y += node.depth;
                        self.pointer = context.builder.nodes.link(self.pointer)?;
                    }
                } else if let NodePayload::Io(effect) = &context.builder.node(self.pointer)?.payload
                {
                    self.phase = Phase::Io;
                    return Ok(ShipmentStep::Effect(PlaceAction::Io(effect.clone())));
                } else if matches!(node.whatsit.as_str(), "special" | "pdf")
                    || node.leader_ptr != C::NULL
                {
                    self.placement = Some(PlaceLineExecution::new(
                        layout,
                        self.pointer,
                        Some(self.x),
                        Some(self.y),
                        true,
                        Some(root.clone()),
                        Some(self.origin),
                        Some(effective),
                    ));
                    self.advance = effective;
                    self.adjustment = true;
                    self.phase = Phase::Place;
                } else {
                    if matches!(node.node_type, NodeKind::Glue | NodeKind::Kern) {
                        self.y += effective;
                    }
                    self.pointer = context.builder.nodes.link(self.pointer)?;
                }
            }
            Phase::Place => {
                let mut adapter = Some(services as &mut dyn ShipoutServices);
                match self
                    .placement
                    .as_mut()
                    .expect("shipment placement")
                    .resume(layout, context, &mut adapter, returned)?
                {
                    PlaceStep::Continue => {}
                    PlaceStep::Yield(action) => return Ok(ShipmentStep::Effect(action)),
                    PlaceStep::Complete(line) => {
                        if matches!(self.kind, ShipmentKind::Built) {
                            self.y += self.advance
                                + if self.adjustment {
                                    line.position_adjustment_sp
                                } else {
                                    0
                                };
                            self.pointer = context.builder.nodes.link(self.pointer)?;
                            self.phase = Phase::Walk;
                        } else {
                            self.phase = Phase::Finish;
                        }
                        self.lines.push(line);
                        self.placement = None;
                    }
                }
            }
            Phase::Io => {
                returned.ok_or_else(|| Error::runtime("Missing delayed IO result"))??;
                self.pointer = context.builder.nodes.link(self.pointer)?;
                self.phase = Phase::Walk;
            }
            Phase::Finish => {
                if returned.is_some() {
                    return Err(Error::runtime("Unexpected page completion result"));
                }
                layout.pages.push(PageDisplayList {
                    page: layout.pages.len() + 1,
                    width_sp: self.width,
                    height_sp: self.height,
                    lines: std::mem::take(&mut self.lines),
                    magnification: self.magnification,
                });
                self.phase = Phase::FinishedPage;
                return Ok(ShipmentStep::FinishPage(self.page_use));
            }
            Phase::FinishedPage => {
                returned.ok_or_else(|| Error::runtime("Missing page finalization result"))??;
                if matches!(self.kind, ShipmentKind::Explicit) && layout.tracing {
                    layout.events.push(super::layout::LayoutEvent::Shipout {
                        page: layout.pages.len(),
                        width: self.width,
                        height: self.height,
                        x: self.x,
                        y: self.y,
                    });
                }
                self.phase = Phase::Done;
                return Ok(ShipmentStep::Complete);
            }
            Phase::Done => return Err(Error::runtime("Shipment already completed")),
        }
        Ok(ShipmentStep::Continue)
    }
}
