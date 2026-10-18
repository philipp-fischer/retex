//! Explicit composition stack for scanner, expansion, conditional and assignment continuations.
use super::*;
use crate::core::errors::ErrorKind;
use crate::language::records::QuantityKind;
use crate::language::retained_text::{RetainedStep, RetainedText};
use crate::language::specials::{SpecialExecution, SpecialStep, SpecialValue};
use crate::language::stream_execution::{
    StreamAction, StreamExecution, StreamRequest, StreamStep, StreamValue,
};
use crate::language::{
    assignments::{
        AssignmentAction as AA, AssignmentContext, AssignmentRequest as AR, AssignmentStep,
        AssignmentValue as AV,
    },
    conditionals::{
        ConditionalAction as CA, ConditionalContext, ConditionalRequest as CR, ConditionalStep,
        ConditionalValue as CV,
    },
    expansion::{
        ExpansionAction as EA, ExpansionContext, ExpansionEvent, ExpansionRequest as ER,
        ExpansionStep, ExpansionValue as EV,
    },
    expressions::{
        ExpressionAction as XA, ExpressionExecution, ExpressionStep, ExpressionValue as XV,
    },
    extended::{ExtendedExecution, ExtendedStep},
    font_commands::FontCommands,
    input::{ExpandedInput, InputEvent},
    internal_values::{
        InternalAction as IA, InternalExecution, InternalRequest as IR, InternalResult,
        InternalStep, InternalValueResult as IV,
    },
    scanning::{
        ScanAction as SA, ScanContext, ScanRequest as SR, ScanStep, ScanValue as SV, ValueScanner,
    },
    services::LanguageServices,
};
mod input;
mod internal;
use input::InputExecution;
pub(super) enum Request {
    Expand(ER),
    Scan(SR),
    Conditional(CR),
    Assignment(AR),
    Input,
    Stream(StreamRequest),
    Special(bool),
    RetainedText(Arc<[Token]>),
    Internal(IR),
    Extended(usize),
    Expression(usize),
    Ready(Result<Value>),
}
pub(super) enum Value {
    Expansion(EV),
    Scan(SV),
    Conditional(CV),
    Assignment(AV),
    Internal(InternalResult),
    InternalValue(crate::language::scanning::InternalValue),
}
enum Machine {
    Expansion(Expander),
    Scan(ValueScanner),
    Conditional(Conditionals),
    Assignment(Assignments),
    Input(InputExecution),
    Stream(StreamExecution),
    Special(SpecialExecution),
    RetainedText(RetainedText),
    Internal(InternalExecution),
    Extended(ExtendedExecution),
    Expression(ExpressionExecution, QuantityKind),
    Ready(Option<Result<Value>>),
}
enum Action {
    Expansion(EA),
    Scan(SA),
    Conditional(CA),
    Assignment(AA),
    Evaluation(Request),
}
enum Step {
    Yield(Action),
    Paused,
    Complete(Value),
}
struct Frame {
    machine: Machine,
    returned: Option<Result<Value>>,
}
impl Frame {
    fn new(request: Request, core: &EngineCore) -> Result<Self> {
        let machine = match request {
            Request::Ready(value) => Machine::Ready(Some(value)),
            Request::RetainedText(tokens) => Machine::RetainedText(RetainedText::new(tokens)),
            Request::Special(literal) => Machine::Special(SpecialExecution::new(literal)),
            Request::Input => Machine::Input(InputExecution::new(core)),
            Request::Stream(request) => Machine::Stream(StreamExecution::new(request)),
            Request::Internal(request) => Machine::Internal(InternalExecution::new(request)),
            Request::Extended(code) => {
                Machine::Extended(ExtendedExecution::new(core.extended.clone(), code))
            }
            Request::Expression(code) => {
                let kind = *crate::language::assignments::KINDS
                    .get(code)
                    .ok_or_else(|| Error::value("Invalid expression type"))?;
                Machine::Expression(ExpressionExecution::new(kind, false), kind)
            }
            Request::Scan(request) => {
                let mut scanner = ValueScanner::default();
                scanner.value_reader_connected = true;
                scanner.start(request)?;
                Machine::Scan(scanner)
            }
            Request::Expand(request) => {
                let mut e = Expander::new(&core.input);
                e.primitive_names = core.expansion.primitive_names.clone();
                e.expansion_limit = core.expansion.expansion_limit;
                e.uninterrupted_expansion_limit = core.expansion.uninterrupted_expansion_limit;
                e.expansion_start = core.expansion.expansion_steps;
                e.expansion_depth_limit = core.expansion.expansion_depth_limit;
                e.recovery_depth_limit = core.expansion.recovery_depth_limit;
                e.start(request)?;
                Machine::Expansion(e)
            }
            Request::Conditional(request) => {
                let mut c = Conditionals::default();
                c.stack = core.conditionals.stack.clone();
                c.start(request)?;
                Machine::Conditional(c)
            }
            Request::Assignment(request) => {
                let mut a = Assignments::default();
                a.start(request)?;
                Machine::Assignment(a)
            }
        };
        Ok(Self {
            machine,
            returned: None,
        })
    }
    fn step(&mut self, s: &mut EvalView<'_>, max_operations: usize) -> Result<Step> {
        // Token/scanner events have no live TeX tracing control. Their immutable
        // source spans are still recorded by input, including when retention is
        // disabled; only construction of unused diagnostic events is avoided.
        s.core.input.tracing = s.core.trace.enabled
            || s.core.trace.observer.is_some()
            || s.core.diagnostics.debug_tokens;
        match &mut self.machine {
            Machine::Ready(value) => value
                .take()
                .ok_or_else(|| Error::runtime("Evaluation result already consumed"))?
                .map(Step::Complete),
            Machine::RetainedText(execution) => {
                let returned = self.returned.take().map(|value| {
                    value.and_then(|value| match value {
                        Value::Expansion(EV::Tokens(tokens)) => Ok(tokens),
                        _ => Err(Error::runtime("Invalid retained text service result")),
                    })
                });
                execution.resume(s, returned).map(|step| match step {
                    RetainedStep::Expand => {
                        Step::Yield(Action::Evaluation(Request::Expand(ER::GeneralText {
                            expanded: true,
                        })))
                    }
                    RetainedStep::Complete(tokens) => {
                        Step::Complete(Value::Expansion(EV::Tokens(tokens)))
                    }
                })
            }
            Machine::Special(execution) => {
                let returned = self.returned.take().map(|value| {
                    value.and_then(|value| match value {
                        Value::Scan(value) => Ok(SpecialValue::Scan(value)),
                        Value::Expansion(EV::Tokens(tokens)) => Ok(SpecialValue::Tokens(tokens)),
                        _ => Err(Error::runtime("Invalid special service result")),
                    })
                });
                execution.resume(s, returned).map(|step| match step {
                    SpecialStep::Complete => Step::Complete(Value::Expansion(EV::Unit)),
                    SpecialStep::Scan(request) => {
                        Step::Yield(Action::Evaluation(Request::Scan(request)))
                    }
                    SpecialStep::Text { expanded } => {
                        Step::Yield(Action::Evaluation(Request::Expand(ER::GeneralText {
                            expanded,
                        })))
                    }
                })
            }
            Machine::Input(input) => input.resume(s, self.returned.take()),
            Machine::Stream(execution) => {
                let returned = self.returned.take().map(|value| {
                    value.and_then(|value| match value {
                        Value::Scan(value) => Ok(StreamValue::Scan(value)),
                        Value::Expansion(EV::Unit) => Ok(StreamValue::Unit),
                        Value::Expansion(EV::Tokens(tokens)) => Ok(StreamValue::Tokens(tokens)),
                        _ => Err(Error::runtime("Invalid stream service result")),
                    })
                });
                s.with_streams(|streams, s| execution.resume(streams, s, returned))
                    .map(|step| match step {
                        StreamStep::Complete => Step::Complete(Value::Expansion(EV::Unit)),
                        StreamStep::Yield(action) => Step::Yield(match action {
                            StreamAction::RetainedText(tokens) => {
                                Action::Evaluation(Request::RetainedText(tokens))
                            }
                            StreamAction::Scan(request) => {
                                Action::Evaluation(Request::Scan(request))
                            }
                            StreamAction::GeneralText { expanded } => {
                                Action::Evaluation(Request::Expand(ER::GeneralText { expanded }))
                            }
                            StreamAction::RawToken => {
                                let result = s.raw_token().map(|_| Value::Expansion(EV::Unit));
                                Action::Evaluation(Request::Ready(result))
                            }
                        }),
                    })
            }
            Machine::Internal(execution) => internal::read(execution, s, self.returned.take()),
            Machine::Extended(execution) => internal::extended(execution, s, self.returned.take()),
            Machine::Expression(execution, kind) => {
                internal::expression(execution, *kind, s, self.returned.take())
            }
            Machine::Scan(scanner) => {
                let returned = self.returned.take().map(|v| {
                    v.and_then(|v| match v {
                        Value::Scan(v) => Ok(v),
                        Value::Expansion(EV::Unit) => Ok(SV::Unit),
                        Value::Internal(InternalResult::Quantity(value)) => Ok(SV::Internal(value)),
                        _ => Err(Error::runtime("Invalid scanner service result")),
                    })
                });
                let mut context = ScanContext {
                    input: &mut s.core.input,
                    env: s.env,
                    fonts: s.fonts,
                };
                scanner
                    .resume(&mut context, returned)
                    .map(|step| match step {
                        ScanStep::Yield(action) => Step::Yield(Action::Scan(action)),
                        ScanStep::Complete(value) => Step::Complete(Value::Scan(value)),
                    })
            }
            Machine::Expansion(e) => {
                e.expansion_steps = s.core.expansion.expansion_steps;
                e.expansion_depth = s.core.expansion.expansion_depth;
                e.csname_depth = s.core.expansion.csname_depth;
                let font_name = |n| FontCommands::name(s.fonts, n);
                let mut context = ExpansionContext {
                    input: &mut s.core.input,
                    env: s.env,
                    font_name: &font_name,
                };
                let returned = self
                    .returned
                    .take()
                    .map(|v| {
                        v.and_then(|v| match v {
                            Value::Expansion(v) => Ok(v),
                            Value::Scan(v) => Ok(EV::Scan(v)),
                            Value::Internal(InternalResult::Text(value)) => Ok(value),
                            Value::Conditional(CV::Unit) => Ok(EV::Unit),
                            _ => Err(Error::runtime("Invalid expansion service result")),
                        })
                    })
                    .transpose();
                let result = match returned {
                    Ok(value) => e.resume_bounded(&mut context, value, max_operations),
                    Err(error) => {
                        e.abort(&mut context);
                        Err(error)
                    }
                };
                s.core.expansion.expansion_steps = e.expansion_steps;
                s.core.expansion.expansion_depth = e.expansion_depth;
                s.core.expansion.csname_depth = e.csname_depth;
                // Keep event ordering identical to uninterrupted execution:
                // logical expansion/service boundaries, not host slices,
                // determine when buffered expansion and input events drain.
                if matches!(result, Ok(ExpansionStep::Paused)) {
                    return Ok(Step::Paused);
                }
                let events = std::mem::take(&mut e.events);
                for event in events {
                    let kind = match &event {
                        ExpansionEvent::Expansion { .. } => "expansion",
                        ExpansionEvent::Conversion { .. } => "conversion",
                        ExpansionEvent::Definition { .. } => "definition",
                    };
                    if s.wants_event(kind) {
                        match event{ExpansionEvent::Expansion{name,replacement,definition,call}=>s.event("expansion",json!({"name":name,"replacement":replacement,"definition":definition,"call":call})),ExpansionEvent::Conversion{operation,output_tokens,source}=>s.event("conversion",json!({"operation":(["number","romannumeral","string","meaning","fontname","eTeXrevision","pdftexrevision","expanded","jobname"].get(operation as usize).copied().unwrap_or("unknown")),"output_tokens":output_tokens,"source":source})),ExpansionEvent::Definition{name,expanded,global,long,outer,source}=>s.event("definition",json!({"name":name,"expanded":expanded,"global_assignment":global,"long":long,"outer":outer,"source":source}))}
                    }
                }
                result.map(|step| match step {
                    ExpansionStep::Yield(action) => Step::Yield(Action::Expansion(action)),
                    ExpansionStep::Paused => Step::Paused,
                    ExpansionStep::Complete(value) => Step::Complete(Value::Expansion(value)),
                })
            }
            Machine::Conditional(c) => {
                c.stack = s.core.conditionals.stack.clone();
                let returned = self
                    .returned
                    .take()
                    .map(|v| {
                        v.and_then(|v| match v {
                            Value::Conditional(v) => Ok(v),
                            Value::Expansion(EV::Unit) => Ok(CV::Unit),
                            Value::Expansion(EV::Flag(v)) => Ok(CV::Flag(v)),
                            Value::Scan(v) => Ok(CV::Scan(v)),
                            _ => Err(Error::runtime("Invalid conditional service result")),
                        })
                    })
                    .transpose()?;
                let mut context = ConditionalContext {
                    input: &mut s.core.input,
                    env: s.env,
                    builder: s.builder,
                    fonts: s.fonts,
                    csname_depth: s.core.expansion.csname_depth,
                };
                let result = c.resume(&mut context, returned);
                s.core.conditionals.stack = c.stack.clone();
                for event in std::mem::take(&mut c.events) {
                    if s.wants_event("conditional") {
                        s.event("conditional",json!({"test":crate::language::conditionals::TESTS[event.test],"result":event.result,"depth":event.depth,"source":event.source}));
                    }
                }
                result.map(|step| match step {
                    ConditionalStep::Yield(action) => Step::Yield(Action::Conditional(action)),
                    ConditionalStep::Complete(value) => Step::Complete(Value::Conditional(value)),
                })
            }
            Machine::Assignment(a) => {
                let returned = self
                    .returned
                    .take()
                    .map(|v| {
                        v.and_then(|v| match v {
                            Value::Assignment(v) => Ok(v),
                            Value::Scan(v) => Ok(AV::Scan(v)),
                            Value::Expansion(EV::Tokens(tokens)) => Ok(AV::Tokens(tokens)),
                            Value::Expansion(EV::Unit) => Ok(AV::Unit),
                            _ => Err(Error::runtime("Invalid assignment service result")),
                        })
                    })
                    .transpose()?;
                let interaction = s.core.interaction;
                let mut warn = |message: &str| s.core.diagnostics.warning(message, interaction);
                let mut typesetting = TypesettingContext {
                    builder: s.builder,
                    fonts: s.fonts,
                    env: s.env,
                    warn: &mut warn,
                };
                let mut context = AssignmentContext {
                    typesetting: &mut typesetting,
                    input: &mut s.core.input,
                };
                let result = a.resume(&mut context, returned);
                for e in std::mem::take(&mut a.events) {
                    if s.wants_event("assignment") {
                        s.event("assignment",json!({"key":e.key,"value":e.value,"global_assignment":e.global,"source":e.source}));
                    }
                }
                result.map(|step| match step {
                    AssignmentStep::Yield(action) => Step::Yield(Action::Assignment(action)),
                    AssignmentStep::Complete(value) => Step::Complete(Value::Assignment(value)),
                })
            }
        }
    }
}
/// Owned composition stack; synchronous and cooperative execution use the
/// same scanner/expansion/assignment machinery and service dispatcher.
pub(super) struct Evaluation {
    #[cfg(feature = "latency-diagnostics")]
    expansion_packet: Option<(String, u64, usize, usize)>,
    #[cfg(feature = "latency-diagnostics")]
    packet_prepared: Option<bool>,
    #[cfg(feature = "latency-diagnostics")]
    packet_checked: bool,
    stack: Vec<Frame>,
}
pub(super) enum EvaluationProgress {
    Yielded,
    Complete(Result<Value>),
}
impl Evaluation {
    pub(super) fn new(request: Request, core: &EngineCore) -> Result<Self> {
        #[cfg(feature = "latency-diagnostics")]
        let packet_prepared = match &request {
            Request::Expand(ER::XToken { .. }) => Some(false),
            Request::Expand(ER::PreparedXToken { .. }) => Some(true),
            _ => None,
        };
        Ok(Self {
            #[cfg(feature = "latency-diagnostics")]
            expansion_packet: None,
            #[cfg(feature = "latency-diagnostics")]
            packet_prepared,
            #[cfg(feature = "latency-diagnostics")]
            packet_checked: false,
            stack: vec![Frame::new(request, core)?],
        })
    }
    pub(super) fn advance(
        &mut self,
        view: &mut EvalView<'_>,
        max_steps: usize,
    ) -> EvaluationProgress {
        #[cfg(feature = "latency-diagnostics")]
        self.begin_expansion_packet(view.core, view.env);
        for _ in 0..max_steps {
            let Some(frame) = self.stack.last_mut() else {
                #[cfg(feature = "latency-diagnostics")]
                self.finish_expansion_packet(view.core, view.env);
                return EvaluationProgress::Complete(Err(Error::runtime(
                    "Evaluation already completed",
                )));
            };
            let operations = if max_steps == usize::MAX {
                usize::MAX
            } else {
                64
            };
            let outcome = frame.step(view, operations);
            if matches!(outcome, Ok(Step::Paused)) {
                continue;
            }
            let outcome = view.drain_events().and(outcome);
            match outcome {
                Ok(Step::Yield(action)) => {
                    let request = view
                        .service(action)
                        .unwrap_or_else(|error| Request::Ready(Err(error)));
                    match Frame::new(request, view.core) {
                        Ok(frame) => self.stack.push(frame),
                        Err(error) => {
                            #[cfg(feature = "latency-diagnostics")]
                            self.finish_expansion_packet(view.core, view.env);
                            return EvaluationProgress::Complete(Err(error));
                        }
                    }
                }
                result => {
                    let result = result.and_then(|step| {
                        if let Step::Complete(value) = step {
                            Ok(value)
                        } else {
                            unreachable!()
                        }
                    });
                    self.stack.pop();
                    if let Some(parent) = self.stack.last_mut() {
                        parent.returned = Some(result);
                    } else {
                        #[cfg(feature = "latency-diagnostics")]
                        self.finish_expansion_packet(view.core, view.env);
                        return EvaluationProgress::Complete(result);
                    }
                }
            }
        }
        EvaluationProgress::Yielded
    }
    #[cfg(feature = "latency-diagnostics")]
    fn begin_expansion_packet(&mut self, core: &EngineCore, env: &mut super::Environment) {
        if self.packet_checked {
            return;
        }
        self.packet_checked = true;
        if !crate::latency_diagnostics::enabled()
            || self.packet_prepared.is_none()
            || env.expansion_packet_probe.get().is_some()
            || env.reuse_expansions.get().is_some()
            || core.conditionals.stack.len() > 64
        {
            return;
        }
        // This selected boundary is knowable without advancing any scanner:
        // the next pending token names a substantial macro. Physical input,
        // literal tokens and cheap helpers do not acquire per-token timers.
        let cs = if self.packet_prepared == Some(true) {
            Some(core.input.current.control_sequence)
        } else {
            core.input
                .cursor
                .token_list
                .as_ref()
                .and_then(|tokens| tokens.get(core.input.cursor.token_index))
                .and_then(|t| t.control_sequence_id())
        };
        let Some(cs) = cs else { return };
        let Some(meaning) = env.meaning(cs) else {
            return;
        };
        if !matches!(&meaning.value,crate::language::records::CommandOperand::Macro(definition) if definition.body.len()>=64)
        {
            return;
        }
        let Some(start) = crate::latency_diagnostics::execution_clock() else {
            return;
        };
        let name = env.cs_name(cs).chars().take(256).collect();
        self.expansion_packet = Some((
            name,
            start,
            core.input.token_count,
            core.conditionals.stack.len(),
        ));
        env.expansion_packet_probe.set(Some((0, 0)));
    }
    #[cfg(feature = "latency-diagnostics")]
    fn finish_expansion_packet(&mut self, core: &mut EngineCore, env: &super::Environment) {
        let Some((name, start, tokens, depth)) = self.expansion_packet.take() else {
            return;
        };
        let (macros, operations) = env.expansion_packet_probe.take().unwrap_or((0, 32));
        if macros == 0 {
            return;
        }
        let name = if macros < 16 {
            "@selected_under_16".to_string()
        } else {
            name
        };
        let Some(end) = crate::latency_diagnostics::execution_clock() else {
            return;
        };
        let elapsed = end.saturating_sub(start);
        let same_depth = depth == core.conditionals.stack.len();
        let rows = &mut core.reuse.stats.expansion_packets;
        if let Some(row) = rows.iter_mut().find(|row| {
            row.first_macro == name
                && row.operations == operations
                && row.conditional_depth_unchanged == same_depth
        }) {
            row.calls += 1;
            row.macros += macros;
            row.tokens += core.input.token_count.saturating_sub(tokens) as u64;
            row.active_ns += elapsed;
            row.max_ns = row.max_ns.max(elapsed);
        } else if rows.len() < 32 {
            rows.push(crate::latency_diagnostics::ExpansionPacketMeasurement {
                first_macro: name,
                operations,
                conditional_depth_unchanged: same_depth,
                calls: 1,
                macros,
                tokens: core.input.token_count.saturating_sub(tokens) as u64,
                active_ns: elapsed,
                max_ns: elapsed,
            });
        } else {
            core.reuse.stats.expansion_packets_dropped += 1;
        }
    }
}
impl EvalView<'_> {
    fn wants_event(&mut self, kind: &str) -> bool {
        let needed = (self.core.trace.enabled
            && self.core.trace.events.len() < self.core.trace.limit)
            || self.core.trace.observer.is_some()
            || (self.core.interaction != 0
                && crate::diagnostics::messages::TEX_TRACE_CONTROLS.iter().any(
                    |(event, control)| {
                        *event == kind && InputProcessor::parameter(self.env, control, 0) != 0
                    },
                ));
        if !needed && self.core.trace.enabled {
            // The observer/live trace paths require fields even after the
            // retention budget fills. Otherwise count this logical event
            // without serializing a payload that the recorder would discard.
            self.core.trace.dropped += 1;
        }
        needed
    }
    pub(super) fn evaluate(&mut self, request: Request) -> Result<Value> {
        stacker::maybe_grow(256 * 1024, 2 * 1024 * 1024, || self.evaluate_inner(request))
    }
    pub(super) fn evaluate_inner(&mut self, request: Request) -> Result<Value> {
        let mut evaluation = Evaluation::new(request, self.core)?;
        loop {
            if let EvaluationProgress::Complete(result) = evaluation.advance(self, usize::MAX) {
                return result;
            }
        }
    }

    fn service(&mut self, action: Action) -> Result<Request> {
        Ok(match action {
            Action::Evaluation(request) => request,
            Action::Scan(SA::ExpandedToken) => Request::Expand(ER::XToken { protected: false }),
            Action::Scan(SA::RawToken) => {
                self.core.input.get_token(self.env)?;
                Request::Ready(Ok(Value::Scan(SV::Unit)))
            }
            Action::Scan(SA::BackInput) => {
                Request::Ready(self.core.input.back_input().map(|_| Value::Scan(SV::Unit)))
            }
            Action::Scan(SA::ResolvePrimitive) => {
                self.core.input.resolve_primitive(self.env)?;
                Request::Ready(Ok(Value::Scan(SV::Unit)))
            }
            Action::Scan(SA::ReadInternal) => Request::Internal(IR::Read),
            Action::Scan(SA::PrepareMagnification) => Request::Ready(
                self.layout
                    .prepare_magnification(self.env)
                    .map(|n| Value::Scan(SV::Integer(n))),
            ),
            Action::Expansion(EA::LocalMaterialBoundary) => {
                self.local_material_boundary();
                Request::Ready(Ok(Value::Expansion(EV::Unit)))
            }
            Action::Expansion(EA::Scan(request)) => Request::Scan(request),
            Action::Expansion(EA::Conditional { code, unless }) => {
                Request::Conditional(CR::Begin { code, unless })
            }
            Action::Expansion(EA::Boundary(code)) => {
                let mut c = std::mem::take(&mut self.core.conditionals);
                let mut context = ConditionalContext {
                    input: &mut self.core.input,
                    env: self.env,
                    builder: self.builder,
                    fonts: self.fonts,
                    csname_depth: self.core.expansion.csname_depth,
                };
                let result = c.boundary(&mut context, code);
                self.core.conditionals = c;
                Request::Ready(result.map(|_| Value::Expansion(EV::Unit)))
            }
            Action::Expansion(EA::The) => Request::Internal(IR::The),
            Action::Expansion(EA::Mark(code)) => {
                let n = if code >= 5 {
                    self.scan_register(Some(32767))?
                } else {
                    0
                };
                let tokens = self
                    .core
                    .marks
                    .get(&(n, (code % 5) as u8))
                    .cloned()
                    .unwrap_or_else(|| Arc::from([]));
                Request::Ready(Ok(Value::Expansion(EV::Tokens(tokens))))
            }
            Action::Expansion(EA::Input) => Request::Input,
            Action::Expansion(EA::PdfConvert(name)) => {
                let mut u = std::mem::take(&mut self.core.pdf_utilities);
                let result = u.convert(self, &name);
                self.core.pdf_utilities = u;
                Request::Ready(result.map(|s| Value::Expansion(EV::Text(s))))
            }
            Action::Conditional(CA::ExpandedToken) => {
                Request::Expand(ER::XToken { protected: false })
            }
            Action::Conditional(CA::Scan(request)) => Request::Scan(request),
            Action::Conditional(CA::CsName) => Request::Expand(ER::CsName { define: false }),
            Action::Conditional(CA::StreamEof(n)) => {
                Request::Ready(Ok(Value::Conditional(CV::Flag(
                    self.core
                        .stream_eof
                        .get(&n)
                        .copied()
                        .unwrap_or_else(|| self.core.streams.at_eof(n)),
                ))))
            }
            Action::Assignment(AA::Scan(request)) => Request::Scan(request),
            Action::Assignment(AA::GeneralText) => {
                Request::Expand(ER::GeneralText { expanded: false })
            }
            Action::Assignment(AA::GetRToken) => {
                self.get_r_token()?;
                Request::Ready(Ok(Value::Assignment(AV::Unit)))
            }
        })
    }
    pub(super) fn event_if_requested(
        &mut self,
        kind: &str,
        fields: impl FnOnce() -> serde_json::Value,
    ) {
        if self.wants_event(kind) {
            self.event(kind, fields());
        }
    }
    pub(super) fn event(&mut self, kind: &str, value: serde_json::Value) {
        let fields = match value {
            serde_json::Value::Object(fields) => fields,
            _ => Default::default(),
        };
        self.core
            .diagnostics
            .tex_event(kind, &fields, self.core.interaction, &|name| {
                InputProcessor::parameter(self.env, name, 0)
            });
        self.core.trace.event(kind, fields)
    }
    pub(super) fn drain_recovery(&mut self) {
        if self.core.input.recovery.pending.is_empty() {
            return;
        }
        // A repaired operation is not an exact computation receipt. Cancel the
        // owning capture and leave reuse disabled for this recovered attempt.
        // Valid receipts from before the error remain independently guarded.
        self.core.reuse.cancel(self.env);
        self.core.config.execution_reuse = false;
        self.core.config.grouped_execution_reuse = false;
        for record in std::mem::take(&mut self.core.input.recovery.pending) {
            if self.core.interaction != 0 {
                crate::resources::host::console_line(true, format!("! {}", record.message));
            }
            self.core.diagnostics.records.push(record);
        }
    }
    pub(super) fn drain_events(&mut self) -> Result<()> {
        self.drain_recovery();
        for event in std::mem::take(&mut self.core.input.events) {
            match event {
                InputEvent::Token {
                    current,
                    name,
                    source,
                    count,
                } => {
                    self.core
                        .diagnostics
                        .debug_token(count, &self.core.input, self.env);
                    if self.wants_event("token") {
                        self.event("token",json!({"command":current.command,"value":current.value,"cs":current.control_sequence,"token":current.token,"name":name,"source":source,"count":count}));
                    }
                }
                InputEvent::ScannerState {
                    status,
                    name,
                    source,
                } => {
                    if self.wants_event("scanner_state") {
                        self.event(
                            "scanner_state",
                            json!({"status":status.as_str(),"name":name,"source":source}),
                        );
                    }
                }
            }
        }
        self.drain_typesetting_events()
    }
    pub(super) fn drain_typesetting_events(&mut self) -> Result<()> {
        use crate::typesetting::{layout::LayoutEvent as L, nodes::BuilderEvent as B};
        for event in std::mem::take(&mut self.builder.events) {
            match event {
                B::MissingCharacter {font,code,source} => {
                    self.core.diagnostics.missing_character(code,&self.fonts.items[font],InputProcessor::parameter(self.env,"tracinglostchars",1),self.core.interaction,source.clone())?;
                    self.event_if_requested("missing_character",||json!({"font":font,"code":code,"source":source}));
                }
                B::Ligature {left,right,result,operation,origins} => self.event_if_requested("ligature",||json!({"left":left,"right":right,"result":result,"operation":operation,"origins":origins})),
                B::ModePush {mode,restricted,depth} => self.event_if_requested("mode_push",||json!({"mode":mode,"restricted":restricted,"depth":depth})),
                B::ModePop {mode,restricted,depth} => self.event_if_requested("mode_pop",||json!({"mode":mode,"restricted":restricted,"depth":depth})),
                B::DebugNode {pointer,kind,character,width} => {if self.core.diagnostics.debug_nodes {self.core.diagnostics.debug_node(&format!("{kind:?} {pointer}"),&format!("character={character:?} width={width}"));}},
            }
        }
        for event in std::mem::take(&mut self.layout.events) {
            match event {
                L::Paragraph(event) => {
                    use crate::typesetting::paragraphs::ParagraphEvent as P;
                    match event {
                        P::Break {line,kind,penalty,hyphenated,node} => self.event_if_requested("paragraph_break",||json!({"line":line,"break_kind":kind.name(),"penalty":penalty,"hyphenated":hyphenated,"node":node})),
                        P::Pass {name,threshold,emergency,final_pass,found,looseness} => self.event_if_requested("paragraph_pass",||json!({"pass_name":name,"threshold":threshold,"emergency_stretch_sp":emergency,"final":final_pass,"found":found,"actual_looseness":looseness})),
                        P::Line {number,pass,state,indent,width,packed} => {
                            if packed.overfull>0 {self.event_if_requested("overfull_hbox",||json!({"natural_width_sp":packed.natural_size,"target_width_sp":width,"excess_sp":packed.overfull}));}
                            let br=state.br.as_ref().expect("completed line has a break");
                            self.event_if_requested("paragraph_line",||json!({"line":number,"pass_name":pass,"break_kind":br.kind.name(),"penalty":br.penalty,"fitness":state.fitness,"badness":state.badness,"total_demerits":state.demerits,"artificial":state.artificial,"indent_sp":indent,"width_sp":width,"natural_width_sp":packed.natural_size,"glue_sign":packed.glue_sign,"glue_order":packed.glue_order,"glue_numerator":packed.glue_numerator,"glue_denominator":packed.glue_denominator}));
                        }
                    }
                }
                L::GlueSet {natural,desired,sign,order,numerator,denominator,badness,overfull} => self.event_if_requested("glue_set",||json!({"natural_size_sp":natural,"target_size_sp":desired,"sign":sign,"order":order,"numerator":numerator,"denominator":denominator,"badness":badness,"overfull_sp":overfull})),
                L::OverfullHbox {natural,target,excess} => self.event_if_requested("overfull_hbox",||json!({"natural_width_sp":natural,"target_width_sp":target,"excess_sp":excess})),
                L::Shipout {page,width,height,x,y} => self.event_if_requested("shipout",||json!({"page":page,"explicit":true,"width_sp":width,"height_sp":height,"origin_x_sp":x,"origin_y_sp":y})),
            }
        }
        Ok(())
    }
}
