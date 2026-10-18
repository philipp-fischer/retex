//! Owned box lifecycle, shared by synchronous callers and resident execution.
use super::{
    box_commands::{BoxCommands, BoxServices},
    model::BoxScanContext,
    paragraph_operations::ParagraphOperations,
};
use crate::typesetting::model::NodeFields;
use crate::{
    core::{
        constants::Constants as C,
        enums::{BoxOperation, CharacterCategory as Cat, Command, GroupKind, ListMode},
        errors::{Error, ErrorKind, Result},
        values::{GlueSpec, MAX_DIMEN},
    },
    language::{
        records::{Quantity, ValueKey},
        scanning::{ScanRequest, ScanValue},
    },
};

pub(crate) enum BoxAction {
    ExpandedToken,
    Scan(ScanRequest),
    ExecuteToken,
}
pub(crate) enum BoxValue {
    Unit,
    Scan(ScanValue),
    Executed(bool),
}
pub(crate) enum BoxStep {
    Yield(BoxAction),
    Complete(i64),
}
#[derive(Clone)]
enum Stage {
    To,
    ToValue,
    Spread,
    SpreadValue,
    LeftBrace,
    Open,
    Next,
    Token,
    Executed,
    Done,
}
#[derive(Clone)]
pub(crate) struct BoxExecution {
    code: BoxOperation,
    target: Option<i64>,
    spread: bool,
    initial_depth: Option<i64>,
    hook: bool,
    group: Option<GroupKind>,
    depths: Option<(usize, usize)>,
    stage: Stage,
    pub capture: Option<BoxScanContext>,
}
impl BoxExecution {
    pub fn new(
        code: BoxOperation,
        capture: Option<BoxScanContext>,
        initial_depth: Option<i64>,
        spec: bool,
        hook: bool,
        group: Option<GroupKind>,
    ) -> Self {
        Self {
            code,
            target: None,
            spread: false,
            initial_depth,
            hook,
            group,
            depths: None,
            stage: if spec { Stage::To } else { Stage::LeftBrace },
            capture,
        }
    }
    pub fn resume(
        &mut self,
        services: &mut impl BoxServices,
        returned: Option<Result<BoxValue>>,
    ) -> Result<BoxStep> {
        let result = self.resume_inner(services, returned);
        if result.is_err() {
            let mapped = result.map_err(|error| {
                if self.depths.is_some() && error.kind == ErrorKind::Eof {
                    Error::value("Unexpected EOF in box")
                        .with_issue(crate::core::recovery::Issue::RunawayArgument)
                } else {
                    error
                }
            });
            self.abort(services)?;
            return mapped;
        }
        result
    }
    fn resume_inner(
        &mut self,
        services: &mut impl BoxServices,
        returned: Option<Result<BoxValue>>,
    ) -> Result<BoxStep> {
        let value = returned.transpose()?;
        match self.stage {
            Stage::To => {
                self.stage = Stage::ToValue;
                Ok(BoxStep::Yield(BoxAction::Scan(ScanRequest::Keyword(
                    "to".into(),
                ))))
            }
            Stage::ToValue => {
                if scan(value)?.flag()? {
                    self.stage = Stage::SpreadValue;
                    Ok(BoxStep::Yield(BoxAction::Scan(ScanRequest::Dimen {
                        mu: false,
                        infinite: false,
                    })))
                } else {
                    self.stage = Stage::Spread;
                    Ok(BoxStep::Yield(BoxAction::Scan(ScanRequest::Keyword(
                        "spread".into(),
                    ))))
                }
            }
            Stage::Spread => {
                if scan(value)?.flag()? {
                    self.spread = true;
                    self.stage = Stage::SpreadValue;
                    Ok(BoxStep::Yield(BoxAction::Scan(ScanRequest::Dimen {
                        mu: false,
                        infinite: false,
                    })))
                } else {
                    self.stage = Stage::LeftBrace;
                    self.resume_inner(services, None)
                }
            }
            Stage::SpreadValue => {
                self.target = Some(scan(value)?.integer()?);
                self.stage = Stage::LeftBrace;
                self.resume_inner(services, None)
            }
            Stage::LeftBrace => {
                self.stage = Stage::Open;
                Ok(BoxStep::Yield(BoxAction::ExpandedToken))
            }
            Stage::Open => {
                unit(value)?;
                let current = &services.input().current;
                if current.is_category(Cat::Spacer) || current.is_command(Command::Relax) {
                    self.stage = Stage::LeftBrace;
                    return self.resume_inner(services, None);
                }
                if !current.is_category(Cat::LeftBrace) {
                    return Err(Error::value("Expected a left brace")
                        .with_issue(crate::core::recovery::Issue::MissingBrace));
                }
                self.depths = Some((
                    services.env().groups.len(),
                    services.builder().context_stack.len(),
                ));
                services.with_layout(|_, c| {
                    c.builder.push_context(
                        c.fonts,
                        c.env,
                        if self.code == BoxOperation::Hbox {
                            ListMode::Horizontal
                        } else {
                            ListMode::Vertical
                        },
                        true,
                    )
                })?;
                services.builder_mut().context.prev_depth = self.initial_depth;
                let group = self.group.unwrap_or(match self.code {
                    BoxOperation::Hbox => GroupKind::Hbox,
                    BoxOperation::Vbox => GroupKind::Vbox,
                    BoxOperation::Vtop => GroupKind::Vtop,
                    _ => return Err(Error::value("Invalid box constructor")),
                });
                services.begin_group("box", group)?;
                if self.code != BoxOperation::Hbox {
                    services.with_layout(|_, c| {
                        ParagraphOperations::normal_paragraph(c);
                        Ok(())
                    })?;
                }
                if let Some(token) = services.pending_box_assignment() {
                    let source = services.input().source();
                    services
                        .input_mut()
                        .begin_tokens(vec![token].into(), source, None)?;
                }
                if self.hook {
                    services.run_hook(if self.code == BoxOperation::Hbox {
                        "everyhbox"
                    } else {
                        "everyvbox"
                    })?;
                }
                self.stage = Stage::Next;
                self.resume_inner(services, None)
            }
            Stage::Next => {
                self.stage = Stage::Token;
                Ok(BoxStep::Yield(BoxAction::ExpandedToken))
            }
            Stage::Token => {
                unit(value)?;
                if services.input().current.is_category(Cat::RightBrace)
                    && services.env().groups.len() == self.depths.unwrap().0 + 1
                {
                    let threshold = if matches!(
                        self.group,
                        Some(GroupKind::NoAlign | GroupKind::Output | GroupKind::Insert)
                    ) {
                        1
                    } else {
                        0
                    };
                    if services.insert_par_token(threshold)? {
                        self.stage = Stage::Next;
                        return self.resume_inner(services, None);
                    }
                    services.finish_paragraph()?;
                    BoxCommands::flush(services)?;
                    let max_depth = services.parameter("boxmaxdepth", MAX_DIMEN);
                    if let Some(capture) = &mut self.capture {
                        capture.prev_depth = services.builder().context.prev_depth;
                        capture.split_top_skip = match services
                            .env()
                            .values
                            .get(&ValueKey::parameter("splittopskip"))
                        {
                            Some(Quantity::Glue(spec)) => *spec,
                            _ => GlueSpec::default(),
                        };
                        capture.split_max_depth = services.parameter("splitmaxdepth", MAX_DIMEN);
                        capture.floating_penalty = services.parameter("floatingpenalty", 0);
                    }
                    services.end_group("box", true)?;
                    BoxCommands::flush(services)?;
                    let pointer = services.with_layout(|layout, c| {
                        let head = c.builder.take_list(c.fonts, c.env)?;
                        if self.code == BoxOperation::Hbox {
                            layout.hpack(c, head, self.target, self.spread, false)
                        } else {
                            layout.vpack(
                                c,
                                head,
                                self.code == BoxOperation::Vtop,
                                self.target,
                                self.spread,
                                max_depth,
                            )
                        }
                    })?;
                    self.abort(services)?;
                    return Ok(BoxStep::Complete(pointer));
                }
                self.stage = Stage::Executed;
                Ok(BoxStep::Yield(BoxAction::ExecuteToken))
            }
            Stage::Executed => {
                match value {
                    Some(BoxValue::Executed(true)) => {}
                    Some(BoxValue::Executed(false)) => {
                        return Err(Error::value("Document end inside an unfinished box")
                            .with_issue(crate::core::recovery::Issue::RunawayArgument));
                    }
                    _ => return Err(Error::runtime("Invalid box command result")),
                }
                self.stage = Stage::Next;
                self.resume_inner(services, None)
            }
            Stage::Done => Err(Error::runtime("Box execution already completed")),
        }
    }
    pub fn abort(&mut self, services: &mut impl BoxServices) -> Result<()> {
        self.stage = Stage::Done;
        let Some((outer_depth, context_depth)) = self.depths.take() else {
            return Ok(());
        };
        while services.env().groups.len() > outer_depth {
            let kind = services.env().groups.last().unwrap().kind.clone();
            services.end_group(&kind, false)?;
        }
        while services.builder().context_stack.len() > context_depth {
            let builder = services.builder_mut();
            let head = builder.context.cur_list_head;
            builder.context.cur_list_head = C::NULL;
            builder.context.cur_list_tail = C::NULL;
            builder.context.cur_l = None;
            builder.context.lig_stack.clear();
            builder.context.unicode_run.clear();
            builder.context.lig_origins.clear();
            if head != C::NULL {
                builder.release(head, true)?;
            }
            builder.pop_context()?;
        }
        Ok(())
    }
}
fn scan(value: Option<BoxValue>) -> Result<ScanValue> {
    if let Some(BoxValue::Scan(value)) = value {
        Ok(value)
    } else {
        Err(Error::runtime("Invalid box scanner result"))
    }
}
fn unit(value: Option<BoxValue>) -> Result<()> {
    if matches!(value, Some(BoxValue::Unit)) {
        Ok(())
    } else {
        Err(Error::runtime("Invalid box token result"))
    }
}

pub(crate) enum RuleStep {
    Scan(ScanRequest),
    Complete(i64),
}
pub(crate) struct RuleExecution {
    dimensions: [i64; 3],
    running: [bool; 3],
    keyword: usize,
    dimension: bool,
    started: bool,
}
impl RuleExecution {
    pub fn new(vertical: bool) -> Self {
        Self {
            dimensions: [
                if vertical { 26214 } else { 0 },
                if vertical { 0 } else { 26214 },
                0,
            ],
            running: [!vertical, vertical, vertical],
            keyword: 0,
            dimension: false,
            started: false,
        }
    }
    pub fn resume(
        &mut self,
        s: &mut impl BoxServices,
        returned: Option<Result<ScanValue>>,
    ) -> Result<RuleStep> {
        if self.started {
            let value = returned.ok_or_else(|| Error::runtime("Missing rule operand result"))??;
            if self.dimension {
                self.dimensions[self.keyword] = value.integer()?;
                self.running[self.keyword] = false;
                self.keyword = 0;
                self.dimension = false;
            } else if value.flag()? {
                self.dimension = true;
                return Ok(RuleStep::Scan(ScanRequest::Dimen {
                    mu: false,
                    infinite: false,
                }));
            } else {
                self.keyword += 1;
            }
        }
        self.started = true;
        if self.keyword == 3 {
            let origins = s.input().source().into_iter().map(Some).collect();
            let pointer = s
                .builder_mut()
                .allocate(super::model::Node::from(NodeFields {
                    node_type: crate::core::enums::NodeKind::Rule,
                    width: self.dimensions[0],
                    height: self.dimensions[1],
                    depth: self.dimensions[2],
                    running_width: self.running[0],
                    running_height: self.running[1],
                    running_depth: self.running[2],
                    origins,
                    ..Default::default()
                }))?;
            return Ok(RuleStep::Complete(pointer));
        }
        Ok(RuleStep::Scan(ScanRequest::Keyword(
            ["width", "height", "depth"][self.keyword].into(),
        )))
    }
}
