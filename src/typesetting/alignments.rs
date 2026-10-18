//! Alignment templates, live entry boundaries and spanning-column propagation.
//! Translated from `typesetting/alignments.py`.
use super::box_commands::{BoxCommands, BoxServices};
use super::model::BoxScanContext;
use super::paragraph_operations::ParagraphOperations;
use super::vertical::Vertical;
use crate::core::constants::Constants as C;
use crate::core::enums::{
    AlignmentBoundary, BoxOperation, CharacterCategory as Cat, Command, GroupKind, ListMode,
    NodeKind,
};
use crate::core::errors::{Error, Result};
use crate::core::values::{GlueSpec, MAX_DIMEN};
use crate::language::character_tokens::Token;
use crate::language::input::{InputProcessor, ScannerStatus};
use crate::language::records::{CommandOperand, MeaningCommand, Quantity, ValueKey};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

mod execution;
pub(crate) use execution::{AlignmentAction, AlignmentExecution, AlignmentStep, AlignmentValue};
use execution::{EntryExecution, EntryStep, PreambleExecution, PreambleStep, drive_action};

#[derive(Clone, Debug)]
pub struct Template {
    pub prefix: Arc<[Token]>,
    pub suffix: Arc<[Token]>,
}
#[derive(Clone, Debug)]
pub struct AlignmentList {
    pub previous_depth: Option<i64>,
    pub rows: BTreeSet<i64>,
}
#[derive(Clone, Debug)]
pub struct AlignmentRow {
    pub entries: Vec<(usize, usize, i64)>,
    pub baseline: Option<GlueSpec>,
}
enum Row {
    Material(i64),
    Entries(AlignmentRow),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryPhase {
    Prefix,
    Body,
    Suffix,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryHandler {
    pub phase: EntryPhase,
    pub baseline: i64,
    pub boundary: Option<MeaningCommand>,
    pub omit: bool,
    pub suffix: Arc<[Token]>,
    pub sentinel: Token,
}
impl EntryHandler {
    pub fn intercept(&mut self, input: &mut InputProcessor) -> Result<bool> {
        if input.current.is_command(Command::AlignSentinel)
            && matches!(
                input.current.value,
                CommandOperand::Alignment(AlignmentBoundary::PrefixEnd)
            )
        {
            self.phase = EntryPhase::Body;
            self.baseline = input.align_state;
            return Ok(true);
        }
        if self.phase == EntryPhase::Body
            && input.align_state == self.baseline
            && (input.current.is_category(Cat::TabMark)
                || input.current.is_command(Command::AlignEnd)
                || input.current.is_command(Command::AlignSpan))
        {
            self.boundary = Some(input.current.command);
            self.phase = EntryPhase::Suffix;
            input.begin_tokens(vec![self.sentinel].into(), None, None)?;
            if !self.omit {
                input.begin_tokens(self.suffix.clone(), None, None)?
            }
            return Ok(true);
        }
        Ok(false)
    }
}
pub trait AlignmentServices: BoxServices {
    fn expanded_protected(&mut self) -> Result<()>;
    fn expand_current(&mut self) -> Result<bool>;
    fn extract_adjustments(&mut self, pointer: i64) -> Result<(Vec<i64>, Vec<i64>)>;
    fn splice(&mut self, pointers: &[i64]) -> Result<()>;
    fn consume_outer(&mut self) -> Result<()>;
}
#[derive(Clone, Default)]
pub struct Alignments {
    pub held: Vec<i64>,
    pub depth: usize,
    pub adjustments: BTreeMap<i64, (Vec<i64>, Vec<i64>)>,
    pub results: BTreeMap<i64, AlignmentList>,
    markers: BTreeMap<AlignmentBoundary, Token>,
    period: Vec<Template>,
    period_skips: Vec<GlueSpec>,
}
impl Alignments {
    pub fn roots(&self, services: &impl AlignmentServices) -> Vec<i64> {
        self.held
            .iter()
            .copied()
            .filter(|pointer| services.builder().nodes.contains_key(pointer))
            .collect()
    }
    pub fn token(
        &mut self,
        services: &mut impl AlignmentServices,
        boundary: AlignmentBoundary,
    ) -> Token {
        if let Some(&token) = self.markers.get(&boundary) {
            return token;
        }
        let symbol = services.env_mut().reserve_symbol();
        services.env_mut().define(
            symbol,
            MeaningCommand::Operation(Command::AlignSentinel),
            CommandOperand::Alignment(boundary),
            true,
        );
        let token = Token::control_sequence(symbol);
        self.markers.insert(boundary, token);
        token
    }
    fn tabskip(services: &impl AlignmentServices) -> GlueSpec {
        match services.env().values.get(&ValueKey::parameter("tabskip")) {
            Some(Quantity::Glue(spec)) => *spec,
            _ => GlueSpec::default(),
        }
    }
    pub fn preamble(
        &mut self,
        services: &mut impl AlignmentServices,
    ) -> Result<(Vec<Template>, Vec<GlueSpec>, Option<usize>)> {
        let mut task = PreambleExecution::new(services);
        let mut returned = None;
        loop {
            match task.resume(services, returned.take())? {
                PreambleStep::Complete(result) => return Ok(result),
                PreambleStep::Yield(action) => returned = Some(drive_action(services, action)),
            }
        }
    }
    pub fn peek(services: &mut impl AlignmentServices) -> Result<()> {
        loop {
            services.expanded_protected()?;
            if !services.input().current.is_category(Cat::Spacer) {
                return Ok(());
            }
        }
    }
    pub fn entry(
        &mut self,
        services: &mut impl AlignmentServices,
        templates: &mut Vec<Template>,
        skips: &mut Vec<GlueSpec>,
        repeat: Option<usize>,
        column: usize,
        vertical: bool,
        sentinel: Token,
    ) -> Result<(usize, usize, i64, MeaningCommand)> {
        let mut task = EntryExecution::new(column);
        let mut returned = None;
        loop {
            match task.resume(
                self,
                services,
                templates,
                skips,
                repeat,
                vertical,
                sentinel,
                returned.take(),
            )? {
                EntryStep::Complete(result) => return Ok(result),
                EntryStep::Yield(action) => returned = Some(drive_action(services, action)),
            }
        }
    }
    pub fn read(&mut self, services: &mut impl AlignmentServices, vertical: bool) -> Result<i64> {
        let mut task = AlignmentExecution::new(std::mem::take(self), vertical);
        let mut returned = None;
        let result = loop {
            match task.resume(services, returned.take()) {
                Ok(AlignmentStep::Complete(pointer)) => break Ok(pointer),
                Err(error) => break Err(error),
                Ok(AlignmentStep::Yield(action)) => returned = Some(drive_action(services, action)),
            }
        };
        *self = task.alignment;
        result
    }
    fn finish_rows(
        &mut self,
        services: &mut impl AlignmentServices,
        vertical: bool,
        target: Option<i64>,
        spread: bool,
        rows: Vec<Row>,
        mut widths: Vec<Option<i64>>,
        mut skips: Vec<GlueSpec>,
        preceding_depth: Option<i64>,
        held_guard: &crate::core::runtime::RootHold,
    ) -> Result<i64> {
        let mut constraints = BTreeMap::<(usize, usize), i64>::new();
        for row in &rows {
            if let Row::Entries(row) = row {
                for &(start, span, pointer) in &row.entries {
                    if span > 1 {
                        let node = services.builder().node(pointer)?;
                        let width = if vertical { node.height } else { node.width };
                        constraints
                            .entry((start, span))
                            .and_modify(|old| *old = (*old).max(width))
                            .or_insert(width);
                    }
                }
            }
        }
        for start in 0..widths.len() {
            if let Some(&incoming) = constraints.get(&(start, 1)) {
                widths[start] = Some(widths[start].unwrap_or(incoming).max(incoming))
            }
            if widths[start].is_none() {
                widths[start] = Some(0);
                skips[start + 1] = GlueSpec::default()
            }
            let pending = constraints
                .iter()
                .filter(|((column, span), _)| *column == start && *span > 1)
                .map(|(&(column, span), &length)| ((column, span), length))
                .collect::<Vec<_>>();
            for ((_, span), length) in pending {
                let residual = length - widths[start].unwrap() - skips[start + 1].width;
                constraints
                    .entry((start + 1, span - 1))
                    .and_modify(|old| *old = (*old).max(residual))
                    .or_insert(residual);
            }
        }
        let widths = widths.into_iter().map(Option::unwrap).collect::<Vec<_>>();
        let mut skeleton = vec![glue(services, skips[0], "normal")?];
        for (index, &width) in widths.iter().enumerate() {
            let pointer = services.builder_mut().new_null_box()?;
            let node = services.builder_mut().node_mut(pointer)?;
            node.node_type = if vertical {
                NodeKind::Vlist
            } else {
                NodeKind::Hlist
            };
            if vertical {
                node.height = width
            } else {
                node.width = width
            }
            skeleton.push(pointer);
            skeleton.push(glue(services, skips[index + 1], "normal")?);
        }
        let proto = services.with_layout(|layout, context| {
            let head = Vertical::link(context.builder, &skeleton)?;
            if vertical {
                layout.vpack(context, head, false, target, spread, MAX_DIMEN)
            } else {
                layout.hpack(context, head, target, spread, false)
            }
        })?;
        self.held.push(proto);
        held_guard.roots.borrow_mut().push(proto);
        let template_box = services.builder().node(proto)?.clone();
        let actual_skips = services.with_layout(|layout, context| {
            Ok(layout
                .effective_widths(context.builder, &template_box, false)?
                .into_iter()
                .filter(|(pointer, _)| {
                    context.builder.nodes[pointer]
                        .as_ref()
                        .is_some_and(|node| node.glue.is_some())
                })
                .map(|(_, width)| width)
                .collect::<Vec<_>>())
        })?;
        let mut row_boxes = BTreeSet::new();
        for row in rows {
            match row {
                Row::Material(pointer) => {
                    let head = services.builder().node(pointer)?.list_ptr;
                    services.builder_mut().node_mut(pointer)?.list_ptr = C::NULL;
                    services.builder_mut().release(pointer, false)?;
                    for pointer in services.builder().siblings(head)? {
                        services.builder_mut().node_mut(pointer)?.link = C::NULL;
                        services.builder_mut().tail_append(pointer)?;
                        let node = services.builder_mut().node_mut(pointer)?;
                        if node.node_type == NodeKind::Rule {
                            if node.running_width {
                                node.width = template_box.width;
                                node.running_width = false
                            }
                            if node.running_height {
                                node.height = template_box.height;
                                node.running_height = false
                            }
                            if node.running_depth {
                                node.depth = template_box.depth;
                                node.running_depth = false
                            }
                        }
                    }
                }
                Row::Entries(row) => {
                    let mut pointers = vec![glue(services, skips[0], "normal")?];
                    let (mut row_height, mut row_width, mut row_depth) = (0, 0, 0);
                    for &(_, _, pointer) in &row.entries {
                        let node = services.builder().node(pointer)?;
                        row_height = row_height.max(node.height);
                        row_width = row_width.max(node.width);
                        row_depth = row_depth.max(node.depth)
                    }
                    for &(start, span, pointer) in &row.entries {
                        let mut node = services.builder().node(pointer)?.clone();
                        let natural = if vertical { node.height } else { node.width };
                        let full = widths[start..start + span].iter().sum::<i64>()
                            + actual_skips[start + 1..start + span].iter().sum::<i64>();
                        services.with_layout(|layout, context| {
                            layout.set_glue(
                                context,
                                &mut node,
                                natural,
                                Some(full),
                                false,
                                vertical,
                            )
                        })?;
                        if vertical {
                            node.height = widths[start];
                            node.width = row_width
                        } else {
                            node.width = widths[start];
                            node.height = row_height;
                            node.depth = row_depth
                        }
                        *services.builder_mut().node_mut(pointer)? = node;
                        pointers.push(pointer);
                        for index in start + 1..start + span {
                            pointers.push(glue(services, skips[index], "normal")?);
                            let empty = services.builder_mut().new_null_box()?;
                            let node = services.builder_mut().node_mut(empty)?;
                            if vertical {
                                node.node_type = NodeKind::Vlist;
                                node.height = widths[index]
                            } else {
                                node.width = widths[index];
                                node.height = row_height;
                                node.depth = row_depth
                            }
                            pointers.push(empty)
                        }
                        pointers.push(glue(services, skips[start + span], "normal")?);
                    }
                    let packed = services.with_layout(|layout, context| {
                        let head = Vertical::link(context.builder, &pointers)?;
                        if vertical {
                            layout.vpack(
                                context,
                                head,
                                false,
                                Some(template_box.height),
                                false,
                                MAX_DIMEN,
                            )
                        } else {
                            layout.hpack(context, head, Some(template_box.width), false, false)
                        }
                    })?;
                    row_boxes.insert(packed);
                    held_guard.roots.borrow_mut().push(packed);
                    let (mut before, mut after) = (Vec::new(), Vec::new());
                    for &(_, _, pointer) in &row.entries {
                        let (pre, post) = self.adjustments.remove(&pointer).unwrap_or_default();
                        before.extend(pre);
                        after.extend(post)
                    }
                    services.splice(&before)?;
                    if let Some(baseline) = row.baseline {
                        let pointer = glue(services, baseline, "normal")?;
                        services.builder_mut().tail_append(pointer)?
                    }
                    services.builder_mut().tail_append(packed)?;
                    services.splice(&after)?;
                }
            }
        }
        services.builder_mut().release(proto, false)?;
        let result = services
            .with_layout(|_, context| context.builder.take_list(context.fonts, context.env))?;
        self.results.insert(
            result,
            AlignmentList {
                previous_depth: preceding_depth,
                rows: row_boxes,
            },
        );
        let pointers = services.builder().siblings(result)?;
        self.held.extend(pointers.iter().copied());
        held_guard.roots.borrow_mut().extend(pointers);
        services.builder_mut().pop_context()?;
        services.end_group("alignment", false)?;
        Ok(result)
    }
    pub(crate) fn prepare_enter(
        services: &mut impl AlignmentServices,
        vertical: bool,
    ) -> Result<()> {
        if vertical {
            services.start_paragraph(false)?
        } else {
            services.finish_paragraph()?;
            if services.builder().context.mode != ListMode::Vertical {
                return Err(Error::value("Horizontal alignment requires vertical mode")
                    .with_issue(crate::core::recovery::Issue::Alignment));
            }
        }
        Ok(())
    }
    pub fn enter(&mut self, services: &mut impl AlignmentServices, vertical: bool) -> Result<()> {
        Self::prepare_enter(services, vertical)?;
        let head = self.read(services, vertical)?;
        self.attach(services, vertical, head)
    }
    pub(crate) fn attach(
        &mut self,
        services: &mut impl AlignmentServices,
        vertical: bool,
        head: i64,
    ) -> Result<()> {
        let transfer = self.prepare_attach(services, vertical, head)?;
        let result = services.consume_outer();
        self.held.truncate(transfer.held_start);
        result
    }
    pub(crate) fn prepare_attach(
        &mut self,
        services: &mut impl AlignmentServices,
        vertical: bool,
        head: i64,
    ) -> Result<AlignmentTransfer> {
        let pointers = services.builder().siblings(head)?;
        let final_depth = self
            .results
            .remove(&head)
            .ok_or_else(|| Error::value("Missing alignment result"))?
            .previous_depth;
        let held_start = self.held.len();
        self.held.extend(pointers.iter().copied());
        let _guard = services.builder().ownership.hold(pointers.iter().copied());
        let result = (|| {
            for pointer in pointers {
                services.builder_mut().node_mut(pointer)?.link = C::NULL;
                services.builder_mut().tail_append(pointer)?
            }
            if !vertical {
                services.builder_mut().context.prev_depth = final_depth
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.held.truncate(held_start);
            return Err(error);
        }
        // All rows now belong to the current list. Page/output callbacks may
        // consume them before returning, so the transfer guard ends here.
        _guard.roots.borrow_mut().clear();
        self.held.truncate(held_start);
        Ok(AlignmentTransfer {
            held_start,
            _hold: _guard,
        })
    }
}

fn glue(s: &mut impl AlignmentServices, spec: GlueSpec, kind: &str) -> Result<i64> {
    let font = s.fonts().current;
    s.builder_mut().new_glue(spec, kind, font)
}

pub(crate) struct AlignmentTransfer {
    pub held_start: usize,
    _hold: crate::core::runtime::RootHold,
}
