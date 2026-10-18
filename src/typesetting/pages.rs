//! Online page building with retained contribution and insertion ownership (`pages.py`).
use super::layout::{Layout, TypesettingContext};
use super::model::{Node, NodePayload};
use super::nodes::{glue_parameter, integer, parameter};
use super::shipout::ShipoutServices;
use super::vertical::{AWFUL, DiscardKind, Vertical, discardable, glue_break_predecessor, is_box};
use crate::core::constants::Constants as C;
use crate::core::enums::{CharacterCategory, NodeKind};
use crate::core::errors::{Error, Result};
use crate::core::values::{GlueSpec, MAX_DIMEN, trunc_div};
use crate::language::character_tokens::{Token, character_token};
use crate::language::records::{Quantity, QuantityKind, ValueKey};
use crate::typesetting::model::NodeFields;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

pub const EJECT_PENALTY: i64 = -(1 << 30);
pub const PAGE_DIMENSIONS: [&str; 8] = [
    "pagegoal",
    "pagetotal",
    "pagestretch",
    "pagefilstretch",
    "pagefillstretch",
    "pagefilllstretch",
    "pageshrink",
    "pagedepth",
];
#[derive(Clone, Copy, Debug)]
pub struct InsertionState {
    pub size: i64,
    pub last: i64,
    pub split: Option<(i64, usize)>,
}
#[derive(Clone, Copy, Debug, Default)]
pub enum OutputHandler {
    #[default]
    Direct,
    Routine,
    External,
}
#[derive(Clone, Debug)]
pub enum PageEvent {
    Shipout {
        page: usize,
        width: i64,
        height: i64,
    },
    BreakCandidate {
        node: i64,
        total: i64,
        goal: i64,
        depth: i64,
        penalty: i64,
        badness: i64,
        cost: i64,
    },
    InsertionSplit {
        register: i64,
        node: i64,
        goal: i64,
        kept: usize,
        size: i64,
    },
    OutputBegin {
        dead_cycles: i64,
        box_pointer: i64,
    },
    OutputEnd {
        dead_cycles: i64,
    },
}
/// The interpreter drives a braced output routine as an internal vbox. Passing
/// the live page builder permits explicit shipment and reentrant contributions.
pub trait PageServices {
    fn recover(
        &mut self,
        _issue: crate::core::recovery::Issue,
        error: Error,
        _action: &str,
    ) -> Result<()> {
        Err(error)
    }
    fn shipout(&mut self) -> &mut dyn ShipoutServices;
    fn read_output_box(
        &mut self,
        pages: &mut PageBuilder,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        tokens: Arc<[Token]>,
    ) -> Result<i64>;
    fn handle_page(
        &mut self,
        pages: &mut PageBuilder,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        box_pointer: i64,
    ) -> Result<()>;
}
#[derive(Clone)]
pub struct PageBuilder {
    pub queue: VecDeque<i64>,
    pub page: Vec<i64>,
    pub active: bool,
    pub output_dimensions: Option<[i64; 8]>,
    pub output_handler: OutputHandler,
    pub dead_cycles: i64,
    pub insert_penalties: i64,
    pub last_glue: Option<GlueSpec>,
    pub last_penalty: i64,
    pub last_kern: i64,
    pub last_node_kind: Option<NodeKind>,
    pub goal: Option<i64>,
    pub maximum: i64,
    pub height: i64,
    pub depth: i64,
    pub shrink: i64,
    pub stretch: [i64; 4],
    pub has_box: bool,
    pub best: usize,
    pub best_goal: i64,
    pub least: i64,
    pub insertions: BTreeMap<i64, InsertionState>,
    pub best_insertions: BTreeMap<i64, InsertionState>,
    pub events: Vec<PageEvent>,
}
impl Default for PageBuilder {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            page: Vec::new(),
            active: false,
            output_dimensions: None,
            output_handler: OutputHandler::Direct,
            dead_cycles: 0,
            insert_penalties: 0,
            last_glue: None,
            last_penalty: 0,
            last_kern: 0,
            last_node_kind: None,
            goal: None,
            maximum: 0,
            height: 0,
            depth: 0,
            shrink: 0,
            stretch: [0; 4],
            has_box: false,
            best: 0,
            best_goal: 0,
            least: AWFUL,
            insertions: BTreeMap::new(),
            best_insertions: BTreeMap::new(),
            events: Vec::new(),
        }
    }
}
impl PageBuilder {
    pub fn roots(&self) -> Vec<i64> {
        let mut roots = Vec::new();
        self.append_roots(&mut roots);
        roots
    }
    pub(crate) fn append_roots(&self, roots: &mut Vec<i64>) {
        roots.extend(self.page.iter().chain(&self.queue).copied());
    }
    pub fn reset(&mut self) {
        self.last_glue = None;
        self.last_penalty = 0;
        self.last_kern = 0;
        self.last_node_kind = None;
        self.goal = None;
        self.maximum = 0;
        self.height = 0;
        self.depth = 0;
        self.shrink = 0;
        self.stretch = [0; 4];
        self.has_box = false;
        self.best = 0;
        self.best_goal = 0;
        self.least = AWFUL;
        self.insertions.clear();
        self.best_insertions.clear();
        self.insert_penalties = 0;
    }
    pub fn dimension(&self, code: usize) -> Result<i64> {
        if code >= 8 {
            return Err(Error::value("Invalid page dimension")
                .with_issue(crate::core::recovery::Issue::PageGeometry));
        }
        if self.active {
            if let Some(dimensions) = self.output_dimensions {
                return Ok(dimensions[code]);
            }
        }
        let Some(goal) = self.goal else {
            return Ok(if code == 0 { MAX_DIMEN } else { 0 });
        };
        Ok([
            goal,
            self.height,
            self.stretch[0],
            self.stretch[1],
            self.stretch[2],
            self.stretch[3],
            self.shrink,
            self.depth,
        ][code])
    }
    pub fn set_dimension(&mut self, code: usize, value: i64) -> Result<()> {
        if code >= 8 {
            return Err(Error::value("Invalid page dimension")
                .with_issue(crate::core::recovery::Issue::PageGeometry));
        }
        if self.active {
            if let Some(dimensions) = &mut self.output_dimensions {
                dimensions[code] = value;
                return Ok(());
            }
        }
        match code {
            0 => self.goal = Some(value),
            1 => self.height = value,
            2..=5 => self.stretch[code - 2] = value,
            6 => self.shrink = value,
            _ => self.depth = value,
        }
        Ok(())
    }
    pub fn contribute(
        &mut self,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
        head: i64,
    ) -> Result<()> {
        for pointer in context.builder.siblings(head)? {
            context.builder.node_mut(pointer)?.link = C::NULL;
            self.queue.push_back(pointer);
        }
        self.build(vertical, layout, context, services)
    }
    pub fn build(
        &mut self,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
    ) -> Result<()> {
        PageExecution::new(false).drive(self, vertical, layout, context, services)
    }
    fn build_step(
        &mut self,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
    ) -> Result<PageBuildStep> {
        if self.active {
            return Ok(PageBuildStep::Complete);
        }
        let Some(&pointer) = self.queue.front() else {
            return Ok(PageBuildStep::Complete);
        };
        let node = context.builder.nodes.shape(pointer)?;
        let kind = node.node_type;
        self.last_glue = if kind == NodeKind::Glue {
            node.glue
        } else {
            None
        };
        self.last_penalty = if kind == NodeKind::Penalty {
            node.penalty
        } else {
            0
        };
        self.last_kern = if kind == NodeKind::Kern {
            node.width
        } else {
            0
        };
        self.last_node_kind = Some(kind);
        if !self.has_box && discardable(kind) {
            self.queue.pop_front();
            vertical.save_discard(context, DiscardKind::Page, pointer)?;
            return Ok(PageBuildStep::Continue);
        }
        if is_box(kind) && !self.has_box {
            if self.goal.is_none() {
                self.goal = Some(parameter(context.env, "vsize", layout.vsize));
                self.maximum = parameter(context.env, "maxdepth", MAX_DIMEN);
            }
            self.has_box = true;
            let spec = glue_parameter(context.env, "topskip");
            let top = context.builder.new_glue(
                GlueSpec {
                    width: (spec.width - node.height).max(0),
                    ..spec
                },
                "topskip",
                context.fonts.current,
            )?;
            self.queue.push_front(top);
            return Ok(PageBuildStep::Continue);
        }
        if kind == NodeKind::Kern && self.queue.len() == 1 {
            return Ok(PageBuildStep::Complete);
        }
        let penalty = if kind == NodeKind::Penalty {
            Some(node.penalty)
        } else if kind == NodeKind::Glue
            && self
                .page
                .last()
                .map(|&pointer| {
                    context
                        .builder
                        .nodes
                        .shape(pointer)
                        .map(|node| glue_break_predecessor(node.node_type))
                })
                .transpose()?
                .unwrap_or(false)
        {
            Some(0)
        } else if kind == NodeKind::Kern
            && context.builder.nodes.shape(self.queue[1])?.node_type == NodeKind::Glue
        {
            Some(0)
        } else {
            None
        };
        if let Some(penalty) = penalty.filter(|&penalty| penalty < 10000) {
            let goal = self
                .goal
                .ok_or_else(|| Error::value("Page breakpoint without a page goal"))?;
            let (badness, cost) = Vertical::cost(
                self.height,
                goal,
                self.stretch,
                self.shrink,
                penalty,
                self.insert_penalties,
            );
            self.events.push(PageEvent::BreakCandidate {
                node: pointer,
                total: self.height,
                goal,
                depth: self.depth,
                penalty,
                badness,
                cost,
            });
            if cost <= self.least {
                self.best = self.page.len();
                self.least = cost;
                self.best_goal = goal;
                self.best_insertions = self.insertions.clone();
            }
            if cost == AWFUL || penalty <= -10000 {
                return self.fire_step(vertical, layout, context, services);
            }
        }
        if is_box(kind) || node.sized_whatsit() {
            self.height += self.depth + node.height;
            self.depth = node.depth;
        } else if matches!(kind, NodeKind::Glue | NodeKind::Kern) {
            self.height += self.depth + node.width;
            self.depth = 0;
            if kind == NodeKind::Glue {
                let spec = node
                    .glue
                    .ok_or_else(|| Error::value("Glue without specification"))?;
                if spec.shrink != 0 && spec.shrink_order != 0 {
                    return Err(
                        Error::value("Infinite glue shrinkage found on current page")
                            .with_issue(crate::core::recovery::Issue::GlueShrinkage),
                    );
                }
                self.stretch[spec.stretch_order as usize] += spec.stretch;
                self.shrink += spec.shrink;
            }
        } else if kind == NodeKind::Insert {
            self.process_insertion(layout, context, pointer)?;
        } else if !matches!(kind, NodeKind::Mark | NodeKind::Whatsit | NodeKind::Penalty) {
            return Err(Error::value("Nonvertical material in page builder"));
        }
        if self.depth > self.maximum {
            self.height += self.depth - self.maximum;
            self.depth = self.maximum;
        }
        self.page.push(self.queue.pop_front().unwrap());
        Ok(PageBuildStep::Continue)
    }
    pub fn fire(
        &mut self,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
    ) -> Result<()> {
        match self.fire_step(vertical, layout, context, services)? {
            PageBuildStep::Output(tokens) => {
                let result = services.read_output_box(self, vertical, layout, context, tokens);
                self.finish_output(vertical, context, result)
            }
            PageBuildStep::Ship { pointer, built } => {
                if built {
                    self.ship(layout, context, services.shipout(), pointer)
                } else {
                    services.handle_page(self, vertical, layout, context, pointer)
                }
            }
            PageBuildStep::Continue | PageBuildStep::Complete => Ok(()),
        }
    }
    fn fire_step(
        &mut self,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
    ) -> Result<PageBuildStep> {
        let rest = self.page.split_off(self.best);
        let mut queue: VecDeque<_> = rest.into();
        queue.append(&mut self.queue);
        self.queue = queue;
        let first = self.page.clone();
        let mut penalty = 10000;
        if let Some(&pointer) = self.queue.front() {
            let node = context.builder.node_mut(pointer)?;
            if node.node_type == NodeKind::Penalty {
                penalty = node.penalty;
                node.penalty = 10000;
            }
        }
        context.env.assign_value(
            ValueKey::parameter("outputpenalty"),
            Quantity::Integer(penalty),
            true,
        );
        self.page_marks(vertical, context, &first)?;
        let (first, held) = self.extract_insertions(vertical, layout, context, &first)?;
        self.page = first.iter().chain(&held).copied().collect();
        let key = ValueKey::parameter("vbadness");
        let saved = context
            .env
            .values
            .insert(key.clone(), Quantity::Integer(10000));
        let packed = (|| {
            let head = Vertical::link(context.builder, &first)?;
            layout.vpack(
                context,
                head,
                false,
                Some(self.best_goal),
                false,
                self.maximum,
            )
        })();
        if let Some(saved) = saved {
            context.env.values.insert(key, saved);
        } else {
            context.env.values.remove(&key);
        }
        let box_pointer = packed?;
        self.output_dimensions = Some([
            self.goal.unwrap_or(0),
            self.height,
            self.stretch[0],
            self.stretch[1],
            self.stretch[2],
            self.stretch[3],
            self.shrink,
            0,
        ]);
        self.page.clear();
        self.reset();
        self.insert_penalties = held.len() as i64;
        for pointer in held.into_iter().rev() {
            self.queue.push_front(pointer);
        }
        match self.output_handler {
            OutputHandler::Direct => Ok(PageBuildStep::Ship {
                pointer: box_pointer,
                built: true,
            }),
            OutputHandler::Routine => {
                let empty = !matches!(context.env.values.get(&ValueKey::parameter("output")),
                    Some(Quantity::Tokens(tokens)) if !tokens.is_empty());
                if empty {
                    vertical.clear_discards(context.builder, DiscardKind::Page)?;
                    return Ok(PageBuildStep::Ship {
                        pointer: box_pointer,
                        built: true,
                    });
                }
                match self.begin_output(vertical, layout, context, services, box_pointer)? {
                    Some(tokens) => Ok(PageBuildStep::Output(tokens)),
                    None => Ok(PageBuildStep::Continue),
                }
            }
            OutputHandler::External => Ok(PageBuildStep::Ship {
                pointer: box_pointer,
                built: false,
            }),
        }
    }
    pub fn page_marks(
        &mut self,
        vertical: &mut Vertical,
        context: &TypesettingContext<'_>,
        pointers: &[i64],
    ) -> Result<()> {
        let mut classes: BTreeSet<_> = vertical
            .marks
            .keys()
            .filter(|key| key.1 < 3)
            .map(|key| key.0)
            .collect();
        let mut found: BTreeMap<i64, Vec<Arc<[Token]>>> = BTreeMap::new();
        for &pointer in pointers {
            let node = context.builder.nodes.shape(pointer)?;
            if node.node_type == NodeKind::Mark {
                let NodePayload::Mark { number, tokens } = &context.builder.node(pointer)?.payload
                else {
                    return Err(Error::value("Invalid mark payload"));
                };
                classes.insert(*number);
                found.entry(*number).or_default().push(tokens.clone());
            }
        }
        for number in classes {
            let top = vertical.mark_text(number, 2);
            vertical.marks.insert((number, 0), top.clone());
            let marks = found.remove(&number).unwrap_or_default();
            vertical.marks.insert(
                (number, 1),
                marks.first().cloned().unwrap_or_else(|| top.clone()),
            );
            vertical
                .marks
                .insert((number, 2), marks.last().cloned().unwrap_or(top));
        }
        Ok(())
    }
    pub fn process_insertion(
        &mut self,
        layout: &Layout,
        context: &mut TypesettingContext<'_>,
        pointer: i64,
    ) -> Result<()> {
        let node = context.builder.node(pointer)?.clone();
        let NodePayload::Insertion {
            number,
            floating_penalty,
            ..
        } = node.payload
        else {
            return Err(Error::value("Invalid insertion payload"));
        };
        if self.goal.is_none() {
            self.goal = Some(parameter(context.env, "vsize", layout.vsize));
            self.maximum = parameter(context.env, "maxdepth", MAX_DIMEN);
        }
        let scale = integer(
            context.env,
            &ValueKey::register(QuantityKind::Count, number),
            0,
        );
        let limit = integer(
            context.env,
            &ValueKey::register(QuantityKind::Dimen, number),
            0,
        );
        let scaled = |value: i64| -> Result<i64> {
            Ok(if scale == 1000 {
                value
            } else {
                trunc_div(value as i128, 1000)? as i64 * scale
            })
        };
        if !self.insertions.contains_key(&number) {
            let owner = context
                .env
                .box_registers
                .get(&number)
                .copied()
                .unwrap_or(C::NULL);
            let size = if owner == C::NULL {
                0
            } else {
                let node = context.builder.node(owner)?;
                if node.node_type != NodeKind::Vlist {
                    return Err(Error::value("Insertion register needs a vbox"));
                }
                node.height + node.depth
            };
            let spec = match context
                .env
                .values
                .get(&ValueKey::register(QuantityKind::Skip, number))
            {
                Some(Quantity::Glue(spec)) => *spec,
                _ => GlueSpec::default(),
            };
            if spec.shrink != 0 && spec.shrink_order != 0 {
                return Err(
                    Error::value("Infinite glue shrinkage in insertion correction")
                        .with_issue(crate::core::recovery::Issue::GlueShrinkage),
                );
            }
            *self.goal.as_mut().unwrap() -= scaled(size)? + spec.width;
            self.stretch[spec.stretch_order as usize] += spec.stretch;
            self.shrink += spec.shrink;
            self.insertions.insert(
                number,
                InsertionState {
                    size,
                    last: C::NULL,
                    split: None,
                },
            );
        }
        let state = self.insertions.get_mut(&number).unwrap();
        if state.split.is_some() {
            self.insert_penalties += floating_penalty;
            return Ok(());
        }
        state.last = pointer;
        let required = scaled(node.height)?;
        let available = self.goal.unwrap() - self.height - self.depth + self.shrink;
        if (required <= 0 || required <= available) && node.height + state.size <= limit {
            *self.goal.as_mut().unwrap() -= required;
            state.size += node.height;
            return Ok(());
        }
        let mut amount = if scale <= 0 {
            MAX_DIMEN
        } else {
            self.goal.unwrap() - self.height - self.depth
        };
        if scale > 0 && scale != 1000 {
            amount = trunc_div(amount as i128, scale as i128)? as i64 * 1000;
        }
        amount = amount.min(limit - state.size);
        let pointers = context.builder.siblings(node.list_ptr)?;
        let (index, size) = Vertical::break_list(context.builder, &pointers, amount, node.depth)?;
        state.size += size;
        *self.goal.as_mut().unwrap() -= scaled(size)?;
        state.split = Some((pointer, index));
        self.insert_penalties += if index == pointers.len() {
            -10000
        } else {
            let node = context.builder.node(pointers[index])?;
            if node.node_type == NodeKind::Penalty {
                node.penalty
            } else {
                0
            }
        };
        self.events.push(PageEvent::InsertionSplit {
            register: number,
            node: pointer,
            goal: amount,
            kept: index,
            size,
        });
        Ok(())
    }
    pub fn extract_insertions(
        &mut self,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        pointers: &[i64],
    ) -> Result<(Vec<i64>, Vec<i64>)> {
        if parameter(context.env, "holdinginserts", 0) > 0 {
            return Ok((pointers.to_vec(), Vec::new()));
        }
        let mut kept = Vec::new();
        let mut held = Vec::new();
        let mut material: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        let transfer = context.builder.ownership.hold([]);
        for (&number, state) in &self.best_insertions {
            if state.last == C::NULL {
                continue;
            }
            let owner = context.env.take_box(number, false, None)?;
            let contents = if owner == C::NULL {
                Vec::new()
            } else {
                let contents = context
                    .builder
                    .siblings(context.builder.node(owner)?.list_ptr)?;
                context.builder.node_mut(owner)?.list_ptr = C::NULL;
                context.builder.release(owner, false)?;
                contents
            };
            transfer.roots.borrow_mut().extend(contents.iter().copied());
            material.insert(number, contents);
        }
        for &pointer in pointers {
            let node = context.builder.nodes.shape(pointer)?;
            if node.node_type != NodeKind::Insert {
                kept.push(pointer);
                continue;
            }
            let NodePayload::Insertion {
                number,
                split_top_skip,
                ..
            } = context.builder.node(pointer)?.payload.clone()
            else {
                return Err(Error::value("Invalid insertion payload"));
            };
            let Some(state) = self
                .best_insertions
                .get_mut(&number)
                .filter(|state| state.last != C::NULL && material.contains_key(&number))
            else {
                held.push(pointer);
                continue;
            };
            let mut contents = context
                .builder
                .siblings(context.builder.node(pointer)?.list_ptr)?;
            let mut rest = Vec::new();
            if let Some((split_pointer, index)) = state.split {
                if split_pointer == pointer {
                    let remainder = contents.split_off(index);
                    rest = vertical.prune(context, &remainder, split_top_skip, false)?;
                }
            }
            material.get_mut(&number).unwrap().extend(contents);
            let rest_head = Vertical::link(context.builder, &rest)?;
            context.builder.node_mut(pointer)?.list_ptr = rest_head;
            if pointer == state.last {
                state.last = C::NULL;
                let head = Vertical::link(context.builder, &material[&number])?;
                let packed = layout.vpack(context, head, false, None, false, MAX_DIMEN)?;
                context.env.box_registers.insert(number, packed);
            }
            if rest.is_empty() {
                context.builder.node_mut(pointer)?.link = C::NULL;
                context.builder.release(pointer, false)?;
            } else {
                let owner = layout.vpack(context, rest_head, false, None, false, MAX_DIMEN)?;
                let original = context.builder.node(owner)?;
                let height = original.height + original.depth;
                context.builder.node_mut(pointer)?.height = height;
                context.builder.node_mut(owner)?.list_ptr = C::NULL;
                context.builder.release(owner, false)?;
                held.push(pointer);
            }
        }
        drop(transfer);
        Ok((kept, held))
    }
    pub fn output(
        &mut self,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
        box_pointer: i64,
    ) -> Result<()> {
        if let Some(tokens) = self.begin_output(vertical, layout, context, services, box_pointer)? {
            let result = services.read_output_box(self, vertical, layout, context, tokens);
            self.finish_output(vertical, context, result)?;
        }
        Ok(())
    }
    fn begin_output(
        &mut self,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
        box_pointer: i64,
    ) -> Result<Option<Arc<[Token]>>> {
        #[cfg(feature = "latency-diagnostics")]
        crate::local_update_audit::consumer("output_program");
        let routine = match context.env.values.get(&ValueKey::parameter("output")) {
            Some(Quantity::Tokens(tokens)) => tokens.clone(),
            _ => Arc::from([]),
        };
        if routine.is_empty() {
            vertical.clear_discards(context.builder, DiscardKind::Page)?;
            self.ship(layout, context, services.shipout(), box_pointer)?;
            return Ok(None);
        }
        if self.dead_cycles >= parameter(context.env, "maxdeadcycles", 25) {
            let error = Error::value(format!(
                "Output loop: {} consecutive dead cycles",
                self.dead_cycles
            ));
            if let Err(error) = services.recover(
                crate::core::recovery::Issue::OutputRoutine,
                error,
                "Used the default page shipment after the output routine looped",
            ) {
                context.builder.release(box_pointer, false)?;
                return Err(error);
            }
            self.dead_cycles = 0;
            vertical.clear_discards(context.builder, DiscardKind::Page)?;
            self.ship(layout, context, services.shipout(), box_pointer)?;
            return Ok(None);
        }
        if context
            .env
            .box_registers
            .get(&C::OUTPUT_BOX_REGISTER)
            .is_some_and(|&pointer| pointer != C::NULL)
        {
            context.builder.release(box_pointer, false)?;
            return Err(Error::value("Box 255 is not void before output")
                .with_issue(crate::core::recovery::Issue::OutputRoutine));
        }
        context
            .env
            .box_registers
            .insert(C::OUTPUT_BOX_REGISTER, box_pointer);
        self.dead_cycles += 1;
        self.active = true;
        self.events.push(PageEvent::OutputBegin {
            dead_cycles: self.dead_cycles,
            box_pointer,
        });
        let mut tokens = vec![character_token(CharacterCategory::LeftBrace, 123)?];
        tokens.extend_from_slice(&routine);
        tokens.push(character_token(CharacterCategory::RightBrace, 125)?);
        Ok(Some(tokens.into()))
    }
    fn finish_output(
        &mut self,
        vertical: &mut Vertical,
        context: &mut TypesettingContext<'_>,
        generated: Result<i64>,
    ) -> Result<()> {
        let result = (|| -> Result<()> {
            let generated = generated?;
            let contribution = context
                .builder
                .siblings(context.builder.node(generated)?.list_ptr)?;
            let hold = context.builder.ownership.hold(contribution.iter().copied());
            context.builder.node_mut(generated)?.list_ptr = C::NULL;
            context.builder.release(generated, false)?;
            if context
                .env
                .box_registers
                .get(&C::OUTPUT_BOX_REGISTER)
                .is_some_and(|&pointer| pointer != C::NULL)
            {
                return Err(Error::value("Output routine did not consume box 255")
                    .with_issue(crate::core::recovery::Issue::OutputRoutine));
            }
            for &pointer in &contribution {
                context.builder.node_mut(pointer)?.link = C::NULL;
            }
            let mut held = Vec::new();
            for _ in 0..self.insert_penalties.max(0) {
                held.push(
                    self.queue
                        .pop_front()
                        .ok_or_else(|| Error::value("Missing held insertion"))?,
                );
            }
            held.extend(contribution);
            for pointer in held.into_iter().rev() {
                self.queue.push_front(pointer);
            }
            drop(hold);
            Ok(())
        })();
        if result.is_err() {
            let remaining = context.env.take_box(C::OUTPUT_BOX_REGISTER, false, None)?;
            if remaining != C::NULL {
                context.builder.release(remaining, false)?;
            }
        }
        let cleanup = vertical.clear_discards(context.builder, DiscardKind::Page);
        self.active = false;
        self.output_dimensions = None;
        self.insert_penalties = 0;
        self.events.push(PageEvent::OutputEnd {
            dead_cycles: self.dead_cycles,
        });
        cleanup?;
        result
    }
    pub fn ship(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn ShipoutServices,
        pointer: i64,
    ) -> Result<()> {
        let hold = context.builder.ownership.hold([pointer]);
        let result = self.ship_owned(layout, context, services, pointer);
        let cleanup = context.builder.release(pointer, false);
        drop(hold);
        cleanup?;
        result
    }
    fn ship_owned(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn ShipoutServices,
        pointer: i64,
    ) -> Result<()> {
        super::shipment_execution::ShipmentExecution::new(
            pointer,
            super::shipment_execution::ShipmentKind::Built,
        )
        .drive(layout, context, services)?;
        self.events.push(PageEvent::Shipout {
            page: layout.pages.len(),
            width: layout.pages.last().unwrap().width_sp,
            height: layout.pages.last().unwrap().height_sp,
        });
        self.dead_cycles = 0;
        Ok(())
    }
    pub fn finish(
        &mut self,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
    ) -> Result<()> {
        PageExecution::new(true).drive(self, vertical, layout, context, services)
    }
    pub fn discard(
        &mut self,
        vertical: &mut Vertical,
        context: &mut TypesettingContext<'_>,
    ) -> Result<()> {
        let roots = self.roots();
        self.page.clear();
        self.queue.clear();
        self.reset();
        for pointer in roots {
            context.builder.node_mut(pointer)?.link = C::NULL;
            context.builder.release(pointer, false)?;
        }
        vertical.pending_migrations.clear();
        vertical.clear_discards(context.builder, DiscardKind::Page)?;
        vertical.clear_discards(context.builder, DiscardKind::Split)
    }
}

/// A page builder boundary retains all queued material in the live PageBuilder.
/// Output tokens must execute before the next queue item is processed.
enum PageBuildStep {
    Ship { pointer: i64, built: bool },
    Complete,
    Continue,
    Output(Arc<[Token]>),
}
pub(crate) enum PageStep {
    Ship { pointer: i64, built: bool },
    Complete,
    Continue,
    Output(Arc<[Token]>),
}
#[derive(Clone)]
pub(crate) struct PageExecution {
    finish: bool,
    awaiting_output: bool,
    awaiting_shipment: bool,
    done: bool,
}
impl PageExecution {
    pub fn new(finish: bool) -> Self {
        Self {
            finish,
            awaiting_output: false,
            awaiting_shipment: false,
            done: false,
        }
    }
    pub fn resume(
        &mut self,
        pages: &mut PageBuilder,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
        returned: Option<Result<i64>>,
    ) -> Result<PageStep> {
        if self.done {
            return Err(Error::value("Page execution already completed"));
        }
        if self.awaiting_shipment {
            self.awaiting_shipment = false;
            returned.ok_or_else(|| Error::runtime("Missing page shipment result"))??;
            return Ok(PageStep::Continue);
        }
        if self.awaiting_output {
            let result = returned.ok_or_else(|| Error::value("Missing output routine result"))?;
            self.awaiting_output = false;
            pages.finish_output(vertical, context, result)?;
            return Ok(PageStep::Continue);
        }
        if returned.is_some() {
            return Err(Error::value("Unexpected output routine result"));
        }
        match pages.build_step(vertical, layout, context, services)? {
            PageBuildStep::Ship { pointer, built } => {
                self.awaiting_shipment = true;
                Ok(PageStep::Ship { pointer, built })
            }
            PageBuildStep::Continue => Ok(PageStep::Continue),
            PageBuildStep::Output(tokens) => {
                self.awaiting_output = true;
                Ok(PageStep::Output(tokens))
            }
            PageBuildStep::Complete => {
                if !self.finish {
                    self.done = true;
                    return Ok(PageStep::Complete);
                }
                if pages.page.is_empty() && pages.queue.is_empty() && pages.dead_cycles == 0 {
                    pages.discard(vertical, context)?;
                    self.done = true;
                    return Ok(PageStep::Complete);
                }
                let pointer = context.builder.new_null_box()?;
                let node = context.builder.node_mut(pointer)?;
                node.width = parameter(context.env, "hsize", layout.hsize);
                node.whatsit = "end".into();
                pages.queue.push_back(pointer);
                pages.queue.push_back(context.builder.new_glue(
                    GlueSpec {
                        stretch: C::UNITY,
                        stretch_order: 2,
                        ..GlueSpec::default()
                    },
                    "normal",
                    context.fonts.current,
                )?);
                pages
                    .queue
                    .push_back(context.builder.allocate(Node::from(NodeFields {
                        node_type: NodeKind::Penalty,
                        penalty: EJECT_PENALTY,
                        ..NodeFields::default()
                    }))?);
                Ok(PageStep::Continue)
            }
        }
    }
    fn drive(
        mut self,
        pages: &mut PageBuilder,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
    ) -> Result<()> {
        let mut returned = None;
        loop {
            match self.resume(pages, vertical, layout, context, services, returned.take())? {
                PageStep::Ship { pointer, built } => {
                    let result = if built {
                        pages.ship(layout, context, services.shipout(), pointer)
                    } else {
                        services.handle_page(pages, vertical, layout, context, pointer)
                    };
                    returned = Some(result.map(|_| C::NULL));
                }
                PageStep::Complete => return Ok(()),
                PageStep::Continue => {}
                PageStep::Output(tokens) => {
                    returned =
                        Some(services.read_output_box(pages, vertical, layout, context, tokens))
                }
            }
        }
    }
}
