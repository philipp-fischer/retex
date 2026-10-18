//! Conditional operand scans and raw branch skipping (`conditionals.py`).
use super::input::{InputProcessor, ScannerStatus};
use super::records::{CommandOperand, MeaningCommand};
use super::scanning::{ScanRequest, ScanValue};
use super::state::Environment;
use crate::core::constants::Constants as C;
use crate::core::enums::{CharacterCategory as Cat, Command, ListMode, NodeKind};
use crate::core::errors::{Error, ErrorKind, Result};
use crate::fonts::catalog::FontCatalog;
use crate::typesetting::nodes::NodeBuilder;
use std::cell::RefCell;
use std::rc::Rc;

pub const TESTS: [&str; 24] = [
    "if",
    "ifcat",
    "ifnum",
    "ifdim",
    "ifodd",
    "ifvmode",
    "ifhmode",
    "ifmmode",
    "ifinner",
    "ifvoid",
    "ifhbox",
    "ifvbox",
    "ifx",
    "ifeof",
    "iftrue",
    "iffalse",
    "ifcase",
    "ifdefined",
    "ifcsname",
    "iffontchar",
    "ifincsname",
    "ifpdfprimitive",
    "ifpdfabsnum",
    "ifpdfabsdim",
];
pub const BOUNDARIES: [&str; 3] = ["fi", "else", "or"];
#[derive(Clone, Debug)]
pub struct ConditionalFrame {
    pub code: usize,
    pub unless: bool,
    pub line: usize,
    pub evaluating: bool,
    pub alternative: bool,
}
pub struct ConditionalContext<'a> {
    pub input: &'a mut InputProcessor,
    pub env: &'a mut Environment,
    pub builder: &'a NodeBuilder,
    pub fonts: &'a FontCatalog,
    pub csname_depth: usize,
}
#[derive(Clone, Debug)]
pub enum ConditionalRequest {
    Begin { code: usize, unless: bool },
    Evaluate(usize),
    Character,
}
pub enum ConditionalAction {
    ExpandedToken,
    Scan(ScanRequest),
    CsName,
    StreamEof(i64),
}
pub enum ConditionalValue {
    Unit,
    Flag(bool),
    Scan(ScanValue),
    Character(Option<(Cat, u32)>),
}
impl ConditionalValue {
    fn flag(self) -> Result<bool> {
        if let Self::Flag(flag) = self {
            Ok(flag)
        } else {
            Err(Error::value("Unexpected conditional Boolean result"))
        }
    }
    fn scan(self) -> Result<ScanValue> {
        if let Self::Scan(value) = self {
            Ok(value)
        } else {
            Err(Error::value("Unexpected conditional scanner result"))
        }
    }
}
pub enum ConditionalStep {
    Yield(ConditionalAction),
    Complete(ConditionalValue),
}
enum Request {
    Native(ConditionalRequest),
    External(ConditionalAction),
}
type Resume = Box<
    dyn for<'a> FnOnce(
        &mut Conditionals,
        &mut ConditionalContext<'a>,
        ConditionalValue,
    ) -> Result<()>,
>;
enum Operation {
    Request(Request),
    Resume(Resume),
}
#[derive(Clone, Debug)]
pub struct ConditionalEvent {
    pub test: usize,
    pub result: bool,
    pub depth: usize,
    pub source: Option<std::sync::Arc<crate::core::source::SourceSpan>>,
}
#[derive(Default)]
pub struct Conditionals {
    pub stack: Vec<Rc<RefCell<ConditionalFrame>>>,
    pub events: Vec<ConditionalEvent>,
    operations: Vec<Operation>,
    value: Option<ConditionalValue>,
    awaiting: bool,
}
pub fn meanings_equal(
    a: &(MeaningCommand, CommandOperand),
    b: &(MeaningCommand, CommandOperand),
) -> bool {
    if a.0 != b.0 {
        return false;
    }
    match (&a.1, &b.1) {
        (CommandOperand::Macro(a), CommandOperand::Macro(b)) => {
            a.parameters == b.parameters
                && a.body == b.body
                && a.protected == b.protected
                && a.long == b.long
                && a.outer == b.outer
        }
        _ => a.1 == b.1,
    }
}
impl Conditionals {
    pub(crate) fn checkpoint_clone(&self) -> Option<Self> {
        if self.awaiting || !self.operations.is_empty() {
            return None;
        }
        Some(Self {
            stack: self
                .stack
                .iter()
                .map(|frame| Rc::new(RefCell::new(frame.borrow().clone())))
                .collect(),
            events: self.events.clone(),
            value: None,
            ..Self::default()
        })
    }

    pub fn start(&mut self, request: ConditionalRequest) -> Result<()> {
        if self.awaiting || !self.operations.is_empty() {
            return Err(Error::value("Conditional operation already active"));
        }
        self.value = None;
        self.operations
            .push(Operation::Request(Request::Native(request)));
        Ok(())
    }
    pub fn resume(
        &mut self,
        context: &mut ConditionalContext<'_>,
        returned: Option<ConditionalValue>,
    ) -> Result<ConditionalStep> {
        if self.awaiting {
            self.value =
                Some(returned.ok_or_else(|| Error::value("Missing conditional service result"))?);
            self.awaiting = false
        } else if returned.is_some() {
            return Err(Error::value("Unrequested conditional service result"));
        }
        let result = (|| {
            while let Some(operation) = self.operations.pop() {
                match operation {
                    Operation::Request(Request::External(action)) => {
                        self.awaiting = true;
                        return Ok(ConditionalStep::Yield(action));
                    }
                    Operation::Request(Request::Native(request)) => {
                        self.request(context, request)?
                    }
                    Operation::Resume(resume) => {
                        let value = self
                            .value
                            .take()
                            .ok_or_else(|| Error::value("Missing conditional child result"))?;
                        resume(self, context, value)?
                    }
                }
            }
            Ok(ConditionalStep::Complete(self.value.take().ok_or_else(
                || Error::value("Missing conditional result"),
            )?))
        })();
        if result.is_err() {
            self.operations.clear();
            self.awaiting = false;
            self.value = None
        }
        result
    }
    fn defer(
        &mut self,
        request: Request,
        resume: impl for<'a> FnOnce(
            &mut Self,
            &mut ConditionalContext<'a>,
            ConditionalValue,
        ) -> Result<()>
        + 'static,
    ) {
        self.operations.push(Operation::Resume(Box::new(resume)));
        self.operations.push(Operation::Request(request));
    }
    fn action(
        &mut self,
        action: ConditionalAction,
        resume: impl for<'a> FnOnce(
            &mut Self,
            &mut ConditionalContext<'a>,
            ConditionalValue,
        ) -> Result<()>
        + 'static,
    ) {
        self.defer(Request::External(action), resume)
    }
    fn native(
        &mut self,
        request: ConditionalRequest,
        resume: impl for<'a> FnOnce(
            &mut Self,
            &mut ConditionalContext<'a>,
            ConditionalValue,
        ) -> Result<()>
        + 'static,
    ) {
        self.defer(Request::Native(request), resume)
    }
    fn complete(&mut self, value: ConditionalValue) {
        self.value = Some(value)
    }
    fn raw_normal(context: &mut ConditionalContext<'_>) -> Result<()> {
        context
            .input
            .scoped_scan(context.env, ScannerStatus::Normal, None, |input, env| {
                input.get_token(env)
            })
    }
    pub fn skip(
        context: &mut ConditionalContext<'_>,
        allow_else: bool,
        allow_or: bool,
    ) -> Result<usize> {
        context
            .input
            .scoped_scan(context.env, ScannerStatus::Skipping, None, |input, env| {
                let mut depth = 0;
                loop {
                    input.get_token(env).map_err(|error| {
                        if error.kind == ErrorKind::Eof {
                            Error::value("Incomplete conditional at EOF")
                                .with_issue(crate::core::recovery::Issue::Conditional)
                        } else {
                            error
                        }
                    })?;
                    if input.current.is_command(Command::IfTest) {
                        depth += 1
                    } else if input.current.is_command(Command::FiElse) {
                        let boundary = input
                            .current
                            .character()
                            .ok_or_else(|| Error::value("Invalid conditional boundary"))?;
                        if depth > 0 {
                            if boundary == 0 {
                                depth -= 1
                            }
                        } else if boundary == 0
                            || (boundary == 1 && allow_else)
                            || (boundary == 2 && allow_or)
                        {
                            return Ok(boundary as usize);
                        } else if boundary == 2 && allow_else {
                            input.recover(
                                crate::core::recovery::Issue::Conditional,
                                Error::value("Extra or outside ifcase"),
                                "Ignored the misplaced or delimiter",
                            )?;
                        }
                    }
                }
            })
    }
    fn finish_nested(
        &mut self,
        context: &mut ConditionalContext<'_>,
        frame: &Rc<RefCell<ConditionalFrame>>,
    ) -> Result<()> {
        while self
            .stack
            .last()
            .is_some_and(|nested| !Rc::ptr_eq(nested, frame))
        {
            Self::skip(context, false, false)?;
            self.stack.pop();
        }
        if self.stack.is_empty() {
            return Err(Error::value("Lost active conditional frame"));
        }
        Ok(())
    }
    pub fn boundary(&mut self, context: &mut ConditionalContext<'_>, code: usize) -> Result<()> {
        let name = *BOUNDARIES
            .get(code)
            .ok_or_else(|| Error::value("Invalid conditional boundary"))?;
        let Some(frame) = self.stack.last().cloned() else {
            context.input.recover(
                crate::core::recovery::Issue::Conditional,
                Error::value(format!("Extra {name} without conditional")),
                "Ignored the extra conditional delimiter",
            )?;
            return Ok(());
        };
        if frame.borrow().evaluating {
            let relax = context.input.primitive_token(
                context.env,
                Command::Relax,
                &CommandOperand::Number(0),
            )?;
            context.input.begin_tokens(
                vec![relax, context.input.current.token].into(),
                context.input.source(),
                None,
            )?;
            return Ok(());
        }
        if code == 0 {
            self.stack.pop();
            return Ok(());
        }
        if frame.borrow().alternative {
            context.input.recover(
                crate::core::recovery::Issue::Conditional,
                Error::value(format!("Extra {name} after else")),
                "Ignored the extra conditional delimiter",
            )?;
            return Ok(());
        }
        if code == 2 && frame.borrow().code != 16 {
            return Err(Error::value("Extra or outside ifcase"));
        }
        Self::skip(context, false, false)?;
        self.stack.pop();
        Ok(())
    }
    pub fn finish(&mut self, explicit_end: bool, warn: &mut dyn FnMut(&str)) -> Result<()> {
        if !self.stack.is_empty() {
            if !explicit_end {
                return Err(Error::value("Incomplete conditional at document end")
                    .with_issue(crate::core::recovery::Issue::Conditional));
            }
            for frame in self.stack.iter().rev() {
                let frame = frame.borrow();
                warn(&format!(
                    "Document ended with unfinished {} opened on line {}",
                    TESTS[frame.code], frame.line
                ));
            }
            self.stack.clear()
        }
        Ok(())
    }
    fn request(
        &mut self,
        context: &mut ConditionalContext<'_>,
        request: ConditionalRequest,
    ) -> Result<()> {
        match request {
            ConditionalRequest::Character => self.action(
                ConditionalAction::ExpandedToken,
                |conditionals, context, _| {
                    let current = &context.input.current;
                    let character = if let (Some(category), Some(code)) =
                        (current.category(), current.character())
                    {
                        if context.input.profile.accepts(code) {
                            Some((category, code as u32))
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    let character = character.or_else(|| {
                        context
                            .env
                            .active_character(current.control_sequence)
                            .map(|code| (Cat::ActiveChar, code))
                    });
                    conditionals.complete(ConditionalValue::Character(character));
                    Ok(())
                },
            ),
            ConditionalRequest::Evaluate(code) => self.evaluate(context, code)?,
            ConditionalRequest::Begin { code, unless } => {
                if context.env.reuse_stateful.get() && code == 19 {
                    context.env.reject_reuse_observation();
                }
                if code >= TESTS.len() {
                    return Err(Error::value("Invalid conditional code"));
                }
                let frame = Rc::new(RefCell::new(ConditionalFrame {
                    code,
                    unless,
                    line: context.input.cursor.line,
                    evaluating: true,
                    alternative: false,
                }));
                self.stack.push(frame.clone());
                if code == 16 {
                    self.action(
                        ConditionalAction::Scan(ScanRequest::Int),
                        move |conditionals, context, value| {
                            let mut case = value.scan()?.integer()?;
                            frame.borrow_mut().evaluating = false;
                            if case != 0 {
                                conditionals.finish_nested(context, &frame)?;
                                loop {
                                    let boundary = Self::skip(context, true, true)?;
                                    if boundary == 0 {
                                        conditionals.stack.pop();
                                        break;
                                    }
                                    if boundary == 1 {
                                        frame.borrow_mut().alternative = true;
                                        break;
                                    }
                                    case -= 1;
                                    if case == 0 {
                                        break;
                                    }
                                }
                            }
                            conditionals.complete(ConditionalValue::Unit);
                            Ok(())
                        },
                    );
                } else {
                    self.native(
                        ConditionalRequest::Evaluate(code),
                        move |conditionals, context, value| {
                            let result = value.flag()? ^ unless;
                            #[cfg(feature = "latency-diagnostics")]
                            if crate::output_diagnostics::detailed() {
                                crate::output_diagnostics::event(
                                    "branch",
                                    format!("test={code}|unless={unless}|result={result}"),
                                );
                            }
                            frame.borrow_mut().evaluating = false;
                            conditionals.events.push(ConditionalEvent {
                                test: code,
                                result,
                                depth: conditionals.stack.len(),
                                source: context.input.source(),
                            });
                            if !result {
                                conditionals.finish_nested(context, &frame)?;
                                if Self::skip(context, true, false)? == 0 {
                                    conditionals.stack.pop();
                                } else {
                                    frame.borrow_mut().alternative = true
                                }
                            }
                            conditionals.complete(ConditionalValue::Unit);
                            Ok(())
                        },
                    );
                }
            }
        }
        Ok(())
    }
    fn evaluate(&mut self, context: &mut ConditionalContext<'_>, code: usize) -> Result<()> {
        match code {
            17 => {
                Self::raw_normal(context)?;
                self.complete(ConditionalValue::Flag(
                    !context.input.current.is_command(Command::UndefinedCs),
                ));
            }
            21 => {
                Self::raw_normal(context)?;
                let current = context.input.current.clone();
                let result = context
                    .env
                    .original_primitive(current.control_sequence)
                    .is_some_and(|meaning| {
                        meaning.command == current.command && meaning.value == current.value
                    });
                self.complete(ConditionalValue::Flag(result));
            }
            18 => self.action(ConditionalAction::CsName, |conditionals, _, value| {
                conditionals.complete(ConditionalValue::Flag(value.flag()?));
                Ok(())
            }),
            20 => self.complete(ConditionalValue::Flag(context.csname_depth > 0)),
            19 => self.action(
                ConditionalAction::Scan(ScanRequest::FontIdent),
                |conditionals, _, value| {
                    let ScanValue::Font(font) = value.scan()? else {
                        return Err(Error::value("Invalid conditional font"));
                    };
                    conditionals.action(
                        ConditionalAction::Scan(ScanRequest::CharNum),
                        move |conditionals, context, value| {
                            let code = value.scan()?.integer()?;
                            conditionals.complete(ConditionalValue::Flag(
                                context.fonts.items[font].has_character(code as u32),
                            ));
                            Ok(())
                        },
                    );
                    Ok(())
                },
            ),
            0 | 1 => self.native(
                ConditionalRequest::Character,
                move |conditionals, _, value| {
                    let ConditionalValue::Character(left) = value else {
                        return Err(Error::value("Invalid conditional character"));
                    };
                    conditionals.native(
                        ConditionalRequest::Character,
                        move |conditionals, _, value| {
                            let ConditionalValue::Character(right) = value else {
                                return Err(Error::value("Invalid conditional character"));
                            };
                            conditionals.complete(ConditionalValue::Flag(if code == 0 {
                                left.map(|(_, code)| code) == right.map(|(_, code)| code)
                            } else {
                                left.map(|(category, _)| category)
                                    == right.map(|(category, _)| category)
                            }));
                            Ok(())
                        },
                    );
                    Ok(())
                },
            ),
            12 => {
                let result = context.input.scoped_scan(
                    context.env,
                    ScannerStatus::Normal,
                    None,
                    |input, env| {
                        input.get_token(env)?;
                        let left = (input.current.command, input.current.value.clone());
                        input.get_token(env)?;
                        Ok(meanings_equal(
                            &left,
                            &(input.current.command, input.current.value.clone()),
                        ))
                    },
                )?;
                self.complete(ConditionalValue::Flag(result));
            }
            2 | 3 | 22 | 23 => {
                let scan = if code == 2 || code == 22 {
                    ScanRequest::Int
                } else {
                    ScanRequest::Dimen {
                        mu: false,
                        infinite: false,
                    }
                };
                self.action(
                    ConditionalAction::Scan(scan.clone()),
                    move |conditionals, _, value| {
                        let mut left = value.scan()?.integer()?;
                        conditionals.action(
                            ConditionalAction::Scan(ScanRequest::Nonblank),
                            move |conditionals, context, _| {
                                let current = &context.input.current;
                                if !current.literal_category(Cat::OtherChar)
                                    || !current
                                        .character()
                                        .is_some_and(|code| (60..=62).contains(&code))
                                {
                                    return Err(Error::value(format!(
                                        "Missing comparison relation in {}",
                                        TESTS[code]
                                    )));
                                }
                                let relation = current.character().unwrap();
                                conditionals.action(
                                    ConditionalAction::Scan(scan),
                                    move |conditionals, _, value| {
                                        let mut right = value.scan()?.integer()?;
                                        if code >= 22 {
                                            left = left.abs();
                                            right = right.abs()
                                        }
                                        conditionals.complete(ConditionalValue::Flag(
                                            match relation {
                                                60 => left < right,
                                                61 => left == right,
                                                _ => left > right,
                                            },
                                        ));
                                        Ok(())
                                    },
                                );
                                Ok(())
                            },
                        );
                        Ok(())
                    },
                );
            }
            4 => self.action(
                ConditionalAction::Scan(ScanRequest::Int),
                |conditionals, _, value| {
                    conditionals
                        .complete(ConditionalValue::Flag(value.scan()?.integer()? % 2 != 0));
                    Ok(())
                },
            ),
            14 | 15 => self.complete(ConditionalValue::Flag(code == 14)),
            9..=11 => self.action(
                ConditionalAction::Scan(ScanRequest::Register(None)),
                move |conditionals, context, value| {
                    let register = value.scan()?.integer()?;
                    let pointer = context
                        .env
                        .box_registers
                        .get(&register)
                        .copied()
                        .unwrap_or(C::NULL);
                    let result = if code == 9 {
                        pointer == C::NULL
                    } else {
                        pointer != C::NULL
                            && context.builder.node(pointer)?.node_type
                                == if code == 10 {
                                    NodeKind::Hlist
                                } else {
                                    NodeKind::Vlist
                                }
                    };
                    conditionals.complete(ConditionalValue::Flag(result));
                    Ok(())
                },
            ),
            13 => self.action(
                ConditionalAction::Scan(ScanRequest::Register(Some(C::MAX_FILE_STREAM))),
                |conditionals, _, value| {
                    let stream = value.scan()?.integer()?;
                    conditionals.action(
                        ConditionalAction::StreamEof(stream),
                        |conditionals, _, value| {
                            conditionals.complete(ConditionalValue::Flag(value.flag()?));
                            Ok(())
                        },
                    );
                    Ok(())
                },
            ),
            8 => self.complete(ConditionalValue::Flag(
                context.builder.context.mode != ListMode::Disabled
                    && context.builder.context.restricted,
            )),
            5..=7 => self.complete(ConditionalValue::Flag(
                context.builder.context.mode
                    == match code {
                        5 => ListMode::Vertical,
                        6 => ListMode::Horizontal,
                        _ => ListMode::Math,
                    },
            )),
            _ => return Err(Error::value("Invalid Boolean conditional code")),
        }
        Ok(())
    }
}
