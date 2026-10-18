//! Owned alignment lifecycle: template scanning, cells, noalign and cleanup.
use super::*;
use crate::{
    core::runtime::RootHold,
    language::{
        commands::traits,
        scanning::{ScanRequest, ScanValue},
    },
};

pub(crate) enum AlignmentAction {
    Boundary,
    RawToken,
    ExpandedToken {
        protected: bool,
    },
    ExpandCurrent,
    Scan(ScanRequest),
    ExecuteToken,
    Noalign {
        vertical: bool,
        initial_depth: Option<i64>,
    },
}
pub(crate) enum AlignmentValue {
    Unit,
    Scan(ScanValue),
    Executed(bool),
    Box {
        pointer: i64,
        capture: BoxScanContext,
    },
}
pub(crate) enum AlignmentStep {
    Yield(AlignmentAction),
    Complete(i64),
}
pub(super) fn drive_action(
    s: &mut impl AlignmentServices,
    action: AlignmentAction,
) -> Result<AlignmentValue> {
    match action {
        AlignmentAction::Boundary => Ok(AlignmentValue::Unit),
        AlignmentAction::RawToken => {
            s.raw_token()?;
            Ok(AlignmentValue::Unit)
        }
        AlignmentAction::ExpandedToken { protected } => {
            if protected {
                s.expanded_protected()?
            } else {
                s.expanded_token()?
            }
            Ok(AlignmentValue::Unit)
        }
        AlignmentAction::ExpandCurrent => {
            s.expand_current()?;
            Ok(AlignmentValue::Unit)
        }
        AlignmentAction::Scan(request) => s.scan(request).map(AlignmentValue::Scan),
        AlignmentAction::ExecuteToken => s.execute_token(true).map(AlignmentValue::Executed),
        AlignmentAction::Noalign {
            vertical,
            initial_depth,
        } => {
            let mut capture = BoxScanContext::default();
            let pointer = BoxCommands::read_box(
                s,
                if vertical {
                    BoxOperation::Hbox
                } else {
                    BoxOperation::Vbox
                },
                Some(&mut capture),
                initial_depth,
                true,
                false,
                Some(GroupKind::NoAlign),
            )?;
            Ok(AlignmentValue::Box { pointer, capture })
        }
    }
}
fn alignment_scan(value: Option<AlignmentValue>) -> Result<ScanValue> {
    match value {
        Some(AlignmentValue::Scan(value)) => Ok(value),
        _ => Err(Error::runtime("Invalid alignment scan result")),
    }
}
fn alignment_unit(value: Option<AlignmentValue>) -> Result<()> {
    match value {
        Some(AlignmentValue::Unit) => Ok(()),
        _ => Err(Error::runtime("Invalid alignment token result")),
    }
}

pub(super) enum PreambleStep {
    Yield(AlignmentAction),
    Complete((Vec<Template>, Vec<GlueSpec>, Option<usize>)),
}
pub(super) struct PreambleExecution {
    stage: u8,
    templates: Vec<Template>,
    skips: Vec<GlueSpec>,
    current: Vec<Token>,
    positions: Vec<usize>,
    level: i64,
    repeat: Option<usize>,
}
impl PreambleExecution {
    pub(super) fn new(s: &impl AlignmentServices) -> Self {
        Self {
            stage: 0,
            templates: Vec::new(),
            skips: vec![Alignments::tabskip(s)],
            current: Vec::new(),
            positions: Vec::new(),
            level: 0,
            repeat: None,
        }
    }
    pub(super) fn resume(
        &mut self,
        s: &mut impl AlignmentServices,
        returned: Option<Result<AlignmentValue>>,
    ) -> Result<PreambleStep> {
        let value = returned.transpose()?;
        let action = match self.stage {
            0 => {
                self.stage = 1;
                AlignmentAction::RawToken
            }
            1 => {
                alignment_unit(value)?;
                if s.input().current.is_command(Command::AlignSpan) {
                    self.stage = 2;
                    AlignmentAction::RawToken
                } else {
                    return self.token(s);
                }
            }
            2 => {
                alignment_unit(value)?;
                if s.input()
                    .current
                    .operation()
                    .is_some_and(|command| traits(command).expandable)
                {
                    self.stage = 3;
                    AlignmentAction::ExpandCurrent
                } else {
                    self.stage = 1;
                    return self.resume(s, Some(Ok(AlignmentValue::Unit)));
                }
            }
            3 => {
                alignment_unit(value)?;
                self.stage = 1;
                AlignmentAction::RawToken
            }
            4 => {
                if !matches!(value, Some(AlignmentValue::Executed(_))) {
                    return Err(Error::runtime("Invalid alignment assignment result"));
                }
                self.stage = 0;
                return self.resume(s, None);
            }
            _ => return Err(Error::runtime("Alignment preamble already completed")),
        };
        Ok(PreambleStep::Yield(action))
    }
    fn token(&mut self, s: &mut impl AlignmentServices) -> Result<PreambleStep> {
        let token = s.input().current.clone();
        if self.level == 0
            && token.is_command(Command::Parameter)
            && matches!(&token.value, CommandOperand::Quantity(reference) if reference.key == ValueKey::parameter("tabskip"))
        {
            self.stage = 4;
            return Ok(PreambleStep::Yield(AlignmentAction::ExecuteToken));
        }
        if token.is_category(Cat::LeftBrace) {
            self.level += 1
        } else if token.is_category(Cat::RightBrace) {
            self.level -= 1
        }
        if self.level < 0 {
            return Err(Error::value("Unfinished alignment preamble")
                .with_issue(crate::core::recovery::Issue::Alignment));
        }
        if self.level == 0
            && (token.is_category(Cat::TabMark) || token.is_command(Command::AlignEnd))
        {
            if self.current.is_empty() && token.is_category(Cat::TabMark) {
                if self.repeat.is_some() {
                    return Err(Error::value("Multiple repeating alignment preambles")
                        .with_issue(crate::core::recovery::Issue::Alignment));
                }
                self.repeat = Some(self.templates.len());
            } else {
                if self.positions.len() != 1 {
                    return Err(Error::value("Alignment template requires one parameter")
                        .with_issue(crate::core::recovery::Issue::Alignment));
                }
                let index = self.positions[0];
                self.templates.push(Template {
                    prefix: self.current[..index].to_vec().into(),
                    suffix: self.current[index + 1..].to_vec().into(),
                });
                self.current.clear();
                self.positions.clear();
                self.skips.push(Alignments::tabskip(s));
                if token.is_command(Command::AlignEnd) {
                    self.stage = 5;
                    return Ok(PreambleStep::Complete((
                        std::mem::take(&mut self.templates),
                        std::mem::take(&mut self.skips),
                        self.repeat,
                    )));
                }
            }
        } else if !(self.current.is_empty() && token.is_category(Cat::Spacer)) {
            if token.is_category(Cat::MacParam) {
                self.positions.push(self.current.len())
            }
            self.current.push(token.token);
        }
        self.stage = 0;
        self.resume(s, None)
    }
}

pub(super) enum EntryStep {
    Yield(AlignmentAction),
    Complete((usize, usize, i64, MeaningCommand)),
}
pub(super) struct EntryExecution {
    start: usize,
    column: usize,
    stage: u8,
    opened: bool,
    handler: Option<Rc<RefCell<EntryHandler>>>,
    previous_handler: Option<Option<Rc<RefCell<EntryHandler>>>>,
    hold: Option<RootHold>,
}
impl EntryExecution {
    pub(super) fn new(column: usize) -> Self {
        Self {
            start: column,
            column,
            stage: 0,
            opened: false,
            handler: None,
            previous_handler: None,
            hold: None,
        }
    }
    fn cleanup(&mut self, s: &mut impl AlignmentServices, completed: bool) -> Result<()> {
        if let Some(previous) = self.previous_handler.take() {
            s.input_mut().alignment_handler = previous;
        }
        if !self.opened {
            return Ok(());
        }
        let head = s.with_layout(|_, c| c.builder.take_list(c.fonts, c.env))?;
        if head != C::NULL {
            s.builder_mut().release(head, true)?
        }
        let b = s.builder_mut();
        b.context.cur_l = None;
        b.context.lig_stack.clear();
        b.context.unicode_run.clear();
        b.pop_context()?;
        while s.env().groups.last().is_some_and(|g| g.kind != "alignment") {
            let kind = s.env().groups.last().unwrap().kind.clone();
            s.end_group(&kind, completed)?;
        }
        self.opened = false;
        Ok(())
    }
    pub(super) fn resume(
        &mut self,
        a: &mut Alignments,
        s: &mut impl AlignmentServices,
        templates: &mut Vec<Template>,
        skips: &mut Vec<GlueSpec>,
        repeat: Option<usize>,
        vertical: bool,
        sentinel: Token,
        returned: Option<Result<AlignmentValue>>,
    ) -> Result<EntryStep> {
        let result =
            self.resume_inner(a, s, templates, skips, repeat, vertical, sentinel, returned);
        if result.is_err() {
            self.cleanup(s, false)?;
        }
        result
    }
    fn resume_inner(
        &mut self,
        a: &mut Alignments,
        s: &mut impl AlignmentServices,
        templates: &mut Vec<Template>,
        skips: &mut Vec<GlueSpec>,
        repeat: Option<usize>,
        vertical: bool,
        sentinel: Token,
        returned: Option<Result<AlignmentValue>>,
    ) -> Result<EntryStep> {
        let value = returned.transpose()?;
        let action = match self.stage {
            0 => {
                self.hold = Some(s.builder().ownership.hold_component(a.held.iter().copied()));
                s.begin_group("alignment_entry", GroupKind::Alignment)?;
                s.with_layout(|_, c| {
                    c.builder.push_context(
                        c.fonts,
                        c.env,
                        if vertical {
                            ListMode::Vertical
                        } else {
                            ListMode::Horizontal
                        },
                        true,
                    )
                })?;
                self.opened = true;
                if vertical {
                    s.with_layout(|_, c| {
                        ParagraphOperations::normal_paragraph(c);
                        Ok(())
                    })?;
                }
                self.stage = 1;
                return self.resume_inner(a, s, templates, skips, repeat, vertical, sentinel, None);
            }
            1 => {
                while self.column >= templates.len() {
                    let repeat = repeat.ok_or_else(|| {
                        Error::value("Extra alignment tab")
                            .with_issue(crate::core::recovery::Issue::Alignment)
                    })?;
                    templates.push(a.period[(self.column - repeat) % a.period.len()].clone());
                    skips.push(a.period_skips[(self.column - repeat) % a.period.len()]);
                }
                self.stage = 2;
                AlignmentAction::ExpandedToken { protected: true }
            }
            2 => {
                alignment_unit(value)?;
                if s.input().current.is_category(Cat::Spacer) {
                    return Ok(EntryStep::Yield(AlignmentAction::ExpandedToken {
                        protected: true,
                    }));
                }
                let omit = s.input().current.is_command(Command::AlignOmit);
                let prefix_marker = a.token(s, AlignmentBoundary::PrefixEnd);
                let handler = Rc::new(RefCell::new(EntryHandler {
                    phase: if omit {
                        EntryPhase::Body
                    } else {
                        EntryPhase::Prefix
                    },
                    baseline: s.input().align_state,
                    boundary: None,
                    omit,
                    suffix: templates[self.column].suffix.clone(),
                    sentinel,
                }));
                self.previous_handler =
                    Some(s.input_mut().alignment_handler.replace(handler.clone()));
                self.handler = Some(handler);
                if !omit {
                    s.back_input()?;
                    let mut prefix = templates[self.column].prefix.to_vec();
                    prefix.push(prefix_marker);
                    s.input_mut().begin_tokens(prefix.into(), None, None)?;
                }
                self.stage = 3;
                AlignmentAction::ExpandedToken { protected: false }
            }
            3 => {
                alignment_unit(value)?;
                if s.input().current.is_command(Command::AlignSentinel)
                    && matches!(
                        s.input().current.value,
                        CommandOperand::Alignment(AlignmentBoundary::EntryEnd)
                    )
                {
                    if s.insert_par_token(1)? {
                        return Ok(EntryStep::Yield(AlignmentAction::ExpandedToken {
                            protected: false,
                        }));
                    }
                    s.input_mut().alignment_handler = self.previous_handler.take().unwrap();
                    let boundary =
                        self.handler
                            .take()
                            .unwrap()
                            .borrow()
                            .boundary
                            .ok_or_else(|| {
                                Error::value("Missing alignment boundary")
                                    .with_issue(crate::core::recovery::Issue::Alignment)
                            })?;
                    self.column += 1;
                    if boundary == MeaningCommand::Operation(Command::AlignSpan) {
                        self.stage = 1;
                        return self.resume_inner(
                            a, s, templates, skips, repeat, vertical, sentinel, None,
                        );
                    }
                    s.finish_paragraph()?;
                    BoxCommands::flush(s)?;
                    let pointer = s.with_layout(|layout, c| {
                        let head = c.builder.take_list(c.fonts, c.env)?;
                        if vertical {
                            layout.vpack(c, head, false, None, false, 0)
                        } else {
                            layout.hpack(c, head, None, false, false)
                        }
                    })?;
                    a.held.push(pointer);
                    self.hold.as_ref().unwrap().roots.borrow_mut().push(pointer);
                    if !vertical {
                        a.adjustments
                            .insert(pointer, s.extract_adjustments(pointer)?);
                    }
                    self.cleanup(s, true)?;
                    self.stage = 5;
                    return Ok(EntryStep::Complete((
                        self.start,
                        self.column - self.start,
                        pointer,
                        boundary,
                    )));
                }
                self.stage = 4;
                AlignmentAction::ExecuteToken
            }
            4 => {
                match value {
                    Some(AlignmentValue::Executed(true)) => {}
                    Some(AlignmentValue::Executed(false)) => {
                        return Err(Error::value("Document end in alignment")
                            .with_issue(crate::core::recovery::Issue::Alignment));
                    }
                    _ => return Err(Error::runtime("Invalid alignment command result")),
                }
                self.stage = 3;
                AlignmentAction::ExpandedToken { protected: false }
            }
            _ => return Err(Error::runtime("Alignment entry already completed")),
        };
        Ok(EntryStep::Yield(action))
    }
}

pub(crate) struct AlignmentExecution {
    pub alignment: Alignments,
    vertical: bool,
    stage: u8,
    target: Option<i64>,
    spread: bool,
    depths: Option<(usize, usize, usize)>,
    previous_handler: Option<Rc<RefCell<EntryHandler>>>,
    previous_period: Option<(Vec<Template>, Vec<GlueSpec>)>,
    status: Option<ScannerStatus>,
    preceding_depth: Option<i64>,
    hold: Option<RootHold>,
    sentinel: Option<Token>,
    preamble: Option<PreambleExecution>,
    templates: Vec<Template>,
    skips: Vec<GlueSpec>,
    repeat: Option<usize>,
    rows: Vec<Row>,
    widths: Vec<Option<i64>>,
    hook: Arc<[Token]>,
    entry: Option<EntryExecution>,
    entries: Vec<(usize, usize, i64)>,
    column: usize,
}
impl AlignmentExecution {
    pub fn new(alignment: Alignments, vertical: bool) -> Self {
        Self {
            alignment,
            vertical,
            stage: 0,
            target: None,
            spread: false,
            depths: None,
            previous_handler: None,
            previous_period: None,
            status: None,
            preceding_depth: None,
            hold: None,
            sentinel: None,
            preamble: None,
            templates: Vec::new(),
            skips: Vec::new(),
            repeat: None,
            rows: Vec::new(),
            widths: Vec::new(),
            hook: Arc::from([]),
            entry: None,
            entries: Vec::new(),
            column: 0,
        }
    }
    fn cleanup(&mut self, s: &mut impl AlignmentServices) -> Result<()> {
        if let Some(status) = self.status.take() {
            s.input_mut().scanner_status = status;
        }
        if let Some(entry) = &mut self.entry {
            entry.cleanup(s, false)?;
        }
        let Some((groups, contexts, held)) = self.depths.take() else {
            return Ok(());
        };
        s.input_mut().alignment_handler = self.previous_handler.take();
        let previous = self.previous_period.take().unwrap();
        (self.alignment.period, self.alignment.period_skips) = previous;
        // Preserve the original group-before-context error cleanup ordering.
        let result = (|| {
            while s.env().groups.len() > groups {
                let kind = s.env().groups.last().unwrap().kind.clone();
                s.end_group(&kind, false)?;
            }
            while s.builder().context_stack.len() > contexts {
                let head = s.with_layout(|_, c| c.builder.take_list(c.fonts, c.env))?;
                if head != C::NULL {
                    s.builder_mut().release(head, true)?
                }
                s.builder_mut().pop_context()?;
            }
            Ok(())
        })();
        self.alignment.held.truncate(held);
        self.alignment.depth -= 1;
        self.hold = None;
        result
    }
    pub fn resume(
        &mut self,
        s: &mut impl AlignmentServices,
        returned: Option<Result<AlignmentValue>>,
    ) -> Result<AlignmentStep> {
        let result = self.resume_inner(s, returned);
        if result.is_err() || matches!(result, Ok(AlignmentStep::Complete(_))) {
            self.cleanup(s)?;
        }
        result
    }
    fn resume_inner(
        &mut self,
        s: &mut impl AlignmentServices,
        returned: Option<Result<AlignmentValue>>,
    ) -> Result<AlignmentStep> {
        // Children need the original error to perform their own unwind first.
        if self.stage == 6 {
            let step = self.preamble.as_mut().unwrap().resume(s, returned);
            let step = match step {
                Err(error) => {
                    s.input_mut().scanner_status = self.status.take().unwrap();
                    return Err(error);
                }
                Ok(step) => step,
            };
            return match step {
                PreambleStep::Yield(action) => Ok(AlignmentStep::Yield(action)),
                PreambleStep::Complete((templates, skips, repeat)) => {
                    s.input_mut().scanner_status = self.status.take().unwrap();
                    self.templates = templates;
                    self.skips = skips;
                    self.repeat = repeat;
                    if let Some(repeat) = repeat {
                        self.alignment.period = self.templates[repeat..].to_vec();
                        self.alignment.period_skips = self.skips[repeat + 1..].to_vec();
                        if self.alignment.period.is_empty() {
                            return Err(Error::value("Empty repeating preamble")
                                .with_issue(crate::core::recovery::Issue::Alignment));
                        }
                    }
                    self.widths = vec![None; self.templates.len()];
                    self.hook = match s.env().values.get(&ValueKey::parameter("everycr")) {
                        Some(Quantity::Tokens(tokens)) => tokens.clone(),
                        _ => Arc::from([]),
                    };
                    if !self.hook.is_empty() {
                        s.input_mut().begin_tokens(self.hook.clone(), None, None)?;
                    }
                    self.preamble = None;
                    self.stage = 7;
                    self.resume_inner(s, None)
                }
            };
        }
        if self.stage == 10 {
            let step = self.entry.as_mut().unwrap().resume(
                &mut self.alignment,
                s,
                &mut self.templates,
                &mut self.skips,
                self.repeat,
                self.vertical,
                self.sentinel.unwrap(),
                returned,
            )?;
            return match step {
                EntryStep::Yield(action) => Ok(AlignmentStep::Yield(action)),
                EntryStep::Complete((start, span, pointer, boundary)) => {
                    self.hold.as_ref().unwrap().roots.borrow_mut().push(pointer);
                    self.entries.push((start, span, pointer));
                    self.column += span;
                    self.widths.resize(self.templates.len(), None);
                    let node = s.builder().node(pointer)?;
                    if span == 1 {
                        let width = if self.vertical {
                            node.height
                        } else {
                            node.width
                        };
                        self.widths[start] = Some(self.widths[start].unwrap_or(0).max(width));
                    }
                    self.entry = None;
                    if boundary == MeaningCommand::Operation(Command::AlignEnd) {
                        let (mut height, mut depth) = (0, 0);
                        for &(_, _, pointer) in &self.entries {
                            let node = s.builder().node(pointer)?;
                            height = height.max(node.height);
                            depth = depth.max(node.depth);
                        }
                        let baseline = if self.vertical {
                            None
                        } else {
                            s.with_layout(|_, c| {
                                Ok(ParagraphOperations::baseline_glue(
                                    c,
                                    height,
                                    self.preceding_depth,
                                ))
                            })?
                        };
                        self.rows.push(Row::Entries(AlignmentRow {
                            entries: std::mem::take(&mut self.entries),
                            baseline,
                        }));
                        if !self.vertical {
                            self.preceding_depth = Some(depth);
                        }
                        if !self.hook.is_empty() {
                            s.input_mut().begin_tokens(self.hook.clone(), None, None)?;
                        }
                        self.stage = 7;
                    } else {
                        self.entry = Some(EntryExecution::new(self.column));
                    }
                    // Return between entries rather than recursing over arbitrary rows.
                    Ok(AlignmentStep::Yield(AlignmentAction::Boundary))
                }
            };
        }
        let value = returned.transpose()?;
        let action = match self.stage {
            0 => {
                self.stage = 1;
                AlignmentAction::Scan(ScanRequest::Keyword("to".into()))
            }
            1 => {
                if alignment_scan(value)?.flag()? {
                    self.stage = 3;
                    AlignmentAction::Scan(ScanRequest::Dimen {
                        mu: false,
                        infinite: false,
                    })
                } else {
                    self.stage = 2;
                    AlignmentAction::Scan(ScanRequest::Keyword("spread".into()))
                }
            }
            2 => {
                if alignment_scan(value)?.flag()? {
                    self.spread = true;
                    self.stage = 3;
                    AlignmentAction::Scan(ScanRequest::Dimen {
                        mu: false,
                        infinite: false,
                    })
                } else {
                    self.stage = 4;
                    return self.resume_inner(s, None);
                }
            }
            3 => {
                self.target = Some(alignment_scan(value)?.integer()?);
                self.stage = 4;
                return self.resume_inner(s, None);
            }
            4 => {
                self.stage = 5;
                AlignmentAction::ExpandedToken { protected: false }
            }
            5 => {
                alignment_unit(value)?;
                if s.input().current.is_category(Cat::Spacer)
                    || s.input().current.is_command(Command::Relax)
                {
                    self.stage = 4;
                    return self.resume_inner(s, None);
                }
                if !s.input().current.is_category(Cat::LeftBrace) {
                    return Err(Error::value("Expected a left brace")
                        .with_issue(crate::core::recovery::Issue::MissingBrace));
                }
                self.alignment.depth += 1;
                self.depths = Some((
                    s.env().groups.len(),
                    s.builder().context_stack.len(),
                    self.alignment.held.len(),
                ));
                self.previous_handler = s.input_mut().alignment_handler.take();
                self.previous_period = Some((
                    std::mem::take(&mut self.alignment.period),
                    std::mem::take(&mut self.alignment.period_skips),
                ));
                self.sentinel = Some(self.alignment.token(s, AlignmentBoundary::EntryEnd));
                self.preceding_depth = s.builder().context.prev_depth;
                if s.builder().context.mode == ListMode::Math {
                    self.preceding_depth = s
                        .builder()
                        .context_stack
                        .iter()
                        .rev()
                        .find(|c| c.mode == ListMode::Vertical)
                        .ok_or_else(|| {
                            Error::value("Display alignment without vertical context")
                                .with_issue(crate::core::recovery::Issue::Alignment)
                        })?
                        .prev_depth;
                }
                self.hold = Some(
                    s.builder()
                        .ownership
                        .hold_component(self.alignment.held.iter().copied()),
                );
                s.begin_group("alignment", GroupKind::Alignment)?;
                s.with_layout(|_, c| {
                    c.builder.push_context(
                        c.fonts,
                        c.env,
                        if self.vertical {
                            ListMode::Horizontal
                        } else {
                            ListMode::Vertical
                        },
                        true,
                    )
                })?;
                self.status = Some(s.input().scanner_status);
                s.input_mut().scanner_status = ScannerStatus::Aligning;
                self.preamble = Some(PreambleExecution::new(s));
                self.stage = 6;
                return self.resume_inner(s, None);
            }
            7 => {
                self.stage = 8;
                AlignmentAction::ExpandedToken { protected: true }
            }
            8 => {
                alignment_unit(value)?;
                if s.input().current.is_category(Cat::Spacer) {
                    self.stage = 7;
                    return self.resume_inner(s, None);
                }
                if s.input().current.is_category(Cat::RightBrace) {
                    let pointer = self.alignment.finish_rows(
                        s,
                        self.vertical,
                        self.target,
                        self.spread,
                        std::mem::take(&mut self.rows),
                        std::mem::take(&mut self.widths),
                        std::mem::take(&mut self.skips),
                        self.preceding_depth,
                        self.hold.as_ref().unwrap(),
                    )?;
                    self.stage = 11;
                    return Ok(AlignmentStep::Complete(pointer));
                }
                if s.input().current.is_command(Command::AlignEnd)
                    && s.input().current.character() == Some(1)
                {
                    self.stage = 7;
                    return self.resume_inner(s, None);
                }
                if s.input().current.is_command(Command::AlignNoalign) {
                    self.stage = 9;
                    AlignmentAction::Noalign {
                        vertical: self.vertical,
                        initial_depth: self.preceding_depth,
                    }
                } else {
                    s.back_input()?;
                    self.column = 0;
                    self.entry = Some(EntryExecution::new(0));
                    self.stage = 10;
                    return self.resume_inner(s, None);
                }
            }
            9 => {
                let Some(AlignmentValue::Box { pointer, capture }) = value else {
                    return Err(Error::runtime("Invalid noalign result"));
                };
                self.preceding_depth = capture.prev_depth;
                self.alignment.held.push(pointer);
                self.hold.as_ref().unwrap().roots.borrow_mut().push(pointer);
                self.rows.push(Row::Material(pointer));
                self.stage = 7;
                return self.resume_inner(s, None);
            }
            _ => return Err(Error::runtime("Alignment already completed")),
        };
        Ok(AlignmentStep::Yield(action))
    }
}
