//! Owned composition of executable commands, scanners and nested box lifecycles.
//! The existing dispatcher supplies the semantics; synchronous and sliced runs
//! drive this same stack. Alignment parsing and cells yield here; packing and page
//! callbacks still contain synchronous work.
use super::{
    evaluation::{Evaluation, EvaluationProgress, Request, Value},
    *,
};
use crate::typesetting::model::NodeFields;
use crate::{
    core::enums::{BoxOperation, CharacterCategory as Cat, Command as Cmd, GroupKind, ListMode},
    language::{
        commands::traits,
        expansion::{ExpansionRequest, ExpansionValue},
        font_commands::FontCommandServices,
        font_execution::{FontAction, FontExecution, FontStep, FontValue},
        input::ExpandedInput,
        records::CommandOperand,
        scanning::{ScanRequest, ScanValue},
        services::{LanguageServices, ListServices},
    },
    typesetting::{
        alignments::{AlignmentAction, AlignmentExecution, AlignmentStep, AlignmentValue},
        box_commands::BoxServices,
        box_execution::{BoxAction, BoxExecution, BoxStep, BoxValue, RuleExecution, RuleStep},
        box_operands::{
            AccentAction, AccentExecution, AccentStep, AccentValue, DiscretionaryExecution,
            DiscretionaryStep, UnpackAction, UnpackExecution, UnpackStep,
        },
        math_atoms::Atom,
        math_construction::MathContext,
        math_layout::{
            DisplayAction, DisplayExecution, DisplayStep, MathAction, MathExecution, MathStep,
            MathValue,
        },
        math_parser::{MathParser, MathParserAction, MathParserStep, MathParserValue, MathStop},
        math_renderer::MathRenderer,
        model::BoxScanContext,
        pages::{PageExecution, PageStep},
    },
};

use crate::typesetting::alignments::AlignmentTransfer;
mod forms;
mod navigation;
mod paragraph;
mod pdf_operands;
mod shipment;
pub(super) use forms::FormDefinitionTask;
use forms::{FinishPageTask, FlushTask, FormTask};
pub(super) use navigation::{ImageTask, NavigationTask, PdfScanTask};
pub(super) use paragraph::{AppendTask, ParagraphMaterial, ParagraphTask, SpliceTask};
pub(super) use pdf_operands::PdfOperandTask;
use shipment::{EffectTask, ShipTask};

pub(super) enum CommandStep {
    Complete(bool),
    Yield(CommandRequest),
}
pub(super) struct StartedCommand {
    pub pending: Option<Token>,
    pub step: CommandStep,
}
pub(super) enum CommandRequest {
    Command { in_box: bool, global: bool },
    Evaluation(Request),
    BoxTask(BoxTask),
    ReadBox(BoxExecution),
    Prefix(Prefix),
    Math(MathExecution),
    ParseMath(MathParser),
    RenderMath(MathRenderer),
    Operand(OperandTask),
    Alignment { vertical: bool, enter: bool },
    Pages(PageTask),
    Ship(ShipTask),
    Effect(EffectTask),
    Form(FormTask),
    FormDefinition(FormDefinitionTask),
    PdfOperand(PdfOperandTask),
    PdfScan(PdfScanTask),
    Navigation(NavigationTask),
    Image(ImageTask),
    Flush(FlushTask),
    FinishPage(FinishPageTask),
    PlaceAlignment(PlaceAlignmentTask),
    PlaceDisplay(PlaceDisplayTask),
    Scalar(ScalarTask),
    Discretionary(DiscretionaryExecution),
    Accent(AccentExecution),
    Unpack(UnpackExecution),
    Rule(RuleTask),
    ScanRule(RuleExecution),
    Font(FontExecution),
    Paragraph(ParagraphTask),
    Append(AppendTask),
    Splice(SpliceTask),
}
#[derive(Clone)]
pub(super) enum BoxIntent {
    Make { adjusted: bool },
    Set { global: bool },
    Ship,
    Insert,
    Adjust,
    Return,
}
#[derive(Clone)]
enum BoxStage {
    Start,
    Register,
    Equals,
    Selected,
    Copy(BoxOperation),
    Read,
    Ready,
    Pre,
}
#[derive(Clone)]
pub(super) struct BoxTask {
    intent: BoxIntent,
    stage: BoxStage,
    register: i64,
    capture: Option<BoxScanContext>,
    pre: bool,
    allow_rule: bool,
}
impl BoxTask {
    pub fn new(intent: BoxIntent) -> Self {
        let capture = matches!(intent, BoxIntent::Make { .. } | BoxIntent::Insert)
            .then(BoxScanContext::default);
        Self {
            intent,
            stage: BoxStage::Start,
            register: 0,
            capture,
            pre: false,
            allow_rule: false,
        }
    }
    fn operand(allow_rule: bool, capture: bool) -> Self {
        let mut task = Self::new(BoxIntent::Return);
        task.allow_rule = allow_rule;
        task.capture = capture.then(BoxScanContext::default);
        task
    }
    fn read_box(&mut self, code: BoxOperation, adjusted: bool) -> CommandRequest {
        self.stage = BoxStage::Read;
        CommandRequest::ReadBox(BoxExecution::new(
            code,
            self.capture.clone(),
            None,
            true,
            true,
            if adjusted && code == BoxOperation::Hbox {
                Some(GroupKind::AdjustedHbox)
            } else {
                None
            },
        ))
    }
    fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        value: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        let value = value.transpose()?;
        let request = match self.stage {
            BoxStage::Start => match self.intent {
                BoxIntent::Set { .. } | BoxIntent::Insert => {
                    self.stage = BoxStage::Register;
                    eval_scan(ScanRequest::Register(None))
                }
                BoxIntent::Adjust => {
                    self.stage = BoxStage::Pre;
                    eval_scan(ScanRequest::Keyword("pre".into()))
                }
                _ => {
                    self.stage = BoxStage::Selected;
                    eval_scan(ScanRequest::Nonblank)
                }
            },
            BoxStage::Register => {
                self.register = scan(value)?.integer()?;
                if matches!(self.intent, BoxIntent::Insert) {
                    if self.register == 255 {
                        return Err(Error::value("Insertion number 255 is reserved"));
                    }
                    self.stage = BoxStage::Read;
                    CommandRequest::ReadBox(BoxExecution::new(
                        BoxOperation::Vbox,
                        self.capture.clone(),
                        None,
                        false,
                        false,
                        Some(GroupKind::Insert),
                    ))
                } else {
                    self.stage = BoxStage::Equals;
                    eval_scan(ScanRequest::OptionalEquals)
                }
            }
            BoxStage::Equals => {
                scan(value)?;
                self.stage = BoxStage::Selected;
                eval_scan(ScanRequest::Nonblank)
            }
            BoxStage::Pre => {
                self.pre = scan(value)?.flag()?;
                self.stage = BoxStage::Read;
                CommandRequest::ReadBox(BoxExecution::new(
                    BoxOperation::Vbox,
                    None,
                    None,
                    false,
                    false,
                    Some(GroupKind::Insert),
                ))
            }
            BoxStage::Selected => {
                scan(value)?;
                if view.core.input.current.is_command(Cmd::Relax)
                    && view.core.input.current.character() == Some(0)
                {
                    return Ok(FrameStep::Yield(eval_scan(ScanRequest::Nonblank)));
                }
                if self.allow_rule && view.core.input.current.is_command(Cmd::Rule) {
                    let vertical = view.core.input.current.character().unwrap_or(0) != 0;
                    self.stage = BoxStage::Read;
                    return Ok(FrameStep::Yield(CommandRequest::ScanRule(
                        RuleExecution::new(vertical),
                    )));
                }
                let CommandOperand::Box(code) = view.core.input.current.value else {
                    return Err(Error::value("Expected a box expression")
                        .with_issue(crate::core::recovery::Issue::BoxOperand));
                };
                if !view.core.input.current.is_command(Cmd::MakeBox) {
                    return Err(Error::value("Expected a box expression")
                        .with_issue(crate::core::recovery::Issue::BoxOperand));
                }
                if view.env.reuse_stateful.get()
                    && !matches!(
                        code,
                        BoxOperation::Hbox
                            | BoxOperation::Vbox
                            | BoxOperation::Vtop
                            | BoxOperation::Box
                            | BoxOperation::Copy
                    )
                {
                    view.env
                        .reuse_rejected_by
                        .set(Some("unsupported-box-operand"));
                    view.env.reject_reuse_observation();
                }
                if let Some(capture) = &mut self.capture {
                    capture.fresh_hbox = code == BoxOperation::Hbox;
                }
                match code {
                    BoxOperation::Box | BoxOperation::Copy => {
                        self.stage = BoxStage::Copy(code);
                        eval_scan(ScanRequest::Register(None))
                    }
                    BoxOperation::LastBox => {
                        let pointer = view.lastbox()?;
                        return self.finish(view, pointer);
                    }
                    BoxOperation::Vsplit => {
                        let pointer = view.split_box()?;
                        return self.finish(view, pointer);
                    }
                    _ => self.read_box(
                        code,
                        matches!(self.intent, BoxIntent::Make { adjusted: true }),
                    ),
                }
            }
            BoxStage::Copy(code) => {
                let register = scan(value)?.integer()?;
                let pointer = view.take_register_box(register, code == BoxOperation::Copy)?;
                return self.finish(view, pointer);
            }
            BoxStage::Read => {
                let Some(CommandValue::Box(result)) = value else {
                    return Err(Error::runtime("Invalid completed box result"));
                };
                self.capture = result.capture.clone();
                return self.finish(view, result.pointer);
            }
            BoxStage::Ready => {
                if !matches!(value, Some(CommandValue::Executed(true))) {
                    return Err(Error::runtime("Invalid completed box child result"));
                }
                return Ok(FrameStep::Complete(CommandValue::Executed(true)));
            }
        };
        Ok(FrameStep::Yield(request))
    }
    fn finish(&mut self, view: &mut EngineView<'_>, pointer: i64) -> Result<FrameStep> {
        self.stage = BoxStage::Ready;
        match self.intent {
            BoxIntent::Return => {
                return Ok(FrameStep::Complete(CommandValue::Box(BoxResult {
                    pointer,
                    capture: self.capture.take(),
                    _hold: view.builder.ownership.hold([pointer]),
                })));
            }
            BoxIntent::Make { .. } => {
                if pointer != C::NULL {
                    let adjusted = self.capture.as_ref().is_some_and(|c| c.fresh_hbox)
                        && view.builder.context.mode == ListMode::Vertical;
                    return Ok(FrameStep::Yield(CommandRequest::Append(AppendTask::new(
                        view, pointer, adjusted,
                    ))));
                }
            }
            BoxIntent::Set { global } => {
                view.assign_box(self.register, pointer, global)?;
                if let Some(token) = view.pending_box_assignment() {
                    let source = view.core.input.source();
                    view.core
                        .input
                        .begin_tokens(vec![token].into(), source, None)?;
                }
            }
            BoxIntent::Ship => {
                if pointer != C::NULL {
                    return Ok(FrameStep::Yield(CommandRequest::Ship(ShipTask::explicit(
                        view, pointer,
                    ))));
                }
            }
            BoxIntent::Insert => {
                view.vertical.insert(
                    view.builder,
                    self.register,
                    pointer,
                    self.capture.as_ref().unwrap(),
                )?;
                return Ok(FrameStep::Yield(CommandRequest::Pages(PageTask::new(
                    true, false,
                ))));
            }
            BoxIntent::Adjust => {
                view.vertical.adjust(view.builder, pointer, self.pre)?;
            }
        }
        Ok(FrameStep::Complete(CommandValue::Executed(true)))
    }
}
pub(super) struct Prefix {
    in_box: bool,
    global: bool,
    long: bool,
    outer: bool,
    protected: bool,
    stage: u8,
    pending: Option<Token>,
}
impl Prefix {
    pub fn new(command: Cmd, in_box: bool, global: bool) -> Self {
        Self {
            in_box,
            global: global || command == Cmd::Global,
            long: command == Cmd::LongPrefix,
            outer: command == Cmd::OuterPrefix,
            protected: command == Cmd::ProtectedPrefix,
            stage: 0,
            pending: None,
        }
    }
    fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        let value = returned.transpose()?;
        if self.stage == 0 {
            self.stage = 1;
            return Ok(FrameStep::Yield(expanded()));
        }
        if self.stage == 2 {
            if let Some(token) = self.pending.take() {
                let source = view.core.input.source();
                view.core
                    .input
                    .begin_tokens(vec![token].into(), source, None)?;
            }
            return Ok(FrameStep::Complete(
                value.ok_or_else(|| Error::runtime("Missing prefix result"))?,
            ));
        }
        evaluation_unit(value)?;
        match view.core.input.current.operation() {
            Some(Cmd::Global) => self.global = true,
            Some(Cmd::LongPrefix) => self.long = true,
            Some(Cmd::OuterPrefix) => self.outer = true,
            Some(Cmd::ProtectedPrefix) => self.protected = true,
            Some(Cmd::Relax) if view.core.input.current.character() == Some(0) => {}
            _ if view.core.input.current.is_category(Cat::Spacer) => {}
            _ => {
                self.stage = 2;
                if self.long || self.outer || self.protected {
                    if !view.core.input.current.is_command(Cmd::Def) {
                        return Err(Error::value(
                            "Long or outer prefix requires a macro definition",
                        ));
                    }
                    self.pending = view.core.assignments.after_assignment.take();
                    let CommandOperand::Definition(operation) = view.core.input.current.value
                    else {
                        return Err(Error::value("Invalid definition operation"));
                    };
                    return Ok(FrameStep::Yield(CommandRequest::Evaluation(
                        Request::Expand(ExpansionRequest::Define {
                            operation,
                            global: self.global,
                            long: self.long,
                            outer: self.outer,
                            protected: self.protected,
                        }),
                    )));
                }
                if !view
                    .core
                    .input
                    .current
                    .operation()
                    .is_some_and(|c| traits(c).assignment)
                {
                    return Err(Error::value("Global prefix requires an assignment"));
                }
                return Ok(FrameStep::Yield(CommandRequest::Command {
                    in_box: self.in_box,
                    global: self.global,
                }));
            }
        }
        Ok(FrameStep::Yield(expanded()))
    }
}
struct BoxResult {
    pointer: i64,
    capture: Option<BoxScanContext>,
    _hold: RootHold,
}
enum CommandValue {
    PdfScan(crate::language::pdf_scan_execution::PdfScanValue),
    Executed(bool),
    ShipmentResult(Option<(String, String)>),
    Evaluation(Value),
    Box(BoxResult),
    Math(MathParserValue),
    Alignment { pointer: i64, _hold: RootHold },
}
enum FrameStep {
    Yield(CommandRequest),
    Complete(CommandValue),
}
enum Frame {
    Command {
        in_box: bool,
        global: bool,
        started: bool,
        pending: Option<Token>,
    },
    Evaluation(Evaluation),
    BoxTask(BoxTask),
    ReadBox(BoxExecution),
    Prefix(Prefix),
    Math(MathExecution),
    RenderMath(MathRenderer),
    Operand(Box<OperandTask>),
    Pages(PageTask),
    Ship(Box<ShipTask>),
    Effect(EffectTask),
    Form(Box<FormTask>),
    FormDefinition(FormDefinitionTask),
    PdfOperand(PdfOperandTask),
    PdfScan(Box<PdfScanTask>),
    Navigation(Box<NavigationTask>),
    Image(ImageTask),
    Flush(FlushTask),
    FinishPage(FinishPageTask),
    PlaceAlignment(PlaceAlignmentTask),
    PlaceDisplay(Box<PlaceDisplayTask>),
    Scalar(ScalarTask),
    Discretionary(DiscretionaryExecution),
    Accent(Box<AccentExecution>),
    Unpack(UnpackExecution),
    Rule(RuleTask),
    ScanRule(RuleExecution),
    Font(Box<FontExecution>),
    Paragraph(ParagraphTask),
    Append(AppendTask),
    Splice(SpliceTask),
    Alignment {
        vertical: bool,
        enter: bool,
        execution: Option<Box<AlignmentExecution>>,
        stage: u8,
        transfer: Option<AlignmentTransfer>,
    },
    ParseMath {
        parser: MathParser,
        started: bool,
    },
}
struct StackFrame {
    frame: Frame,
    returned: Option<Result<CommandValue>>,
}
impl StackFrame {
    fn new(request: CommandRequest, core: &EngineCore) -> Result<Self> {
        let frame = match request {
            CommandRequest::Command { in_box, global } => Frame::Command {
                in_box,
                global,
                started: false,
                pending: None,
            },
            CommandRequest::Evaluation(request) => {
                Frame::Evaluation(Evaluation::new(request, core)?)
            }
            CommandRequest::BoxTask(task) => Frame::BoxTask(task),
            CommandRequest::ReadBox(task) => Frame::ReadBox(task),
            CommandRequest::Prefix(prefix) => Frame::Prefix(prefix),
            CommandRequest::Math(task) => Frame::Math(task),
            CommandRequest::RenderMath(renderer) => Frame::RenderMath(renderer),
            CommandRequest::Operand(task) => Frame::Operand(Box::new(task)),
            CommandRequest::Pages(task) => Frame::Pages(task),
            CommandRequest::Ship(task) => Frame::Ship(Box::new(task)),
            CommandRequest::Effect(task) => Frame::Effect(task),
            CommandRequest::Form(task) => Frame::Form(Box::new(task)),
            CommandRequest::FormDefinition(task) => Frame::FormDefinition(task),
            CommandRequest::PdfOperand(task) => Frame::PdfOperand(task),
            CommandRequest::PdfScan(task) => Frame::PdfScan(Box::new(task)),
            CommandRequest::Navigation(task) => Frame::Navigation(Box::new(task)),
            CommandRequest::Image(task) => Frame::Image(task),
            CommandRequest::Flush(task) => Frame::Flush(task),
            CommandRequest::FinishPage(task) => Frame::FinishPage(task),
            CommandRequest::PlaceAlignment(task) => Frame::PlaceAlignment(task),
            CommandRequest::PlaceDisplay(task) => Frame::PlaceDisplay(Box::new(task)),
            CommandRequest::Scalar(task) => Frame::Scalar(task),
            CommandRequest::Discretionary(task) => Frame::Discretionary(task),
            CommandRequest::Accent(task) => Frame::Accent(Box::new(task)),
            CommandRequest::Unpack(task) => Frame::Unpack(task),
            CommandRequest::Rule(task) => Frame::Rule(task),
            CommandRequest::ScanRule(task) => Frame::ScanRule(task),
            CommandRequest::Font(task) => Frame::Font(Box::new(task)),
            CommandRequest::Paragraph(task) => Frame::Paragraph(task),
            CommandRequest::Append(task) => Frame::Append(task),
            CommandRequest::Splice(task) => Frame::Splice(task),
            CommandRequest::Alignment { vertical, enter } => Frame::Alignment {
                vertical,
                enter,
                execution: None,
                stage: 0,
                transfer: None,
            },
            CommandRequest::ParseMath(parser) => Frame::ParseMath {
                parser,
                started: false,
            },
        };
        Ok(Self {
            frame,
            returned: None,
        })
    }
    fn resume(&mut self, view: &mut EngineView<'_>, budget: usize) -> Result<Option<FrameStep>> {
        let step = match &mut self.frame {
            Frame::Command {
                in_box,
                global,
                started,
                pending,
            } => {
                let result = if !*started {
                    let begin = view.start_command(*in_box, *global)?;
                    *started = true;
                    *pending = begin.pending;
                    match begin.step {
                        CommandStep::Yield(request) => return Ok(Some(FrameStep::Yield(request))),
                        CommandStep::Complete(result) => result,
                    }
                } else {
                    let returned = self
                        .returned
                        .take()
                        .ok_or_else(|| Error::runtime("Missing command result"))??;
                    match returned {
                        CommandValue::Executed(result) => result,
                        CommandValue::Evaluation(_) => true,
                        _ => return Err(Error::runtime("Invalid command result")),
                    }
                };
                view.eval().drain_typesetting_events()?;
                if let Some(token) = pending.take() {
                    let source = view.core.input.source();
                    view.core
                        .input
                        .begin_tokens(vec![token].into(), source, None)?;
                }
                FrameStep::Complete(CommandValue::Executed(result))
            }
            Frame::Evaluation(evaluation) => match evaluation.advance(&mut view.eval(), budget) {
                EvaluationProgress::Yielded => return Ok(None),
                EvaluationProgress::Complete(result) => {
                    FrameStep::Complete(CommandValue::Evaluation(result?))
                }
            },
            Frame::Operand(task) => task.resume(view, self.returned.take())?,
            Frame::Pages(task) => return task.resume(view, self.returned.take()),
            Frame::Ship(task) => return task.resume(view, self.returned.take()),
            Frame::Effect(task) => return task.resume(view, self.returned.take()).map(Some),
            Frame::Form(task) => return task.resume(view, self.returned.take()),
            Frame::FormDefinition(task) => {
                return task.resume(view, self.returned.take()).map(Some);
            }
            Frame::PdfScan(task) => return task.resume(view, self.returned.take()).map(Some),
            Frame::Image(task) => return task.resume(view, self.returned.take()).map(Some),
            Frame::Navigation(task) => return task.resume(view, self.returned.take()).map(Some),
            Frame::PdfOperand(task) => return task.resume(view, self.returned.take()).map(Some),
            Frame::Flush(task) => return task.resume(view, self.returned.take()),
            Frame::FinishPage(task) => return task.resume(view, self.returned.take()).map(Some),
            Frame::PlaceAlignment(task) => task.resume(view, self.returned.take())?,
            Frame::PlaceDisplay(task) => task.resume(view, self.returned.take())?,
            Frame::Scalar(task) => task.resume(view, self.returned.take())?,
            Frame::Font(task) => {
                let returned = self.returned.take().map(|result| {
                    result.and_then(|value| match value {
                        CommandValue::Evaluation(Value::Scan(value)) => Ok(FontValue::Scan(value)),
                        CommandValue::Evaluation(Value::Expansion(ExpansionValue::Unit)) => {
                            Ok(FontValue::Unit)
                        }
                        CommandValue::Evaluation(Value::Expansion(ExpansionValue::Tokens(
                            tokens,
                        ))) => crate::language::streams::display_tokens(view, &tokens)
                            .map(FontValue::Text),
                        _ => Err(Error::runtime("Invalid font child result")),
                    })
                });
                match task.resume(view, returned)? {
                    FontStep::Complete(_) => FrameStep::Complete(CommandValue::Executed(true)),
                    FontStep::Yield(action) => FrameStep::Yield(match action {
                        FontAction::Scan(request) => eval_scan(request),
                        FontAction::Text => CommandRequest::Evaluation(Request::Expand(
                            ExpansionRequest::GeneralText { expanded: true },
                        )),
                        FontAction::Destination => CommandRequest::Evaluation(Request::Ready(
                            view.get_r_token()
                                .map(|_| Value::Expansion(ExpansionValue::Unit)),
                        )),
                        FontAction::UseFont(number) => CommandRequest::Evaluation(Request::Ready(
                            view.use_font(number)
                                .map(|_| Value::Expansion(ExpansionValue::Unit)),
                        )),
                    }),
                }
            }
            Frame::Discretionary(task) => {
                let returned = self.returned.take().map(|result| {
                    result.and_then(|value| {
                        let CommandValue::Box(result) = value else {
                            return Err(Error::runtime("Invalid discretionary box result"));
                        };
                        crate::typesetting::box_commands::BoxCommands::finish_discretionary_part(
                            view,
                            result.pointer,
                        )
                    })
                });
                match task.resume(view, returned)? {
                    DiscretionaryStep::ReadPart => {
                        FrameStep::Yield(CommandRequest::ReadBox(BoxExecution::new(
                            BoxOperation::Hbox,
                            None,
                            None,
                            true,
                            false,
                            Some(GroupKind::Disc),
                        )))
                    }
                    DiscretionaryStep::Complete(_) => {
                        FrameStep::Complete(CommandValue::Executed(true))
                    }
                }
            }
            Frame::Accent(task) => {
                let returned = self.returned.take().map(|result| {
                    result.and_then(|value| match value {
                        CommandValue::Evaluation(Value::Scan(value)) => {
                            Ok(AccentValue::Scan(value))
                        }
                        CommandValue::Executed(_) => Ok(AccentValue::Executed),
                        _ => Err(Error::runtime("Invalid accent service result")),
                    })
                });
                match task.resume(view, returned)? {
                    AccentStep::Complete => FrameStep::Complete(CommandValue::Executed(true)),
                    AccentStep::Yield(AccentAction::Scan(request)) => {
                        FrameStep::Yield(eval_scan(request))
                    }
                    AccentStep::Yield(AccentAction::ExecuteToken) => {
                        FrameStep::Yield(CommandRequest::Command {
                            in_box: !view.builder.context_stack.is_empty(),
                            global: false,
                        })
                    }
                }
            }
            Frame::Unpack(task) => {
                let returned = self.returned.take().map(|result| {
                    result.and_then(|value| match value {
                        CommandValue::Evaluation(Value::Scan(value)) => Ok(value),
                        CommandValue::Executed(true) => Ok(ScanValue::Unit),
                        _ => Err(Error::runtime("Invalid unpack service result")),
                    })
                });
                match task.resume(view, returned)? {
                    UnpackStep::Complete => FrameStep::Complete(CommandValue::Executed(true)),
                    UnpackStep::Yield(UnpackAction::Scan(request)) => {
                        FrameStep::Yield(eval_scan(request))
                    }
                    UnpackStep::Yield(UnpackAction::StartParagraph) => FrameStep::Yield(
                        CommandRequest::Paragraph(ParagraphTask::start(false, false)),
                    ),
                    UnpackStep::Yield(UnpackAction::FinishParagraph) => {
                        FrameStep::Yield(CommandRequest::Paragraph(ParagraphTask::finish(false)))
                    }
                }
            }
            Frame::Rule(task) => task.resume(view, self.returned.take())?,
            Frame::ScanRule(task) => {
                let returned = self
                    .returned
                    .take()
                    .map(|value| value.and_then(|value| scan(Some(value))));
                match task.resume(view, returned)? {
                    RuleStep::Scan(request) => FrameStep::Yield(eval_scan(request)),
                    RuleStep::Complete(pointer) => {
                        FrameStep::Complete(CommandValue::Box(BoxResult {
                            pointer,
                            capture: None,
                            _hold: view.builder.ownership.hold([pointer]),
                        }))
                    }
                }
            }
            Frame::Paragraph(task) => task.resume(view, self.returned.take())?,
            Frame::Append(task) => task.resume(view, self.returned.take())?,
            Frame::Splice(task) => task.resume(view, self.returned.take())?,
            Frame::BoxTask(task) => task.resume(view, self.returned.take())?,
            Frame::Prefix(prefix) => prefix.resume(view, self.returned.take())?,
            Frame::Math(execution) => {
                let returned = self.returned.take().map(|value| {
                    value.and_then(|value| match value {
                        CommandValue::Math(MathParserValue::Read { atoms, .. }) => {
                            Ok(MathValue::Atoms(atoms))
                        }
                        CommandValue::Math(MathParserValue::Rendered(nodes)) => {
                            Ok(MathValue::Rendered(nodes))
                        }
                        CommandValue::Evaluation(_) | CommandValue::Executed(true) => {
                            Ok(MathValue::Unit)
                        }
                        _ => Err(Error::runtime("Invalid math lifecycle result")),
                    })
                });
                match execution.resume(view, returned)? {
                    MathStep::Complete => FrameStep::Complete(CommandValue::Executed(true)),
                    MathStep::Yield(MathAction::Parse) => {
                        FrameStep::Yield(CommandRequest::ParseMath(MathParser::default()))
                    }
                    MathStep::Yield(MathAction::Render {
                        atoms,
                        style,
                        penalties,
                    }) => FrameStep::Yield(render_math(atoms, style, penalties)?),
                    MathStep::Yield(MathAction::ExpandedToken) => FrameStep::Yield(expanded()),
                    MathStep::Yield(MathAction::ParagraphBeforeDisplay) => {
                        FrameStep::Yield(CommandRequest::Paragraph(ParagraphTask::finish(true)))
                    }
                    MathStep::Yield(MathAction::PlaceDisplay(pointers)) => FrameStep::Yield(
                        CommandRequest::PlaceDisplay(PlaceDisplayTask::new(pointers)),
                    ),
                    MathStep::Yield(MathAction::PlaceAlignment(head)) => FrameStep::Yield(
                        CommandRequest::PlaceAlignment(PlaceAlignmentTask::new(head)),
                    ),
                    MathStep::Yield(MathAction::OptionalSpace) => {
                        FrameStep::Yield(eval_scan(ScanRequest::OptionalSpace))
                    }
                }
            }
            Frame::RenderMath(renderer) => {
                match view.with_layout(|layout, typesetting| {
                    renderer.advance_render(
                        &mut MathContext {
                            typesetting,
                            layout,
                        },
                        budget,
                    )
                })? {
                    Some(result) => FrameStep::Complete(CommandValue::Math(
                        MathParserValue::Rendered(result.nodes),
                    )),
                    None => return Ok(None),
                }
            }
            Frame::ParseMath { parser, started } => {
                if !*started {
                    view.with_math_parser(|context, _| parser.start(context, MathStop::Shift))?;
                    *started = true;
                }
                let mut box_hold = None;
                let returned = self
                    .returned
                    .take()
                    .map(|value| {
                        value.and_then(|value| match value {
                            CommandValue::Math(value) => Ok(value),
                            CommandValue::Evaluation(Value::Expansion(ExpansionValue::Unit)) => {
                                Ok(MathParserValue::Unit)
                            }
                            CommandValue::Evaluation(Value::Scan(value)) => {
                                Ok(MathParserValue::Scan(value))
                            }
                            CommandValue::Executed(value) => Ok(MathParserValue::Executed(value)),
                            CommandValue::Alignment { pointer, _hold } => {
                                box_hold = Some(_hold);
                                Ok(MathParserValue::Alignment(pointer))
                            }
                            CommandValue::Box(result) => {
                                box_hold = Some(result._hold);
                                Ok(MathParserValue::Box(result.pointer))
                            }
                            _ => Err(Error::runtime("Invalid math parser child result")),
                        })
                    })
                    .transpose();
                let returned = match returned {
                    Ok(value) => value,
                    Err(error) => {
                        view.with_math_parser(|context, _| {
                            parser.abort(context);
                            Ok(())
                        })?;
                        return Err(error);
                    }
                };
                match view.with_math_parser(|context, services| {
                    parser.resume(context, services, returned)
                })? {
                    MathParserStep::Complete(value) => {
                        FrameStep::Complete(CommandValue::Math(value))
                    }
                    MathParserStep::Yield(action) => match action {
                        MathParserAction::ExpandedToken => FrameStep::Yield(expanded()),
                        MathParserAction::Scan(request) => FrameStep::Yield(eval_scan(request)),
                        MathParserAction::BoxOperand => FrameStep::Yield(CommandRequest::BoxTask(
                            BoxTask::new(BoxIntent::Return),
                        )),
                        MathParserAction::ReadBox { operation, group } => {
                            FrameStep::Yield(CommandRequest::ReadBox(BoxExecution::new(
                                operation,
                                None,
                                None,
                                true,
                                true,
                                Some(group),
                            )))
                        }
                        MathParserAction::ExecuteToken { in_box } => {
                            FrameStep::Yield(CommandRequest::Command {
                                in_box,
                                global: false,
                            })
                        }
                        MathParserAction::ReadAlignment { vertical } => {
                            FrameStep::Yield(CommandRequest::Alignment {
                                vertical,
                                enter: false,
                            })
                        }
                        MathParserAction::RenderEquation { atoms, style } => {
                            FrameStep::Yield(render_math(atoms, style, false)?)
                        }
                    },
                }
            }
            Frame::Alignment {
                vertical,
                enter,
                execution,
                stage,
                transfer,
            } => {
                if *stage == 3 {
                    let result = self
                        .returned
                        .take()
                        .ok_or_else(|| Error::runtime("Missing alignment transfer result"))?;
                    let mut task = execution.take().unwrap();
                    task.alignment
                        .held
                        .truncate(transfer.take().unwrap().held_start);
                    *view.alignments = task.alignment;
                    view.core.alignment_depth = view.alignments.depth;
                    if !matches!(result?, CommandValue::Executed(true)) {
                        return Err(Error::runtime("Invalid alignment transfer result"));
                    }
                    return Ok(Some(FrameStep::Complete(CommandValue::Executed(true))));
                }
                if *stage == 0 {
                    *stage = 2;
                    if *enter {
                        *stage = 1;
                        return Ok(Some(FrameStep::Yield(CommandRequest::Paragraph(
                            if *vertical {
                                ParagraphTask::start(false, false)
                            } else {
                                ParagraphTask::finish(false)
                            },
                        ))));
                    }
                }
                if *stage == 1 {
                    let result = self
                        .returned
                        .take()
                        .ok_or_else(|| Error::runtime("Missing alignment preparation result"))??;
                    if !matches!(result, CommandValue::Executed(true)) {
                        return Err(Error::runtime("Invalid alignment preparation result"));
                    }
                    if !*vertical && view.builder.context.mode != ListMode::Vertical {
                        return Err(Error::value("Horizontal alignment requires vertical mode")
                            .with_issue(crate::core::recovery::Issue::Alignment));
                    }
                    *stage = 2;
                }
                if execution.is_none() {
                    let alignment = std::mem::take(view.alignments);
                    view.core.alignment_depth = alignment.depth;
                    *execution = Some(Box::new(AlignmentExecution::new(alignment, *vertical)));
                }
                let mut box_hold = None;
                let returned = self.returned.take().map(|value| {
                    value.and_then(|value| match value {
                        CommandValue::Executed(value) => Ok(AlignmentValue::Executed(value)),
                        CommandValue::Evaluation(Value::Scan(value)) => {
                            Ok(AlignmentValue::Scan(value))
                        }
                        CommandValue::Evaluation(Value::Expansion(
                            ExpansionValue::Unit | ExpansionValue::Flag(_),
                        )) => Ok(AlignmentValue::Unit),
                        CommandValue::Box(result) => {
                            box_hold = Some(result._hold);
                            Ok(AlignmentValue::Box {
                                pointer: result.pointer,
                                capture: result.capture.unwrap_or_default(),
                            })
                        }
                        _ => Err(Error::runtime("Invalid alignment service result")),
                    })
                });
                let result = execution.as_mut().unwrap().resume(view, returned);
                let result = match result {
                    Ok(AlignmentStep::Yield(action)) => {
                        return Ok(match action {
                            AlignmentAction::Boundary => {
                                self.returned = Some(Ok(CommandValue::Evaluation(
                                    Value::Expansion(ExpansionValue::Unit),
                                )));
                                None
                            }
                            AlignmentAction::RawToken => {
                                self.returned = Some(view.raw_token().map(|()| {
                                    CommandValue::Evaluation(Value::Expansion(ExpansionValue::Unit))
                                }));
                                None
                            }
                            AlignmentAction::ExpandedToken { protected } => {
                                Some(FrameStep::Yield(CommandRequest::Evaluation(
                                    Request::Expand(ExpansionRequest::XToken { protected }),
                                )))
                            }
                            AlignmentAction::ExpandCurrent => {
                                Some(FrameStep::Yield(CommandRequest::Evaluation(
                                    Request::Expand(ExpansionRequest::Current),
                                )))
                            }
                            AlignmentAction::Scan(request) => {
                                Some(FrameStep::Yield(eval_scan(request)))
                            }
                            AlignmentAction::ExecuteToken => {
                                Some(FrameStep::Yield(CommandRequest::Command {
                                    in_box: true,
                                    global: false,
                                }))
                            }
                            AlignmentAction::Noalign {
                                vertical,
                                initial_depth,
                            } => Some(FrameStep::Yield(CommandRequest::ReadBox(
                                BoxExecution::new(
                                    if vertical {
                                        BoxOperation::Hbox
                                    } else {
                                        BoxOperation::Vbox
                                    },
                                    Some(BoxScanContext::default()),
                                    initial_depth,
                                    true,
                                    false,
                                    Some(GroupKind::NoAlign),
                                ),
                            ))),
                        });
                    }
                    Ok(AlignmentStep::Complete(pointer)) => Ok(pointer),
                    Err(error) => Err(error),
                };
                let mut task = execution.take().unwrap();
                if let Ok(pointer) = result {
                    let hold = view.builder.ownership.hold(view.builder.siblings(pointer)?);
                    if *enter {
                        let attached = task.alignment.prepare_attach(view, *vertical, pointer);
                        match attached {
                            Ok(attached) => {
                                *transfer = Some(attached);
                                *execution = Some(task);
                                *stage = 3;
                            }
                            Err(error) => {
                                *view.alignments = task.alignment;
                                view.core.alignment_depth = view.alignments.depth;
                                return Err(error);
                            }
                        }
                        FrameStep::Yield(CommandRequest::Pages(PageTask::new(true, false)))
                    } else {
                        *view.alignments = task.alignment;
                        view.core.alignment_depth = view.alignments.depth;
                        FrameStep::Complete(CommandValue::Alignment {
                            pointer,
                            _hold: hold,
                        })
                    }
                } else {
                    *view.alignments = task.alignment;
                    view.core.alignment_depth = view.alignments.depth;
                    return Err(result.err().unwrap());
                }
            }
            Frame::ReadBox(execution) => {
                let returned = self.returned.take().map(|value| {
                    value.and_then(|value| match value {
                        CommandValue::Executed(executed) => Ok(BoxValue::Executed(executed)),
                        CommandValue::Evaluation(Value::Scan(value)) => Ok(BoxValue::Scan(value)),
                        CommandValue::Evaluation(Value::Expansion(ExpansionValue::Unit)) => {
                            Ok(BoxValue::Unit)
                        }
                        _ => Err(Error::runtime("Invalid box execution service result")),
                    })
                });
                match execution.resume(view, returned)? {
                    BoxStep::Yield(BoxAction::ExpandedToken) => FrameStep::Yield(expanded()),
                    BoxStep::Yield(BoxAction::Scan(request)) => {
                        FrameStep::Yield(eval_scan(request))
                    }
                    BoxStep::Yield(BoxAction::ExecuteToken) => {
                        FrameStep::Yield(CommandRequest::Command {
                            in_box: true,
                            global: false,
                        })
                    }
                    BoxStep::Complete(pointer) => {
                        FrameStep::Complete(CommandValue::Box(BoxResult {
                            pointer,
                            capture: execution.capture.take(),
                            _hold: view.builder.ownership.hold([pointer]),
                        }))
                    }
                }
            }
        };
        Ok(Some(step))
    }
}
pub(super) struct CommandExecution {
    stack: Vec<StackFrame>,
}
pub(super) enum CommandProgress {
    Yielded,
    Complete(Result<bool>),
}
impl CommandExecution {
    pub(super) fn checkpoint_blocker(&self) -> &'static str {
        for frame in &self.stack {
            match &frame.frame {
                Frame::Command { .. }
                | Frame::BoxTask(_)
                | Frame::ReadBox(_)
                | Frame::Pages(_)
                | Frame::Paragraph(_)
                | Frame::Append(_)
                | Frame::Splice(_) => {}
                Frame::Ship(task) if task.checkpoint_ready() => {}
                Frame::Evaluation(_) => return "evaluation",
                Frame::Prefix(_) => return "prefix",
                Frame::Scalar(_) => return "scalar",
                Frame::Operand(_) => return "operand",
                Frame::FinishPage(_) => return "finish_page",
                Frame::Ship(_) => return "shipment",
                _ => return "other_frame",
            }
        }
        "returned_value"
    }
    pub(super) fn local_literal_prefix(&self) -> bool {
        self.stack.iter().all(|f| match &f.frame {
            Frame::Command {
                in_box: false,
                global: false,
                pending: None,
                ..
            } => true,
            Frame::Paragraph(task) => task.local_literal_prefix(),
            Frame::Pages(_) | Frame::Append(_) | Frame::Splice(_) => true,
            _ => false,
        })
    }
    pub(super) fn checkpoint_holds(&self) -> Option<usize> {
        let mut holds = 0;
        for frame in &self.stack {
            holds += match &frame.frame {
                Frame::Command { .. } | Frame::BoxTask(_) | Frame::ReadBox(_) | Frame::Pages(_) => {
                    0
                }
                Frame::Paragraph(task) => task.checkpoint_holds(),
                Frame::Append(_) | Frame::Splice(_) => 1,
                Frame::Ship(task) if task.checkpoint_ready() => task.checkpoint_holds(),
                _ => return None,
            };
            holds += match &frame.returned {
                None
                | Some(Err(_))
                | Some(Ok(CommandValue::Executed(_)))
                | Some(Ok(CommandValue::ShipmentResult(_))) => 0,
                Some(Ok(CommandValue::Box(_))) => 1,
                _ => return None,
            };
        }
        Some(holds)
    }
    pub(super) fn checkpoint_clone(
        &self,
        roots: &crate::core::runtime::OwnershipRoots,
    ) -> Option<Self> {
        self.checkpoint_holds()?;
        let mut warnings = std::collections::BTreeMap::new();
        let mut copy_warning = |source: &std::rc::Rc<std::cell::RefCell<Vec<String>>>| {
            warnings
                .entry(std::rc::Rc::as_ptr(source) as usize)
                .or_insert_with(|| {
                    std::rc::Rc::new(std::cell::RefCell::new(source.borrow().clone()))
                })
                .clone()
        };
        let mut stack = Vec::with_capacity(self.stack.len());
        for source in &self.stack {
            let frame = match &source.frame {
                Frame::Command {
                    in_box,
                    global,
                    started,
                    pending,
                } => Frame::Command {
                    in_box: *in_box,
                    global: *global,
                    started: *started,
                    pending: *pending,
                },
                Frame::BoxTask(task) => Frame::BoxTask(task.clone()),
                Frame::ReadBox(task) => Frame::ReadBox(task.clone()),
                Frame::Paragraph(task) => Frame::Paragraph(task.checkpoint_clone(roots)),
                Frame::Append(task) => Frame::Append(task.checkpoint_clone(roots)),
                Frame::Splice(task) => Frame::Splice(task.checkpoint_clone(roots)),
                Frame::Pages(task) => Frame::Pages(PageTask {
                    consume: task.consume,
                    started: task.started,
                    execution: task.execution.clone(),
                    warnings: copy_warning(&task.warnings),
                }),
                Frame::Ship(task) => Frame::Ship(Box::new(
                    task.checkpoint_clone(roots, copy_warning(task.checkpoint_warnings()))?,
                )),
                _ => return None,
            };
            let returned = match &source.returned {
                None => None,
                Some(Err(error)) => Some(Err(error.clone())),
                Some(Ok(CommandValue::Executed(value))) => Some(Ok(CommandValue::Executed(*value))),
                Some(Ok(CommandValue::ShipmentResult(value))) => {
                    Some(Ok(CommandValue::ShipmentResult(value.clone())))
                }
                Some(Ok(CommandValue::Box(value))) => Some(Ok(CommandValue::Box(BoxResult {
                    pointer: value.pointer,
                    capture: value.capture.clone(),
                    _hold: roots.hold(value._hold.roots.borrow().iter().copied()),
                }))),
                _ => return None,
            };
            stack.push(StackFrame { frame, returned });
        }
        Some(Self { stack })
    }
    pub(super) fn from_started(
        in_box: bool,
        global: bool,
        begin: StartedCommand,
        core: &EngineCore,
    ) -> Result<Self> {
        let mut parent = StackFrame {
            frame: Frame::Command {
                in_box,
                global,
                started: true,
                pending: begin.pending,
            },
            returned: None,
        };
        let child = match begin.step {
            CommandStep::Complete(value) => {
                parent.returned = Some(Ok(CommandValue::Executed(value)));
                None
            }
            CommandStep::Yield(request) => Some(StackFrame::new(request, core)?),
        };
        let mut stack = vec![parent];
        if let Some(child) = child {
            stack.push(child);
        }
        Ok(Self { stack })
    }
    pub fn new(in_box: bool, global: bool, core: &EngineCore) -> Result<Self> {
        Self::from_request(CommandRequest::Command { in_box, global }, core)
    }
    pub fn from_request(request: CommandRequest, core: &EngineCore) -> Result<Self> {
        Ok(Self {
            stack: vec![StackFrame::new(request, core)?],
        })
    }
    pub fn advance(&mut self, view: &mut EngineView<'_>, steps: usize) -> CommandProgress {
        for _ in 0..steps {
            let budget = if steps == usize::MAX { usize::MAX } else { 1 };
            let Some(frame) = self.stack.last_mut() else {
                return CommandProgress::Complete(Err(Error::runtime("Command already completed")));
            };
            match frame.resume(view, budget) {
                Ok(None) => {}
                Ok(Some(FrameStep::Yield(request))) => match StackFrame::new(request, view.core) {
                    Ok(frame) => self.stack.push(frame),
                    Err(error) => return CommandProgress::Complete(Err(error)),
                },
                result => {
                    let value = result.and_then(|step| match step {
                        Some(FrameStep::Complete(value)) => Ok(value),
                        _ => unreachable!(),
                    });
                    self.stack.pop();
                    if let Some(parent) = self.stack.last_mut() {
                        parent.returned = Some(value);
                    } else {
                        return CommandProgress::Complete(value.and_then(|value| match value {
                            CommandValue::Executed(result) => Ok(result),
                            _ => Err(Error::runtime("Invalid final command result")),
                        }));
                    }
                }
            }
        }
        CommandProgress::Yielded
    }
}
fn eval_scan(request: ScanRequest) -> CommandRequest {
    CommandRequest::Evaluation(Request::Scan(request))
}
fn expanded() -> CommandRequest {
    CommandRequest::Evaluation(Request::Expand(ExpansionRequest::XToken {
        protected: false,
    }))
}
fn scan(value: Option<CommandValue>) -> Result<ScanValue> {
    if let Some(CommandValue::Evaluation(Value::Scan(value))) = value {
        Ok(value)
    } else {
        Err(Error::runtime("Invalid command scanner result"))
    }
}
fn evaluation_unit(value: Option<CommandValue>) -> Result<()> {
    if matches!(
        value,
        Some(CommandValue::Evaluation(Value::Expansion(
            ExpansionValue::Unit
        )))
    ) {
        Ok(())
    } else {
        Err(Error::runtime("Invalid command token result"))
    }
}

fn render_math(
    atoms: Vec<Atom>,
    style: crate::core::enums::MathStyle,
    penalties: bool,
) -> Result<CommandRequest> {
    let mut renderer = MathRenderer::default();
    renderer.start_render(atoms, style, penalties, false)?;
    Ok(CommandRequest::RenderMath(renderer))
}

enum OperandKind {
    Shift(u8),
    Leaders(i64),
    Message(bool),
    FontMap(i64),
    Mark(bool),
    Immediate,
    Vskip(i64),
}
pub(super) struct OperandTask {
    kind: OperandKind,
    stage: u8,
    amount: i64,
    pointer: Option<BoxResult>,
    skip_command: Option<Cmd>,
    glue: Option<crate::core::values::GlueSpec>,
}
impl OperandTask {
    fn new(kind: OperandKind) -> Self {
        Self {
            kind,
            stage: 0,
            amount: 0,
            pointer: None,
            skip_command: None,
            glue: None,
        }
    }
    pub fn shift(code: u8) -> Self {
        Self::new(OperandKind::Shift(code))
    }
    pub fn leaders(code: i64) -> Self {
        Self::new(OperandKind::Leaders(code))
    }
    pub fn vskip(code: i64) -> Self {
        Self::new(OperandKind::Vskip(code))
    }
    pub fn message(error: bool) -> Self {
        Self::new(OperandKind::Message(error))
    }
    pub fn immediate() -> Self {
        Self::new(OperandKind::Immediate)
    }
    pub fn font_map(code: i64) -> Self {
        Self::new(OperandKind::FontMap(code))
    }
    pub fn mark(extended: bool) -> Self {
        Self::new(OperandKind::Mark(extended))
    }
    fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        let returned = returned.transpose()?;
        let request = match self.kind {
            OperandKind::Message(error) => {
                if self.stage == 0 {
                    self.stage = 1;
                    CommandRequest::Evaluation(Request::Expand(ExpansionRequest::GeneralText {
                        expanded: true,
                    }))
                } else {
                    let Some(CommandValue::Evaluation(Value::Expansion(ExpansionValue::Tokens(
                        tokens,
                    )))) = returned
                    else {
                        return Err(Error::runtime("Invalid message expansion result"));
                    };
                    let text = crate::language::streams::display_tokens(view, &tokens)?;
                    view.finish_message(text, error)?;
                    return Ok(FrameStep::Complete(CommandValue::Executed(true)));
                }
            }
            OperandKind::Immediate => {
                if self.stage == 0 {
                    self.stage = 1;
                    expanded()
                } else if self.stage == 1 {
                    evaluation_unit(returned)?;
                    if view.core.input.current.is_command(Cmd::OutStream) {
                        self.stage = 2;
                        CommandRequest::Evaluation(Request::Stream(
                            crate::language::stream_execution::StreamRequest::Output {
                                operation: view.core.input.current.character().unwrap_or(0) as u8,
                                immediate: true,
                            },
                        ))
                    } else {
                        if view.core.input.current.is_command(Cmd::PdfExtension)
                            && matches!(&view.core.input.current.value, CommandOperand::Text(name) if ["pdfobj","pdfxform","pdfximage"].contains(&name.as_str()))
                        {
                            let CommandOperand::Text(name) = view.core.input.current.value.clone()
                            else {
                                unreachable!()
                            };
                            if name == "pdfxform" {
                                self.stage = 2;
                                return Ok(FrameStep::Yield(CommandRequest::FormDefinition(
                                    FormDefinitionTask::new(false, true),
                                )));
                            }
                            if name == "pdfximage" {
                                self.stage = 2;
                                return Ok(FrameStep::Yield(CommandRequest::Image(
                                    ImageTask::new(false, true),
                                )));
                            }
                            if crate::language::pdf_operand_execution::PdfOperandExecution::supports(
                                &name,
                            ) {
                                self.stage = 2;
                                return Ok(FrameStep::Yield(CommandRequest::PdfOperand(
                                    PdfOperandTask::new(name, true),
                                )));
                            }
                            crate::language::pdf_extensions::execute(view, &name, true)?;
                        } else {
                            view.back_input()?;
                        }
                        return Ok(FrameStep::Complete(CommandValue::Executed(true)));
                    }
                } else {
                    if !matches!(returned, Some(CommandValue::Executed(true))) {
                        evaluation_unit(returned)?;
                    }
                    return Ok(FrameStep::Complete(CommandValue::Executed(true)));
                }
            }
            OperandKind::FontMap(code) => {
                if self.stage == 0 {
                    self.stage = 1;
                    CommandRequest::Evaluation(Request::Expand(ExpansionRequest::GeneralText {
                        expanded: true,
                    }))
                } else {
                    let Some(CommandValue::Evaluation(Value::Expansion(ExpansionValue::Tokens(
                        tokens,
                    )))) = returned
                    else {
                        return Err(Error::runtime("Invalid font map expansion result"));
                    };
                    let text = crate::language::streams::display_tokens(view, &tokens)?;
                    let directory = view.source_directory();
                    let suppress = view.parameter("pdfsuppresswarningdupmap", 0) > 0;
                    let mut warnings = Vec::new();
                    view.fonts.map_command(
                        &text,
                        code != 0,
                        &directory,
                        suppress,
                        &mut |text| warnings.push(text.to_string()),
                    )?;
                    for warning in warnings {
                        view.warning(&warning);
                    }
                    return Ok(FrameStep::Complete(CommandValue::Executed(true)));
                }
            }
            OperandKind::Mark(extended) => {
                if self.stage == 0 && extended {
                    self.stage = 1;
                    eval_scan(ScanRequest::Register(None))
                } else if self.stage < 2 {
                    if self.stage == 1 {
                        self.amount = scan(returned)?.integer()?;
                    }
                    self.stage = 2;
                    CommandRequest::Evaluation(Request::Expand(ExpansionRequest::GeneralText {
                        expanded: true,
                    }))
                } else {
                    let Some(CommandValue::Evaluation(Value::Expansion(ExpansionValue::Tokens(
                        tokens,
                    )))) = returned
                    else {
                        return Err(Error::runtime("Invalid mark expansion result"));
                    };
                    view.vertical.mark(view.builder, self.amount, tokens)?;
                    return Ok(FrameStep::Complete(CommandValue::Executed(true)));
                }
            }
            OperandKind::Shift(code) => match self.stage {
                0 => {
                    let horizontal = code < 2;
                    let mode = view.builder.context.mode;
                    if (horizontal && !matches!(mode, ListMode::Horizontal | ListMode::Math))
                        || (!horizontal && mode != ListMode::Vertical)
                    {
                        return Err(Error::value("Box shift in the wrong mode")
                            .with_issue(crate::core::recovery::Issue::Mode));
                    }
                    self.stage = 1;
                    eval_scan(ScanRequest::Dimen {
                        mu: false,
                        infinite: false,
                    })
                }
                1 => {
                    self.amount = scan(returned)?.integer()?;
                    self.stage = 2;
                    CommandRequest::BoxTask(BoxTask::operand(false, true))
                }
                2 => {
                    let Some(CommandValue::Box(result)) = returned else {
                        return Err(Error::runtime("Invalid shifted box result"));
                    };
                    self.stage = 3;
                    if result.pointer != C::NULL {
                        view.builder.node_mut(result.pointer)?.shift = if code == 0 || code == 2 {
                            -self.amount
                        } else {
                            self.amount
                        };
                        return Ok(FrameStep::Yield(CommandRequest::Append(AppendTask::new(
                            view,
                            result.pointer,
                            code >= 2 && result.capture.as_ref().is_some_and(|c| c.fresh_hbox),
                        ))));
                    }
                    return Ok(FrameStep::Complete(CommandValue::Executed(true)));
                }
                _ => {
                    if !matches!(returned, Some(CommandValue::Executed(true))) {
                        return Err(Error::runtime("Invalid shifted box append result"));
                    }
                    return Ok(FrameStep::Complete(CommandValue::Executed(true)));
                }
            },
            OperandKind::Vskip(code) => match self.stage {
                0 => {
                    if code == 0 {
                        self.stage = 1;
                        eval_scan(ScanRequest::Glue { mu: false })
                    } else {
                        self.glue = Some(crate::typesetting::box_commands::BoxCommands::skip_spec(
                            view, code,
                        )?);
                        self.stage = 2;
                        CommandRequest::Paragraph(ParagraphTask::finish(false))
                    }
                }
                1 => {
                    let ScanValue::Glue(glue) = scan(returned)? else {
                        return Err(Error::runtime("Invalid vertical skip glue result"));
                    };
                    self.glue = Some(glue);
                    self.stage = 2;
                    CommandRequest::Paragraph(ParagraphTask::finish(false))
                }
                _ => {
                    if !matches!(returned, Some(CommandValue::Executed(true))) {
                        return Err(Error::runtime("Invalid vertical skip paragraph result"));
                    }
                    if view.builder.context.mode != ListMode::Vertical {
                        return Err(Error::value("Vertical skip in horizontal box")
                            .with_issue(crate::core::recovery::Issue::Mode));
                    }
                    let pointer = view.builder.new_glue(
                        self.glue.take().unwrap(),
                        "normal",
                        view.fonts.current,
                    )?;
                    view.builder.tail_append(pointer)?;
                    return Ok(FrameStep::Complete(CommandValue::Executed(true)));
                }
            },
            OperandKind::Leaders(code) => match self.stage {
                0 => {
                    self.stage = 1;
                    CommandRequest::BoxTask(BoxTask::operand(true, false))
                }
                1 => {
                    let Some(CommandValue::Box(result)) = returned else {
                        return Err(Error::runtime("Invalid leaders box result"));
                    };
                    self.pointer = Some(result);
                    self.stage = 2;
                    eval_scan(ScanRequest::Nonblank)
                }
                2 => {
                    scan(returned)?;
                    self.skip_command =
                        Some(view.core.input.current.operation().ok_or_else(|| {
                            Error::value("Leaders not followed by proper glue")
                                .with_issue(crate::core::recovery::Issue::BoxOperand)
                        })?);
                    let skip = view.core.input.current.character().unwrap_or(0);
                    if skip == 0 {
                        self.stage = 3;
                        eval_scan(ScanRequest::Glue { mu: false })
                    } else {
                        let spec =
                            crate::typesetting::box_commands::BoxCommands::skip_spec(view, skip)?;
                        return self.finish_leaders(view, code, spec);
                    }
                }
                _ => {
                    let crate::language::scanning::ScanValue::Glue(spec) = scan(returned)? else {
                        return Err(Error::runtime("Invalid leaders glue result"));
                    };
                    return self.finish_leaders(view, code, spec);
                }
            },
        };
        Ok(FrameStep::Yield(request))
    }
    fn finish_leaders(
        &mut self,
        view: &mut EngineView<'_>,
        code: i64,
        spec: crate::core::values::GlueSpec,
    ) -> Result<FrameStep> {
        let pointer = self.pointer.take().unwrap();
        let command = self.skip_command.unwrap();
        view.with_layout(|_, ctx| {
            crate::typesetting::classic_lists::ClassicLists::leaders(
                ctx,
                ["leaders", "cleaders", "xleaders"][code as usize],
                pointer.pointer,
                command,
                spec,
            )
        })?;
        Ok(FrameStep::Complete(CommandValue::Executed(true)))
    }
}

pub(super) struct PageTask {
    consume: bool,
    started: bool,
    execution: PageExecution,
    warnings: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
}
impl PageTask {
    pub fn new(consume: bool, finish: bool) -> Self {
        Self {
            consume,
            started: false,
            execution: PageExecution::new(finish),
            warnings: Default::default(),
        }
    }
    fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<Option<FrameStep>> {
        if !self.started {
            view.observe();
            self.started = true;
            if self.consume {
                if view.builder.context.mode != ListMode::Vertical
                    || view.builder.context.restricted
                {
                    return Ok(Some(FrameStep::Complete(CommandValue::Executed(true))));
                }
                let head = view.builder.take_list(view.fonts, view.env)?;
                if head == C::NULL {
                    return Ok(Some(FrameStep::Complete(CommandValue::Executed(true))));
                }
                for pointer in view.builder.siblings(head)? {
                    view.builder.nodes.set_link(pointer, C::NULL)?;
                    view.pages.queue.push_back(pointer);
                }
            }
        }
        let mut hold = None;
        let returned = returned.map(|value| {
            value.and_then(|value| match value {
                CommandValue::Executed(true) => Ok(C::NULL),
                CommandValue::Box(result) => {
                    hold = Some(result._hold);
                    Ok(result.pointer)
                }
                _ => Err(Error::runtime("Invalid output routine result")),
            })
        });
        let result = view.with_pages_buffered(&mut self.warnings.borrow_mut(), |p, v, l, c, s| {
            self.execution.resume(p, v, l, c, s, returned)
        });
        if result.is_err() || matches!(result, Ok(PageStep::Complete)) {
            for warning in std::mem::take(&mut *self.warnings.borrow_mut()) {
                view.warning(&warning);
            }
        }
        match result? {
            PageStep::Ship { pointer, built } => Ok(Some(FrameStep::Yield(CommandRequest::Ship(
                ShipTask::page(view, pointer, built, self.warnings.clone()),
            )))),
            PageStep::Continue => Ok(None),
            PageStep::Complete => Ok(Some(FrameStep::Complete(CommandValue::Executed(true)))),
            PageStep::Output(tokens) => {
                view.core.input.begin_tokens(tokens, None, None)?;
                Ok(Some(FrameStep::Yield(CommandRequest::ReadBox(
                    BoxExecution::new(
                        BoxOperation::Vbox,
                        None,
                        None,
                        false,
                        false,
                        Some(GroupKind::Output),
                    ),
                ))))
            }
        }
    }
}

pub(super) struct PlaceAlignmentTask {
    head: i64,
    started: bool,
    hold: Option<RootHold>,
    warnings: Vec<String>,
}
impl PlaceAlignmentTask {
    fn new(head: i64) -> Self {
        Self {
            head,
            started: false,
            hold: None,
            warnings: Vec::new(),
        }
    }
    fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        if !self.started {
            self.started = true;
            self.hold = Some(
                view.builder
                    .ownership
                    .hold(view.builder.siblings(self.head)?),
            );
            let result = view
                .alignments
                .results
                .remove(&self.head)
                .ok_or_else(|| Error::value("Missing display alignment result"))?;
            view.observe();
            let result = view.with_pages_buffered(&mut self.warnings, |_, _, l, c, _| {
                crate::typesetting::math_layout::MathLayout::prepare_display_alignment(
                    l,
                    c,
                    self.head,
                    &result.rows,
                    result.previous_depth,
                )
            });
            if let Err(error) = result {
                for warning in std::mem::take(&mut self.warnings) {
                    view.warning(&warning);
                }
                return Err(error);
            }
            self.hold = None;
            return Ok(FrameStep::Yield(CommandRequest::Pages(PageTask::new(
                true, false,
            ))));
        }
        for warning in std::mem::take(&mut self.warnings) {
            view.warning(&warning);
        }
        let result = returned
            .ok_or_else(|| Error::runtime("Missing display alignment transfer result"))??;
        if !matches!(result, CommandValue::Executed(true)) {
            return Err(Error::runtime("Invalid display alignment transfer result"));
        }
        Ok(FrameStep::Complete(CommandValue::Executed(true)))
    }
}

pub(super) struct PlaceDisplayTask {
    state: Option<MathState>,
    execution: DisplayExecution,
    nodes: Vec<i64>,
    hold: Option<RootHold>,
    warnings: Vec<String>,
}
impl PlaceDisplayTask {
    fn new(pointers: Vec<i64>) -> Self {
        Self {
            state: None,
            nodes: pointers.clone(),
            execution: DisplayExecution::new(pointers),
            hold: None,
            warnings: Vec::new(),
        }
    }
    fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        if self.state.is_none() {
            let state = std::mem::take(view.math);
            let mut roots = state.held.clone();
            roots.extend(self.nodes.iter().copied());
            if let Some((_, equation)) = state.equation_number {
                roots.push(equation);
            }
            self.hold = Some(view.builder.ownership.hold_component(roots));
            self.state = Some(state);
            view.observe();
        }
        let returned = returned.map(|value| {
            value.and_then(|value| match value {
                CommandValue::Executed(true) => Ok(()),
                _ => Err(Error::runtime("Invalid display placement result")),
            })
        });
        let result = view.with_pages_buffered(&mut self.warnings, |_, v, l, c, _| {
            self.execution
                .resume(v, l, c, self.state.as_mut().unwrap(), returned)
        });
        if result.is_err() || matches!(result, Ok(DisplayStep::Complete)) {
            let state = self.state.take().unwrap();
            for warning in std::mem::take(&mut self.warnings) {
                view.warning(&warning);
            }
            *view.math = state;
        }
        match result? {
            DisplayStep::Complete => Ok(FrameStep::Complete(CommandValue::Executed(true))),
            DisplayStep::Yield(DisplayAction::Append(pointer)) => Ok(FrameStep::Yield(
                CommandRequest::Append(AppendTask::new(view, pointer, false)),
            )),
            DisplayStep::Yield(DisplayAction::Splice(pointers)) => Ok(FrameStep::Yield(
                CommandRequest::Splice(SpliceTask::new(view, pointers)),
            )),
            DisplayStep::Yield(DisplayAction::Consume) => Ok(FrameStep::Yield(
                CommandRequest::Pages(PageTask::new(true, false)),
            )),
        }
    }
}

enum ScalarKind {
    Character,
    Kern,
    Penalty,
    Hskip(i64),
}
pub(super) struct ScalarTask {
    kind: ScalarKind,
    stage: u8,
}
impl ScalarTask {
    pub fn character() -> Self {
        Self {
            kind: ScalarKind::Character,
            stage: 0,
        }
    }
    pub fn kern() -> Self {
        Self {
            kind: ScalarKind::Kern,
            stage: 0,
        }
    }
    pub fn penalty() -> Self {
        Self {
            kind: ScalarKind::Penalty,
            stage: 0,
        }
    }
    pub fn hskip(code: i64) -> Self {
        Self {
            kind: ScalarKind::Hskip(code),
            stage: 0,
        }
    }
    fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        let returned = returned.transpose()?;
        if self.stage == 0 {
            self.stage = 1;
            let request = match self.kind {
                ScalarKind::Character => ScanRequest::CharNum,
                ScalarKind::Kern => ScanRequest::Dimen {
                    mu: false,
                    infinite: false,
                },
                ScalarKind::Penalty => ScanRequest::Int,
                ScalarKind::Hskip(code) => {
                    view.start_paragraph(false)?;
                    crate::typesetting::box_commands::BoxCommands::flush(view)?;
                    if code != 0 {
                        let spec =
                            crate::typesetting::box_commands::BoxCommands::skip_spec(view, code)?;
                        return self.finish_glue(view, spec);
                    }
                    ScanRequest::Glue { mu: false }
                }
            };
            return Ok(FrameStep::Yield(eval_scan(request)));
        }
        if self.stage == 2 {
            if !matches!(returned, Some(CommandValue::Executed(true))) {
                return Err(Error::runtime("Invalid penalty page result"));
            }
            return Ok(FrameStep::Complete(CommandValue::Executed(true)));
        }
        let value = scan(returned)?;
        match self.kind {
            ScalarKind::Character => view.append_character(value.integer()? as u32)?,
            ScalarKind::Kern => {
                let pointer = view.builder.new_kern(value.integer()?)?;
                view.builder.node_mut(pointer)?.kern_kind = "explicit".into();
                view.builder.tail_append(pointer)?;
            }
            ScalarKind::Penalty => {
                let penalty = value.integer()?;
                let pointer = view
                    .builder
                    .allocate(crate::typesetting::model::Node::from(NodeFields {
                        node_type: crate::core::enums::NodeKind::Penalty,
                        penalty,
                        origins: vec![view.core.input.source()],
                        ..Default::default()
                    }))?;
                view.builder.tail_append(pointer)?;
                self.stage = 2;
                return Ok(FrameStep::Yield(CommandRequest::Pages(PageTask::new(
                    true, false,
                ))));
            }
            ScalarKind::Hskip(_) => {
                let ScanValue::Glue(spec) = value else {
                    return Err(Error::runtime("Invalid horizontal skip glue result"));
                };
                return self.finish_glue(view, spec);
            }
        }
        Ok(FrameStep::Complete(CommandValue::Executed(true)))
    }
    fn finish_glue(
        &mut self,
        view: &mut EngineView<'_>,
        spec: crate::core::values::GlueSpec,
    ) -> Result<FrameStep> {
        let pointer = view.builder.new_glue(spec, "normal", view.fonts.current)?;
        view.builder.tail_append(pointer)?;
        Ok(FrameStep::Complete(CommandValue::Executed(true)))
    }
}
pub(super) struct RuleTask {
    vertical: bool,
    stage: u8,
}
impl RuleTask {
    pub fn new(vertical: bool) -> Self {
        Self { vertical, stage: 0 }
    }
    fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        let returned = returned.transpose()?;
        if self.stage == 0 {
            self.stage = 1;
            if self.vertical {
                return Ok(FrameStep::Yield(CommandRequest::Paragraph(
                    ParagraphTask::start(false, false),
                )));
            }
            if view.builder.context.restricted && view.builder.context.mode == ListMode::Horizontal
            {
                return Err(
                    Error::value("Horizontal rule in a restricted horizontal box")
                        .with_issue(crate::core::recovery::Issue::Mode),
                );
            }
            if view.builder.context.vertical_paragraph {
                return Ok(FrameStep::Yield(CommandRequest::Paragraph(
                    ParagraphTask::finish(false),
                )));
            }
            if view.builder.context.mode == ListMode::Horizontal {
                view.handle_par()?;
            }
            return self.resume(view, None);
        }
        if self.stage == 1 {
            if let Some(returned) = returned {
                if !matches!(returned, CommandValue::Executed(true)) {
                    return Err(Error::runtime("Invalid rule paragraph result"));
                }
            }
            self.stage = 2;
            return Ok(FrameStep::Yield(CommandRequest::ScanRule(
                RuleExecution::new(self.vertical),
            )));
        }
        let Some(CommandValue::Box(result)) = returned else {
            return Err(Error::runtime("Invalid rule scan result"));
        };
        view.builder.tail_append(result.pointer)?;
        if !self.vertical {
            view.builder.context.prev_depth = None;
        }
        Ok(FrameStep::Complete(CommandValue::Executed(true)))
    }
}
