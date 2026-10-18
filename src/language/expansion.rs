//! Macro matching and stack-driven token expansion (`language/expansion.py`).
use super::character_tokens::{Token, character_token, token_category};
use super::commands::traits;
use super::conversions::{PrimitiveName, TokenPrinter, roman_numeral};
use super::input::{InputProcessor, ScannerStatus};
use super::records::{CommandOperand, MeaningCommand};
use super::scanning::{ScanRequest, ScanValue};
use super::state::Environment;
use super::tokens::{
    ArgumentTokens, MacroDefinition, MacroParameters, parameter_number, parameter_token,
};
use crate::core::capacities::{exceeded, reached};
use crate::core::constants::Constants as C;
use crate::core::enums::{CharacterCategory as Cat, Command, DefinitionOperation, TokenProtection};
use crate::core::errors::{Error, ErrorKind, Result};
use crate::core::persistence::PersistentComponent;
use crate::core::source::SourceSpan;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;

pub struct ExpansionContext<'a> {
    pub input: &'a mut InputProcessor,
    pub env: &'a mut Environment,
    pub font_name: &'a dyn Fn(usize) -> Result<String>,
}
#[derive(Clone, Debug)]
pub enum ExpansionRequest {
    XToken {
        protected: bool,
    },
    /// Resume an outer token whose raw input has already been consumed.
    PreparedXToken {
        protected: bool,
    },
    Current,
    DefinitionToken,
    CsName {
        define: bool,
    },
    Convert(u8),
    ScanToks {
        macro_def: bool,
        expanded: bool,
    },
    GeneralText {
        expanded: bool,
    },
    GeneralTextSourced {
        expanded: bool,
    },
    Define {
        operation: DefinitionOperation,
        global: bool,
        long: bool,
        outer: bool,
        protected: bool,
    },
}
#[derive(Clone, Debug)]
pub enum ExpansionAction {
    /// One native material handoff before expansion can observe a paragraph.
    LocalMaterialBoundary,
    Scan(ScanRequest),
    Conditional {
        code: usize,
        unless: bool,
    },
    Boundary(usize),
    The,
    Mark(usize),
    Input,
    PdfConvert(String),
}
#[derive(Clone, Debug)]
pub enum ExpansionValue {
    Unit,
    Flag(bool),
    Text(String),
    Tokens(Arc<[Token]>),
    SourcedTokens {
        tokens: Arc<[Token]>,
        sources: Arc<[Option<Arc<SourceSpan>>]>,
    },
    Scan(ScanValue),
    Copied(Option<Arc<[Token]>>),
    Definition {
        body: Arc<[Token]>,
        parameters: MacroParameters,
        sources: Arc<[Option<Arc<SourceSpan>>]>,
    },
}
impl ExpansionValue {
    pub fn flag(self) -> Result<bool> {
        if let Self::Flag(value) = self {
            Ok(value)
        } else {
            Err(Error::value("Unexpected expansion Boolean result"))
        }
    }
    pub fn scan(self) -> Result<ScanValue> {
        if let Self::Scan(value) = self {
            Ok(value)
        } else {
            Err(Error::value("Unexpected expansion scanner result"))
        }
    }
    pub fn tokens(self) -> Result<Arc<[Token]>> {
        if let Self::Tokens(value) = self {
            Ok(value)
        } else {
            Err(Error::value("Unexpected expansion token result"))
        }
    }
}
pub enum ExpansionStep {
    Yield(ExpansionAction),
    /// Internal continuations remain owned by this expander. No service result
    /// is expected when resuming after a cooperative pause.
    Paused,
    Complete(ExpansionValue),
}
enum Request {
    Native(ExpansionRequest),
    External(ExpansionAction),
}
type Resume =
    Box<dyn for<'a> FnOnce(&mut Expander, &mut ExpansionContext<'a>, ExpansionValue) -> Result<()>>;
enum Operation {
    Request(Request),
    Resume(Resume),
}
enum Guard {
    Scanner(ScannerStatus, i64),
    Depth(usize),
    CsName,
}
#[derive(Clone, Debug)]
pub enum ExpansionEvent {
    Expansion {
        name: String,
        replacement: Arc<[Token]>,
        definition: Option<Arc<SourceSpan>>,
        call: Option<Arc<SourceSpan>>,
    },
    Conversion {
        operation: u8,
        output_tokens: usize,
        source: Option<Arc<SourceSpan>>,
    },
    Definition {
        name: String,
        expanded: bool,
        global: bool,
        long: bool,
        outer: bool,
        source: Option<Arc<SourceSpan>>,
    },
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExpansionSnapshot {
    pub primitives: Vec<PrimitiveName>,
}
#[derive(Default)]
pub struct Expander {
    pub csname_depth: usize,
    pub primitive_names: Arc<Vec<PrimitiveName>>,
    pub expansion_limit: Option<usize>,
    /// Interactive watchdog for one expansion request, independent of document length.
    pub uninterrupted_expansion_limit: Option<usize>,
    pub expansion_start: usize,
    pub expansion_depth_limit: Option<usize>,
    pub recovery_depth_limit: Option<usize>,
    pub expansion_steps: usize,
    pub expansion_depth: usize,
    pub events: Vec<ExpansionEvent>,
    operations: Vec<Operation>,
    value: Option<ExpansionValue>,
    awaiting: bool,
    guards: Vec<Guard>,
}
impl PersistentComponent for Expander {
    type State = ExpansionSnapshot;
    fn dump_state(&self) -> Self::State {
        ExpansionSnapshot {
            primitives: self.primitive_names.as_ref().clone(),
        }
    }
    fn restore_state(&mut self, state: Self::State) -> Result<()> {
        self.primitive_names = Arc::new(state.primitives);
        Ok(())
    }
}
pub fn macro_source(
    definition: Option<Arc<SourceSpan>>,
    name: &str,
    call: Option<Arc<SourceSpan>>,
    maximum: usize,
) -> (Option<Arc<SourceSpan>>, Option<Arc<SourceSpan>>) {
    let call = bounded_call(call, maximum);
    let label: Arc<str> = format!("macro:{name}").into();
    (source_for_call(definition, &label, &call), call)
}
fn bounded_call(mut call: Option<Arc<SourceSpan>>, maximum: usize) -> Option<Arc<SourceSpan>> {
    let mut cursor = call.as_ref();
    for _ in 0..maximum {
        let Some(ancestor) = cursor else { return call };
        cursor = ancestor.call_site.as_ref();
    }
    if cursor.is_none() {
        return call;
    }
    let mut ancestors = Vec::new();
    let mut cursor = call.clone();
    while ancestors.len() < maximum {
        let Some(ancestor) = cursor else { break };
        cursor = ancestor.call_site.clone();
        ancestors.push(ancestor);
    }
    if cursor.is_some() {
        #[cfg(feature = "latency-diagnostics")]
        let original_call = call.clone();
        let mut bounded = None;
        for ancestor in ancestors.into_iter().rev() {
            let mut source = (*ancestor).clone();
            source.call_site_truncated |= bounded.is_none();
            source.call_site = bounded;
            #[cfg(feature = "latency-diagnostics")]
            crate::latency_diagnostics::count("bounded_call_source_records", 1);
            bounded = Some(Arc::new(source));
        }
        call = bounded;
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::remember_bounded(original_call.as_ref(), call.as_ref());
    }
    call
}
fn source_for_call(
    definition: Option<Arc<SourceSpan>>,
    label: &Arc<str>,
    call: &Option<Arc<SourceSpan>>,
) -> Option<Arc<SourceSpan>> {
    // Diagnostic upper bound only: intentionally omits exact expansion provenance.
    // Disabled by default and unavailable in ordinary builds.
    #[cfg(feature = "latency-diagnostics")]
    if crate::latency_diagnostics::bypass_provenance() {
        return definition.or_else(|| call.clone());
    }
    let physical = definition.is_some();
    definition.or_else(|| call.clone()).map(|definition| {
        let mut source = (*definition).clone();
        if !physical {
            source.end_byte_column = None;
        }
        source.expansion = Some(label.clone());
        source.call_site = call.clone();
        #[cfg(feature = "latency-diagnostics")]
        {
            crate::latency_diagnostics::count("provenance_source_records", 1);
            crate::latency_diagnostics::count(
                "provenance_record_and_arc_bytes",
                std::mem::size_of::<SourceSpan>() + 2 * std::mem::size_of::<usize>(),
            );
        }
        Arc::new(source)
    })
}
#[cfg(test)]
mod provenance_tests {
    use super::*;
    #[derive(Deserialize)]
    struct Case {
        definition: Option<Arc<SourceSpan>>,
        name: String,
        call: Option<Arc<SourceSpan>>,
        maximum: usize,
        source: Option<Arc<SourceSpan>>,
        bounded_call: Option<Arc<SourceSpan>>,
    }
    #[derive(Deserialize)]
    struct Oracle {
        cases: Vec<Case>,
    }
    #[test]
    fn bounded_macro_provenance_preserves_independent_python_records() {
        let oracle: Oracle = serde_json::from_str(include_str!(
            "../../tests/fixtures/macro-sources-python.json"
        ))
        .unwrap();
        assert_eq!(oracle.cases.len(), 36);
        for case in oracle.cases {
            let (source, call) = macro_source(case.definition, &case.name, case.call, case.maximum);
            assert_eq!(source, case.source);
            assert_eq!(call, case.bounded_call);
        }
    }
}
impl Expander {
    pub(crate) fn checkpoint_clone(&self) -> Option<Self> {
        if self.awaiting || !self.operations.is_empty() || !self.guards.is_empty() {
            return None;
        }
        Some(Self {
            csname_depth: self.csname_depth,
            primitive_names: self.primitive_names.clone(),
            expansion_limit: self.expansion_limit,
            uninterrupted_expansion_limit: self.uninterrupted_expansion_limit,
            expansion_start: self.expansion_start,
            expansion_depth_limit: self.expansion_depth_limit,
            recovery_depth_limit: self.recovery_depth_limit,
            expansion_steps: self.expansion_steps,
            expansion_depth: self.expansion_depth,
            events: self.events.clone(),
            value: self.value.clone(),
            ..Self::default()
        })
    }

    pub fn new(input: &InputProcessor) -> Self {
        Self {
            expansion_depth_limit: input.capacities.expand_depth,
            ..Self::default()
        }
    }
    pub fn register_name(&mut self, command: Command, value: CommandOperand, name: String) {
        if !self
            .primitive_names
            .iter()
            .any(|primitive| primitive.command == command && primitive.value == value)
        {
            Arc::make_mut(&mut self.primitive_names).push(PrimitiveName {
                command,
                value,
                name,
            });
        }
    }
    pub fn define_macro(
        env: &mut Environment,
        cs: i64,
        definition: MacroDefinition,
        global: bool,
    ) -> Arc<MacroDefinition> {
        let definition = Arc::new(definition);
        env.define(
            cs,
            MeaningCommand::Operation(Command::Macro),
            CommandOperand::Macro(definition.clone()),
            global,
        );
        definition
    }
    pub fn start(&mut self, request: ExpansionRequest) -> Result<()> {
        if self.awaiting || !self.operations.is_empty() {
            return Err(Error::value("Expansion operation already active"));
        }
        self.value = None;
        self.operations
            .push(Operation::Request(Request::Native(request)));
        Ok(())
    }
    pub fn resume(
        &mut self,
        context: &mut ExpansionContext<'_>,
        returned: Option<ExpansionValue>,
    ) -> Result<ExpansionStep> {
        self.resume_bounded(context, returned, usize::MAX)
    }
    pub fn resume_bounded(
        &mut self,
        context: &mut ExpansionContext<'_>,
        returned: Option<ExpansionValue>,
        max_operations: usize,
    ) -> Result<ExpansionStep> {
        if self.awaiting {
            self.value =
                Some(returned.ok_or_else(|| Error::value("Missing expansion service result"))?);
            self.awaiting = false;
        } else if returned.is_some() {
            return Err(Error::value("Unrequested expansion service result"));
        }
        let result = (|| {
            let mut operations = 0;
            while !self.operations.is_empty() {
                if operations == max_operations {
                    return Ok(ExpansionStep::Paused);
                }
                operations += 1;
                let operation = self.operations.pop().unwrap();
                match operation {
                    Operation::Request(Request::External(action)) => {
                        self.awaiting = true;
                        return Ok(ExpansionStep::Yield(action));
                    }
                    Operation::Request(Request::Native(request)) => {
                        self.request(context, request)?
                    }
                    Operation::Resume(resume) => {
                        let value = self
                            .value
                            .take()
                            .ok_or_else(|| Error::value("Missing expansion child result"))?;
                        resume(self, context, value)?
                    }
                }
                if context.env.reuse_boundary.get() {
                    return Ok(ExpansionStep::Paused);
                }
            }
            Ok(ExpansionStep::Complete(
                self.value
                    .take()
                    .ok_or_else(|| Error::value("Missing expansion result"))?,
            ))
        })();
        if result.is_err() {
            self.abort(context);
        }
        result
    }
    pub fn abort(&mut self, context: &mut ExpansionContext<'_>) {
        while !self.guards.is_empty() {
            self.leave(context);
        }
        self.operations.clear();
        self.value = None;
        self.awaiting = false;
    }
    fn leave(&mut self, context: &mut ExpansionContext<'_>) {
        match self.guards.pop().unwrap() {
            Guard::Scanner(status, cs) => {
                context.input.scanner_status = status;
                context.input.warning_cs = cs
            }
            Guard::Depth(depth) => self.expansion_depth = depth,
            Guard::CsName => self.csname_depth -= 1,
        }
    }
    fn scanning(
        &mut self,
        context: &mut ExpansionContext<'_>,
        status: ScannerStatus,
        warning: Option<i64>,
    ) {
        self.guards.push(Guard::Scanner(
            context.input.scanner_status,
            context.input.warning_cs,
        ));
        context.input.scanner_status = status;
        if let Some(warning) = warning {
            context.input.warning_cs = warning;
        }
    }
    fn defer(
        &mut self,
        request: Request,
        resume: impl for<'a> FnOnce(&mut Self, &mut ExpansionContext<'a>, ExpansionValue) -> Result<()>
        + 'static,
    ) {
        self.operations.push(Operation::Resume(Box::new(resume)));
        self.operations.push(Operation::Request(request));
    }
    fn native(
        &mut self,
        request: ExpansionRequest,
        resume: impl for<'a> FnOnce(&mut Self, &mut ExpansionContext<'a>, ExpansionValue) -> Result<()>
        + 'static,
    ) {
        self.defer(Request::Native(request), resume);
    }
    fn action(
        &mut self,
        request: ExpansionAction,
        resume: impl for<'a> FnOnce(&mut Self, &mut ExpansionContext<'a>, ExpansionValue) -> Result<()>
        + 'static,
    ) {
        self.defer(Request::External(request), resume);
    }
    fn complete(&mut self, value: ExpansionValue) {
        self.value = Some(value);
    }
    fn enter_expansion(&mut self) -> Result<()> {
        self.count_expansion()?;
        if reached(
            self.expansion_depth_limit.or(self.recovery_depth_limit),
            self.expansion_depth,
        ) {
            return Err(Error::value("Expansion nesting limit exceeded")
                .with_issue(crate::core::recovery::Issue::Capacity));
        }
        self.expansion_depth += 1;
        Ok(())
    }
    fn count_expansion(&mut self) -> Result<()> {
        self.expansion_steps = self.expansion_steps.checked_add(1).ok_or_else(|| {
            Error::value("Expansion limit exceeded")
                .with_issue(crate::core::recovery::Issue::Capacity)
        })?;
        if exceeded(
            self.uninterrupted_expansion_limit,
            self.expansion_steps.saturating_sub(self.expansion_start),
        ) {
            return Err(Error::value(
                "Uninterrupted expansion limit exceeded (possible runaway expansion)",
            )
            .with_issue(crate::core::recovery::Issue::Capacity));
        }
        if exceeded(self.expansion_limit, self.expansion_steps) {
            return Err(
                Error::value("Expansion limit exceeded (possible runaway expansion)")
                    .with_issue(crate::core::recovery::Issue::Capacity),
            );
        }
        Ok(())
    }
    pub fn get_r_token(context: &mut ExpansionContext<'_>) -> Result<()> {
        let old = context.env.reuse_ignore_meanings.get();
        if context.env.reuse_stateful.get() && context.input.scanner_status == ScannerStatus::Normal
        {
            context.env.reuse_ignore_meanings.set(true);
        }
        let result = (|| -> Result<()> {
            loop {
                context.input.get_token(context.env)?;
                if !context.input.current.literal_category(Cat::Spacer) {
                    break;
                }
            }
            if context.input.current.control_sequence == 0
                || context
                    .env
                    .font_identifier(context.input.current.control_sequence)
                    .is_some()
            {
                return Err(Error::value("Expected a control sequence for assignment"));
            }
            Ok(())
        })();
        context.env.reuse_ignore_meanings.set(old);
        result
    }
    fn raw_normal(context: &mut ExpansionContext<'_>) -> Result<()> {
        context
            .input
            .scoped_scan(context.env, ScannerStatus::Normal, None, |input, env| {
                input.get_token(env)
            })
    }
    pub fn insert_text(context: &mut ExpansionContext<'_>, text: &str) -> Result<()> {
        let tokens: Result<Vec<_>> = text
            .chars()
            .map(|character| {
                character_token(
                    if character == ' ' {
                        Cat::Spacer
                    } else {
                        Cat::OtherChar
                    },
                    character as i64,
                )
            })
            .collect();
        context
            .input
            .begin_generated_tokens(tokens?.into(), context.input.source())
    }
    fn insert_value(
        &mut self,
        context: &mut ExpansionContext<'_>,
        value: ExpansionValue,
    ) -> Result<()> {
        match value {
            ExpansionValue::Tokens(tokens) => context
                .input
                .begin_generated_tokens(tokens, context.input.source())?,
            ExpansionValue::SourcedTokens { tokens, sources } => {
                context
                    .input
                    .begin_tokens(tokens, context.input.source(), Some(sources))?
            }
            ExpansionValue::Text(text) => Self::insert_text(context, &text)?,
            ExpansionValue::Unit => {}
            _ => return Err(Error::value("Unexpected expansion text result")),
        }
        self.complete(ExpansionValue::Unit);
        Ok(())
    }
    fn request(
        &mut self,
        context: &mut ExpansionContext<'_>,
        request: ExpansionRequest,
    ) -> Result<()> {
        match request {
            ExpansionRequest::XToken { protected } => self.x_token(context, protected)?,
            ExpansionRequest::PreparedXToken { protected } => {
                self.prepared_x_token(context, protected)?
            }
            ExpansionRequest::Current => self.expand_current(context)?,
            ExpansionRequest::DefinitionToken => self.definition_token(context)?,
            ExpansionRequest::CsName { define } => {
                self.csname_depth += 1;
                self.guards.push(Guard::CsName);
                let source = context.input.source();
                self.cs_name_next(define, String::new(), source)
            }
            ExpansionRequest::Convert(code) => self.convert(context, code)?,
            ExpansionRequest::ScanToks {
                macro_def,
                expanded,
            } => {
                self.scanning(
                    context,
                    if macro_def {
                        ScannerStatus::Defining
                    } else {
                        ScannerStatus::Absorbing
                    },
                    Some(context.input.current.control_sequence),
                );
                let parameters = if macro_def {
                    Self::scan_parameter_part(context)?
                } else {
                    MacroParameters::default()
                };
                self.body_next(parameters, expanded, macro_def, Vec::new(), Vec::new(), 1)
            }
            request @ (ExpansionRequest::GeneralText { .. }
            | ExpansionRequest::GeneralTextSourced { .. }) => {
                let (expanded, sourced) = match request {
                    ExpansionRequest::GeneralText { expanded } => (expanded, false),
                    ExpansionRequest::GeneralTextSourced { expanded } => (expanded, true),
                    _ => unreachable!(),
                };
                self.action(
                    ExpansionAction::Scan(ScanRequest::Nonblank),
                    move |expander, context, _| {
                        if !context.input.current.is_category(Cat::LeftBrace) {
                            return Err(Error::value("Expected a left brace")
                                .with_issue(crate::core::recovery::Issue::MissingBrace));
                        }
                        expander.native(
                            ExpansionRequest::ScanToks {
                                macro_def: false,
                                expanded,
                            },
                            move |expander, _, value| {
                                let ExpansionValue::Definition { body, sources, .. } = value else {
                                    return Err(Error::value("Invalid balanced text result"));
                                };
                                expander.complete(if sourced {
                                    ExpansionValue::SourcedTokens {
                                        tokens: body,
                                        sources,
                                    }
                                } else {
                                    ExpansionValue::Tokens(body)
                                });
                                Ok(())
                            },
                        );
                        Ok(())
                    },
                )
            }
            ExpansionRequest::Define {
                operation,
                global,
                long,
                outer,
                protected,
            } => {
                let global = context
                    .env
                    .assignment_global(global || operation.is_global());
                Self::get_r_token(context)?;
                let cs = context.input.current.control_sequence;
                let source = context.input.source();
                self.native(
                    ExpansionRequest::ScanToks {
                        macro_def: true,
                        expanded: operation.expands_body(),
                    },
                    move |expander, context, value| {
                        let ExpansionValue::Definition {
                            body,
                            parameters,
                            sources,
                        } = value
                        else {
                            return Err(Error::value("Invalid macro definition result"));
                        };
                        Self::define_macro(
                            context.env,
                            cs,
                            MacroDefinition {
                                body,
                                parameters: Some(parameters),
                                protected,
                                source: source.clone(),
                                long,
                                outer,
                                body_sources: Some(sources),
                            },
                            global,
                        );
                        expander.events.push(ExpansionEvent::Definition {
                            name: context.env.cs_name(cs),
                            expanded: operation.expands_body(),
                            global,
                            long,
                            outer,
                            source,
                        });
                        expander.complete(ExpansionValue::Unit);
                        Ok(())
                    },
                );
            }
        }
        Ok(())
    }
    fn x_token(&mut self, context: &mut ExpansionContext<'_>, protected: bool) -> Result<()> {
        context.input.get_token(context.env)?;
        if (!context.env.reuse_stateful.get() || context.env.reuse_paragraph_level.get().is_some())
            && context.env.values.recording()
            && context.env.meanings.recording()
            && context
                .env
                .reuse_expansions
                .get()
                .is_some_and(|(_, valid)| valid)
            && context.input.scanner_status == ScannerStatus::Normal
            && context.input.current.control_sequence == context.input.par_loc
            && context.input.current.is_command(Command::Macro)
        {
            context.env.reuse_boundary.set(true);
            self.operations.push(Operation::Request(Request::Native(
                ExpansionRequest::PreparedXToken { protected },
            )));
            return Ok(());
        }
        self.prepared_x_token(context, protected)
    }
    fn prepared_x_token(
        &mut self,
        context: &mut ExpansionContext<'_>,
        protected: bool,
    ) -> Result<()> {
        if protected
            && matches!(&context.input.current.value,CommandOperand::Macro(definition) if context.input.current.is_command(Command::Macro) && definition.protected)
        {
            self.complete(ExpansionValue::Unit);
            return Ok(());
        }
        // The ordinary scanner already resolved this token's actual meaning.
        // Collapse pure trampoline steps for a terminal token and for a macro;
        // macro arguments, input frames, counters and provenance still execute
        // through the same services, once per bounded operation.
        if context
            .input
            .current
            .operation()
            .is_none_or(|command| !traits(command).expandable)
        {
            self.complete(ExpansionValue::Unit);
            return Ok(());
        }
        if context.env.local_prefix_armed.replace(false) {
            self.action(ExpansionAction::LocalMaterialBoundary, move |e, c, _| {
                e.prepared_x_token(c, protected)
            });
            return Ok(());
        }
        if context.input.current.is_command(Command::Macro) {
            self.guards.push(Guard::Depth(self.expansion_depth));
            self.enter_expansion()?;
            self.expand_one(context)?;
            let expanded = self
                .value
                .take()
                .ok_or_else(|| Error::value("Missing macro expansion result"))?
                .flag()?;
            self.finish_expand(context, Vec::new(), expanded)?;
            if expanded {
                self.value = None;
                self.operations.push(Operation::Request(Request::Native(
                    ExpansionRequest::XToken { protected },
                )));
            } else {
                self.complete(ExpansionValue::Unit);
            }
            return Ok(());
        }
        self.native(
            ExpansionRequest::Current,
            move |expander, context, value| {
                if value.flag()? {
                    expander.x_token(context, protected)?
                } else {
                    expander.complete(ExpansionValue::Unit)
                }
                Ok(())
            },
        );
        Ok(())
    }
    fn expand_current(&mut self, context: &mut ExpansionContext<'_>) -> Result<()> {
        if context
            .input
            .current
            .operation()
            .is_none_or(|command| !traits(command).expandable)
        {
            self.complete(ExpansionValue::Flag(false));
            return Ok(());
        }
        if context.env.local_prefix_armed.replace(false) {
            self.action(ExpansionAction::LocalMaterialBoundary, |e, c, _| {
                e.expand_current(c)
            });
            return Ok(());
        }
        self.guards.push(Guard::Depth(self.expansion_depth));
        let mut pending = Vec::new();
        while context
            .input
            .current
            .operation()
            .is_some_and(|command| traits(command).expandable)
        {
            self.enter_expansion()?;
            if !context.input.current.is_command(Command::Expandafter) {
                break;
            }
            context.env.observe_expansion(Command::Expandafter);
            context.input.get_token(context.env)?;
            pending.push((context.input.current.token, context.input.source()));
            context.input.get_token(context.env)?;
        }
        if context
            .input
            .current
            .operation()
            .is_none_or(|command| !traits(command).expandable)
        {
            self.finish_expand(context, pending, false)?;
            return Ok(());
        }
        self.operations.push(Operation::Resume(Box::new(
            move |expander, context, value| expander.finish_expand(context, pending, value.flag()?),
        )));
        self.expand_one(context)
    }
    fn finish_expand(
        &mut self,
        context: &mut ExpansionContext<'_>,
        pending: Vec<(Token, Option<Arc<SourceSpan>>)>,
        expanded: bool,
    ) -> Result<()> {
        if !pending.is_empty() && !expanded {
            context.input.back_input()?
        }
        let has_pending = !pending.is_empty();
        for (token, source) in pending.into_iter().rev() {
            context.input.back_token(token, source)?
        }
        self.leave(context);
        self.complete(ExpansionValue::Flag(has_pending || expanded));
        Ok(())
    }
    fn expand_one(&mut self, context: &mut ExpansionContext<'_>) -> Result<()> {
        let current = context.input.current.clone();
        let command = current.operation().unwrap();
        let number = current.character().unwrap_or(-1);
        context
            .env
            .local_expansion_epoch
            .set(context.env.local_expansion_epoch.get().saturating_add(1));
        context.env.observe_expansion(command);
        #[cfg(feature = "latency-diagnostics")]
        if let Some((macros, operations)) = context.env.expansion_packet_probe.get() {
            let flag = match command {
                Command::Macro | Command::Noexpand | Command::Expandafter => 0,
                Command::IfTest | Command::FiElse | Command::Unless => 1,
                Command::CsName => 2,
                Command::The => 4,
                Command::Convert | Command::PdfConvert => 8,
                Command::Input => 16,
                _ => 32,
            };
            context.env.expansion_packet_probe.set(Some((
                macros + u64::from(command == Command::Macro),
                operations | flag,
            )));
        }
        if command == Command::Input && context.env.reuse_physical_input.get().is_some() {
            context.env.reuse_physical_input.set(Some(true));
            // Match InputExecution's operand dispatch. Owned pseudo-input is
            // permitted only while this bounded recording is active; the exit
            // must return to the original physical scanner boundary.
            if !matches!(current.character().unwrap_or(0), 0..=2) {
                context.env.reuse_rejected_by.set(Some("input-kind"));
                context.env.reject_reuse_observation();
            }
        }
        if context.env.reuse_stateful.get()
            && ((command == Command::Convert
                && number == 4
                && !context.env.reuse_font_construction_allowed.get())
                || (command == Command::PdfConvert
                    && !matches!(&current.value,CommandOperand::Text(name)
                if ["pdfstrcmp","pdfescapestring","pdfescapename","pdfescapehex","pdfunescapehex","pdfmdfivesum","pdffilesize","pdffiledump","pdffilemoddate"].contains(&name.as_str()))))
        {
            context.env.reject_reuse_observation();
        }
        match command {
            Command::Macro => {
                let CommandOperand::Macro(definition) = current.value else {
                    return Err(Error::value("Invalid macro meaning"));
                };
                #[cfg(feature = "latency-diagnostics")]
                let _preparation = crate::output_diagnostics::detailed()
                    .then(|| crate::output_diagnostics::operation("macro_preparation"));
                let call = context.input.source();
                let name = context.env.cs_name(current.control_sequence);
                #[cfg(feature = "latency-diagnostics")]
                let _macro = crate::latency_diagnostics::macro_call(
                    &name,
                    definition.source.as_ref(),
                    call.as_ref(),
                    definition.body.len(),
                );
                let has_parameters = definition.parameters.as_ref().is_some_and(|parameters| {
                    !parameters.prefix.is_empty() || !parameters.delimiters.is_empty()
                });
                let params = if has_parameters {
                    #[cfg(feature = "latency-diagnostics")]
                    let _diag = crate::latency_diagnostics::macro_phase(
                        "argument_scanning",
                        &name,
                        Some(crate::latency_diagnostics::region(call.as_ref())),
                    );
                    #[cfg(feature = "latency-diagnostics")]
                    let _scanning = crate::output_diagnostics::detailed()
                        .then(|| crate::output_diagnostics::operation("argument_scanning"));
                    context.input.scoped_scan(
                        context.env,
                        ScannerStatus::Matching,
                        Some(current.control_sequence),
                        |input, env| Self::scan_macro_parameters(input, env, &definition),
                    )?
                } else {
                    Vec::new()
                };
                #[cfg(feature = "latency-diagnostics")]
                if name == "hook_use:n" {
                    crate::output_diagnostics::hook(&params);
                }
                #[cfg(feature = "latency-diagnostics")]
                crate::output_diagnostics::macro_input(&name, &definition, &params, context.env);
                self.begin_macro(context, &definition, &params, call, &name)?;
                self.complete(ExpansionValue::Flag(true));
            }
            Command::CsName => self.native(
                ExpansionRequest::CsName { define: true },
                |expander, _, _| {
                    expander.complete(ExpansionValue::Flag(true));
                    Ok(())
                },
            ),
            Command::Noexpand => {
                Self::raw_normal(context)?;
                if number == 1 {
                    let public = context.input.current.control_sequence;
                    if let Some(meaning) = context.env.original_primitive(public).cloned() {
                        if matches!(meaning.command,MeaningCommand::Operation(command) if traits(command).expandable)
                        {
                            let frozen = context.env.frozen_primitives[&public];
                            context.input.begin_tokens(
                                vec![Token::control_sequence(frozen)].into(),
                                context.input.source(),
                                None,
                            )?
                        } else {
                            context.input.back_input()?;
                            context
                                .input
                                .back_token(Token::control_sequence(C::FROZEN_PRIMITIVE), None)?
                        }
                    }
                } else {
                    if context.input.current.literal_category(Cat::LeftBrace) {
                        context.input.align_state -= 1
                    } else if context.input.current.literal_category(Cat::RightBrace) {
                        context.input.align_state += 1
                    }
                    let tokens = if context.input.current.control_sequence != 0 {
                        vec![
                            Token::Protection(TokenProtection::Noexpand),
                            context.input.current.token,
                        ]
                    } else {
                        vec![context.input.current.token]
                    };
                    context
                        .input
                        .begin_tokens(tokens.into(), context.input.source(), None)?
                }
                self.complete(ExpansionValue::Flag(true));
            }
            Command::Unless => {
                context.input.get_token(context.env)?;
                if !context.input.current.is_command(Command::IfTest)
                    || context.input.current.character() == Some(16)
                {
                    return Err(Error::value("unless requires a non-ifcase conditional"));
                }
                let code = context.input.current.character().unwrap() as usize;
                self.action(
                    ExpansionAction::Conditional { code, unless: true },
                    |expander, _, _| {
                        expander.complete(ExpansionValue::Flag(true));
                        Ok(())
                    },
                );
            }
            Command::IfTest => self.action(
                ExpansionAction::Conditional {
                    code: number as usize,
                    unless: false,
                },
                |expander, _, _| {
                    expander.complete(ExpansionValue::Flag(true));
                    Ok(())
                },
            ),
            Command::FiElse => self.action(
                ExpansionAction::Boundary(number as usize),
                |expander, _, _| {
                    expander.complete(ExpansionValue::Flag(true));
                    Ok(())
                },
            ),
            Command::Convert => {
                self.native(ExpansionRequest::Convert(number as u8), |expander, _, _| {
                    expander.complete(ExpansionValue::Flag(true));
                    Ok(())
                })
            }
            Command::PdfConvert => {
                let CommandOperand::Text(name) = current.value else {
                    return Err(Error::value("Invalid PDF conversion"));
                };
                self.action(
                    ExpansionAction::PdfConvert(name),
                    |expander, context, value| {
                        expander.insert_value(context, value)?;
                        expander.complete(ExpansionValue::Flag(true));
                        Ok(())
                    },
                );
            }
            Command::The => self.action(ExpansionAction::The, |expander, context, value| {
                expander.insert_value(context, value)?;
                expander.complete(ExpansionValue::Flag(true));
                Ok(())
            }),
            Command::MarkText => self.action(
                ExpansionAction::Mark(number as usize),
                |expander, context, value| {
                    expander.insert_value(context, value)?;
                    expander.complete(ExpansionValue::Flag(true));
                    Ok(())
                },
            ),
            Command::Input => {
                if context.input.scanning_filename && number == 0 {
                    self.complete(ExpansionValue::Flag(false));
                } else {
                    self.action(ExpansionAction::Input, |expander, _, _| {
                        expander.complete(ExpansionValue::Flag(true));
                        Ok(())
                    });
                }
            }
            Command::UndefinedCs => {
                context.input.recover(
                    crate::core::recovery::Issue::UndefinedCommand,
                    Error::value(format!(
                        "Undefined control sequence: {}",
                        context.env.cs_name(current.control_sequence)
                    )),
                    "Skipped the undefined command; arguments remain in the input",
                )?;
                self.complete(ExpansionValue::Flag(true));
            }
            _ => self.complete(ExpansionValue::Flag(false)),
        }
        Ok(())
    }
    fn definition_token(&mut self, context: &mut ExpansionContext<'_>) -> Result<()> {
        context.input.get_token(context.env)?;
        if matches!(&context.input.current.value,CommandOperand::Macro(definition)if context.input.current.is_command(Command::Macro)&&definition.protected)
        {
            self.complete(ExpansionValue::Copied(None));
        } else if context.input.current.is_command(Command::The) {
            self.count_expansion()?;
            self.action(ExpansionAction::The, |expander, context, value| {
                match value {
                    ExpansionValue::Tokens(tokens) => {
                        expander.complete(ExpansionValue::Copied(Some(tokens)))
                    }
                    ExpansionValue::Text(text) => {
                        Self::insert_text(context, &text)?;
                        expander.definition_token(context)?
                    }
                    _ => return Err(Error::value("Unexpected the result")),
                }
                Ok(())
            });
        } else {
            self.native(ExpansionRequest::Current, |expander, context, value| {
                if value.flag()? {
                    expander.definition_token(context)?
                } else {
                    expander.complete(ExpansionValue::Copied(None))
                }
                Ok(())
            });
        }
        Ok(())
    }
    fn cs_name_next(&mut self, define: bool, mut name: String, source: Option<Arc<SourceSpan>>) {
        self.native(
            ExpansionRequest::XToken { protected: false },
            move |expander, context, _| {
                if context.input.current.control_sequence != 0 {
                    if !context.input.current.is_command(Command::EndCsName) {
                        context.input.recover(
                            crate::core::recovery::Issue::MissingBrace,
                            Error::value("Missing endcsname in control sequence name"),
                            "Closed the control-sequence name before the unexpected token",
                        )?;
                        context.input.back_input()?;
                    }
                    expander.leave(context);
                    let cs = if name.is_empty() {
                        C::NULL_CS
                    } else if name.chars().count() == 1 {
                        context.env.character_symbol(
                            name.chars().next().unwrap() as u32,
                            false,
                            define,
                        )
                    } else {
                        context.env.intern(&name, define)
                    };
                    if !define && !name.is_empty() && cs == C::NULL_CS {
                        expander.complete(ExpansionValue::Flag(false));
                        return Ok(());
                    }
                    let defined = context.env.meaning(cs).is_some_and(|meaning| {
                        meaning.command != MeaningCommand::Operation(Command::UndefinedCs)
                    });
                    if !define {
                        expander.complete(ExpansionValue::Flag(defined))
                    } else {
                        if !defined {
                            context.env.define(
                                cs,
                                MeaningCommand::Operation(Command::Relax),
                                CommandOperand::Number(0),
                                false,
                            )
                        }
                        context.input.begin_tokens(
                            vec![Token::control_sequence(cs)].into(),
                            source,
                            None,
                        )?;
                        expander.complete(ExpansionValue::Unit)
                    }
                } else {
                    name.push(
                        char::from_u32(
                            context
                                .input
                                .current
                                .character()
                                .ok_or_else(|| Error::value("Invalid csname character"))?
                                as u32,
                        )
                        .ok_or_else(|| Error::value("Invalid csname character"))?,
                    );
                    expander.cs_name_next(define, name, source)
                }
                Ok(())
            },
        );
    }
    fn conversion_text(
        &mut self,
        context: &mut ExpansionContext<'_>,
        code: u8,
        text: String,
        source: Option<Arc<SourceSpan>>,
    ) -> Result<()> {
        let length = text.chars().count();
        if exceeded(context.input.capacities.main_memory, length) {
            return Err(Error::value("Conversion output exceeds token limit"));
        }
        self.events.push(ExpansionEvent::Conversion {
            operation: code,
            output_tokens: length,
            source,
        });
        Self::insert_text(context, &text)?;
        self.complete(ExpansionValue::Unit);
        Ok(())
    }
    fn convert(&mut self, context: &mut ExpansionContext<'_>, code: u8) -> Result<()> {
        let source = context.input.source();
        match code {
            7 => self.native(
                ExpansionRequest::GeneralTextSourced { expanded: true },
                move |expander, context, value| {
                    let ExpansionValue::SourcedTokens { tokens, sources } = value else {
                        return Err(Error::runtime("Missing expanded-text provenance"));
                    };
                    context.input.begin_tokens(tokens, source, Some(sources))?;
                    expander.complete(ExpansionValue::Unit);
                    Ok(())
                },
            ),
            8 => self.conversion_text(
                context,
                code,
                context
                    .input
                    .job_name
                    .clone()
                    .unwrap_or_else(|| "texput".into()),
                source,
            )?,
            0 | 1 => self.action(
                ExpansionAction::Scan(ScanRequest::Int),
                move |expander, context, value| {
                    let number = value.scan()?.integer()?;
                    let text = if code == 0 {
                        number.to_string()
                    } else {
                        roman_numeral(number, context.input.capacities.main_memory)?
                    };
                    expander.conversion_text(context, code, text, source)
                },
            ),
            4 => self.action(
                ExpansionAction::Scan(ScanRequest::FontIdent),
                move |expander, context, value| {
                    let ScanValue::Font(font) = value.scan()? else {
                        return Err(Error::value("Invalid scanned font"));
                    };
                    expander.conversion_text(context, code, (context.font_name)(font)?, source)
                },
            ),
            5 | 6 => self.conversion_text(
                context,
                code,
                if code == 5 { ".6" } else { "25" }.into(),
                source,
            )?,
            2 | 3 => {
                Self::raw_normal(context)?;
                if context.env.reuse_stateful.get()
                    && code == 3
                    && context.input.current.is_command(Command::SetFont)
                    && !context.env.reuse_font_construction_allowed.get()
                {
                    context.env.reject_reuse_observation();
                }
                let printer = TokenPrinter {
                    input: context.input,
                    env: context.env,
                    primitives: &self.primitive_names,
                    font_name: context.font_name,
                };
                let text = if code == 2 {
                    printer.string_token(context.input.current.token)?
                } else {
                    printer.meaning(context.input.current.command, &context.input.current.value)?
                };
                self.conversion_text(context, code, text, source)?
            }
            _ => return Err(Error::value("Unknown conversion code")),
        }
        Ok(())
    }
    pub fn scan_parameter_part(context: &mut ExpansionContext<'_>) -> Result<MacroParameters> {
        let mut parts = vec![Vec::new()];
        let mut hash_brace = None;
        let mut markers = Vec::new();
        loop {
            context.input.get_token(context.env)?;
            let current = context.input.current.clone();
            if current.literal_category(Cat::LeftBrace) {
                break;
            }
            if current.literal_category(Cat::RightBrace) {
                return Err(Error::value("Missing left brace in macro definition")
                    .with_issue(crate::core::recovery::Issue::MacroDefinition));
            }
            if current.is_category(Cat::MacParam) {
                let marker = current.character().unwrap() as u32;
                context.input.get_token(context.env)?;
                if context.input.current.literal_category(Cat::LeftBrace) {
                    hash_brace = Some(context.input.current.token);
                    parts.last_mut().unwrap().push(context.input.current.token);
                    break;
                }
                if !context.input.current.literal_category(Cat::OtherChar)
                    || parts.len() > 9
                    || context.input.current.character() != Some(48 + parts.len() as i64)
                {
                    return Err(Error::value(
                        "Parameters must be numbered consecutively from 1 to 9",
                    )
                    .with_issue(crate::core::recovery::Issue::MacroDefinition));
                }
                parts.push(Vec::new());
                markers.push(marker);
            } else {
                parts.last_mut().unwrap().push(current.token)
            }
        }
        let prefix = parts.remove(0);
        Ok(MacroParameters {
            prefix,
            delimiters: parts,
            hash_brace,
            match_characters: markers,
        })
    }
    fn body_next(
        &mut self,
        parameters: MacroParameters,
        expanded: bool,
        macro_def: bool,
        body: Vec<Token>,
        sources: Vec<Option<Arc<SourceSpan>>>,
        depth: i64,
    ) {
        if expanded {
            self.native(
                ExpansionRequest::DefinitionToken,
                move |expander, context, value| {
                    let ExpansionValue::Copied(copied) = value else {
                        return Err(Error::value("Invalid definition token result"));
                    };
                    if let Some(copied) = copied {
                        let mut body = body;
                        let mut sources = sources;
                        body.extend(copied.iter().copied());
                        sources.extend(std::iter::repeat_n(None, copied.len()));
                        expander.body_next(parameters, expanded, macro_def, body, sources, depth);
                        Ok(())
                    } else {
                        expander.body_token(
                            context, parameters, expanded, macro_def, body, sources, depth,
                        )
                    }
                },
            )
        } else {
            self.operations
                .push(Operation::Resume(Box::new(move |expander, context, _| {
                    context.input.get_token(context.env)?;
                    expander.body_token(
                        context, parameters, expanded, macro_def, body, sources, depth,
                    )
                })));
            self.complete(ExpansionValue::Unit)
        }
    }
    fn body_token(
        &mut self,
        context: &mut ExpansionContext<'_>,
        parameters: MacroParameters,
        expanded: bool,
        macro_def: bool,
        mut body: Vec<Token>,
        mut sources: Vec<Option<Arc<SourceSpan>>>,
        mut depth: i64,
    ) -> Result<()> {
        if context.input.current.literal_category(Cat::LeftBrace) {
            depth += 1
        } else if context.input.current.literal_category(Cat::RightBrace) {
            depth -= 1;
            if depth == 0 {
                if let Some(brace) = parameters.hash_brace {
                    body.push(brace);
                    sources.push(context.input.source())
                }
                self.leave(context);
                self.complete(ExpansionValue::Definition {
                    body: body.into(),
                    parameters,
                    sources: sources.into(),
                });
                return Ok(());
            }
        } else if context.input.current.is_category(Cat::MacParam) && macro_def {
            let finish = move |expander: &mut Self,
                               context: &mut ExpansionContext<'_>,
                               _: ExpansionValue| {
                let current = &context.input.current;
                let token = if current.is_category(Cat::MacParam) {
                    current.token
                } else if current.literal_category(Cat::OtherChar)
                    && current.character().is_some_and(|number| {
                        (49..=48 + parameters.delimiters.len() as i64).contains(&number)
                    })
                {
                    parameter_token((current.character().unwrap() - 48) as usize)?
                } else {
                    return Err(Error::value("Illegal parameter number in definition")
                        .with_issue(crate::core::recovery::Issue::MacroDefinition));
                };
                body.push(token);
                sources.push(context.input.source());
                expander.body_next(parameters, expanded, macro_def, body, sources, depth);
                Ok(())
            };
            if expanded {
                self.native(ExpansionRequest::XToken { protected: false }, finish)
            } else {
                context.input.get_token(context.env)?;
                finish(self, context, ExpansionValue::Unit)?
            }
            return Ok(());
        }
        body.push(context.input.current.token);
        sources.push(context.input.source());
        self.body_next(parameters, expanded, macro_def, body, sources, depth);
        Ok(())
    }
    fn check_argument(input: &InputProcessor, long: bool) -> Result<()> {
        if !long && input.current.token == Token::control_sequence(input.par_loc) {
            return Err(Error::value(
                "Paragraph ended before ordinary macro argument was complete",
            )
            .with_issue(crate::core::recovery::Issue::RunawayArgument));
        }
        Ok(())
    }
    fn argument_group(
        input: &mut InputProcessor,
        env: &mut Environment,
        long: bool,
    ) -> Result<ArgumentTokens> {
        let mut tokens = ArgumentTokens::new(vec![input.current.token], vec![input.source()]);
        let mut depth = 1;
        while depth != 0 {
            input.get_token(env)?;
            Self::check_argument(input, long)?;
            if input.current.literal_category(Cat::LeftBrace) {
                depth += 1
            } else if input.current.literal_category(Cat::RightBrace) {
                depth -= 1
            }
            tokens.append(input.current.token, input.source());
        }
        Ok(tokens)
    }
    pub fn strip_argument_group(tokens: ArgumentTokens) -> ArgumentTokens {
        if tokens
            .tokens
            .first()
            .is_some_and(|token| token_category(*token) == Some(Cat::LeftBrace))
        {
            let mut depth = 0;
            for (index, &token) in tokens.tokens.iter().enumerate() {
                if token_category(token) == Some(Cat::LeftBrace) {
                    depth += 1
                } else if token_category(token) == Some(Cat::RightBrace) {
                    depth -= 1
                }
                if depth == 0 {
                    return if index + 1 == tokens.tokens.len() {
                        tokens.slice(1..index)
                    } else {
                        tokens
                    };
                }
            }
        }
        tokens
    }
    fn delimited_argument(
        input: &mut InputProcessor,
        env: &mut Environment,
        delimiter: &[Token],
        long: bool,
    ) -> Result<ArgumentTokens> {
        let mut tokens = ArgumentTokens::default();
        let mut pending = VecDeque::new();
        loop {
            input.get_token(env)?;
            pending.push_back((input.current.token, input.source()));
            while !pending.is_empty()
                && (pending.len() > delimiter.len()
                    || !pending
                        .iter()
                        .map(|(token, _)| *token)
                        .eq(delimiter[..pending.len()].iter().copied()))
            {
                let fresh = pending.len() == 1;
                let (token, origin) = pending.pop_front().unwrap();
                if fresh {
                    Self::check_argument(input, long)?;
                    if input.current.literal_category(Cat::RightBrace) {
                        return Err(Error::value("Unexpected right brace in macro argument")
                            .with_issue(crate::core::recovery::Issue::RunawayArgument));
                    }
                    if input.current.literal_category(Cat::LeftBrace) {
                        tokens.extend(&Self::argument_group(input, env, long)?);
                        break;
                    }
                }
                tokens.append(token, origin);
            }
            if pending
                .iter()
                .map(|(token, _)| *token)
                .eq(delimiter.iter().copied())
            {
                if input.current.literal_category(Cat::LeftBrace) {
                    input.align_state -= 1
                }
                return Ok(Self::strip_argument_group(tokens));
            }
        }
    }
    pub fn scan_macro_parameters(
        input: &mut InputProcessor,
        env: &mut Environment,
        definition: &MacroDefinition,
    ) -> Result<Vec<ArgumentTokens>> {
        let scan = (|| {
            let Some(parameters) = &definition.parameters else {
                return Ok(Vec::new());
            };
            let mut result = Vec::new();
            for &token in &parameters.prefix {
                input.get_token(env)?;
                if input.current.token != token {
                    return Err(Error::value(
                        "Macro invocation does not match its parameter prefix",
                    )
                    .with_issue(crate::core::recovery::Issue::RunawayArgument));
                }
                if input.current.literal_category(Cat::LeftBrace) {
                    input.align_state -= 1
                }
            }
            for delimiter in &parameters.delimiters {
                if !delimiter.is_empty() {
                    result.push(Self::delimited_argument(
                        input,
                        env,
                        delimiter,
                        definition.long,
                    )?);
                    continue;
                }
                loop {
                    input.get_token(env)?;
                    if !input.current.literal_category(Cat::Spacer) {
                        break;
                    }
                }
                Self::check_argument(input, definition.long)?;
                if input.current.literal_category(Cat::RightBrace) {
                    return Err(Error::value("Unexpected right brace in macro argument")
                        .with_issue(crate::core::recovery::Issue::RunawayArgument));
                }
                if !input.current.literal_category(Cat::LeftBrace) {
                    result.push(ArgumentTokens::new(
                        vec![input.current.token],
                        vec![input.source()],
                    ))
                } else {
                    let group = Self::argument_group(input, env, definition.long)?;
                    result.push(group.slice(1..group.tokens.len() - 1))
                }
            }
            Ok(result)
        })();
        scan.map_err(|error: Error| {
            if error.kind == ErrorKind::Eof {
                Error::value("Unexpected EOF in macro argument")
                    .with_issue(crate::core::recovery::Issue::RunawayArgument)
            } else {
                error
            }
        })
    }
    pub fn begin_macro(
        &mut self,
        context: &mut ExpansionContext<'_>,
        definition: &MacroDefinition,
        params: &[ArgumentTokens],
        call: Option<Arc<SourceSpan>>,
        name: &str,
    ) -> Result<()> {
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::macro_phase(
            "substitution_and_provenance",
            name,
            Some(crate::latency_diagnostics::region(call.as_ref())),
        );
        #[cfg(feature = "latency-diagnostics")]
        let _substitution = crate::output_diagnostics::detailed()
            .then(|| crate::output_diagnostics::operation("substitution_and_provenance"));
        // Every token in this invocation has the same immutable bounded call
        // chain. Preserve its values once instead of walking/allocating per
        // replacement token; each physical definition/argument origin remains.
        let call = bounded_call(call.or_else(|| context.input.source()), 8);
        let label: Arc<str> = format!("macro:{name}").into();
        let source = source_for_call(definition.source.clone(), &label, &call);
        let unknown = source.as_ref().map(SourceSpan::without_range);
        let (tokens, origins): (Arc<[Token]>, Option<Arc<[Option<Arc<SourceSpan>>]>>) =
            if params.is_empty() {
                let origins = definition.body_sources.as_ref().map(|origins| {
                    origins
                        .iter()
                        .map(|origin| {
                            if origin.is_some() {
                                source_for_call(origin.clone(), &label, &call)
                            } else {
                                unknown.clone()
                            }
                        })
                        .collect::<Vec<_>>()
                        .into()
                });
                (definition.body.clone(), origins)
            } else {
                let mut tokens = Vec::new();
                let mut origins = Vec::new();
                for (index, &token) in definition.body.iter().enumerate() {
                    if let Some(number) = parameter_number(token) {
                        let argument = params
                            .get(number - 1)
                            .ok_or_else(|| Error::value("Invalid macro argument reference"))?;
                        tokens.extend(argument.tokens.iter().copied());
                        origins.extend(argument.sources.iter().cloned())
                    } else {
                        tokens.push(token);
                        origins.push(
                            definition
                                .body_sources
                                .as_ref()
                                .map_or_else(|| unknown.clone(), |origins| origins[index].clone()),
                        )
                    }
                }
                let origins = origins
                    .into_iter()
                    .map(|origin| {
                        if origin
                            .as_ref()
                            .zip(source.as_ref())
                            .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
                            || origin.is_none()
                        {
                            unknown.clone()
                        } else {
                            source_for_call(origin, &label, &call)
                        }
                    })
                    .collect::<Vec<_>>();
                (tokens.into(), Some(origins.into()))
            };
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::macro_material(name, &definition.body, params, tokens.len());
        #[cfg(feature = "latency-diagnostics")]
        {
            crate::latency_diagnostics::count(
                "replacement_origin_slots",
                origins.as_ref().map_or(0, |o| o.len()),
            );
            if !params.is_empty() {
                crate::latency_diagnostics::count(
                    "flattened_replacement_token_bytes",
                    tokens.len() * std::mem::size_of::<Token>(),
                );
            }
        }
        let source = if origins.is_some() { source } else { unknown };
        context.input.begin_tokens(tokens, source, origins)?;
        self.events.push(ExpansionEvent::Expansion {
            name: name.into(),
            replacement: definition.body.clone(),
            definition: definition.source.clone(),
            call,
        });
        Ok(())
    }
    pub fn handle_let(context: &mut ExpansionContext<'_>, global: bool) -> Result<()> {
        let future = context.input.current.character() == Some(1);
        Self::get_r_token(context)?;
        let destination = context.input.current.control_sequence;
        if future {
            context.input.get_token(context.env)?;
            let first = context.input.current.token;
            let source = context.input.source();
            context.input.get_token(context.env)?;
            let current = context.input.current.clone();
            context.input.back_input()?;
            context.input.back_token(first, source)?;
            context
                .env
                .define(destination, current.command, current.value, global);
            return Ok(());
        }
        loop {
            context.input.get_token(context.env)?;
            if !context.input.current.is_category(Cat::Spacer) {
                break;
            }
        }
        if context.input.current.is_category(Cat::OtherChar)
            && context.input.current.character() == Some(61)
        {
            context.input.get_token(context.env)?;
            if context.input.current.is_category(Cat::Spacer) {
                context.input.get_token(context.env)?
            }
        }
        context.env.define(
            destination,
            context.input.current.command,
            context.input.current.value.clone(),
            global,
        );
        Ok(())
    }
}
