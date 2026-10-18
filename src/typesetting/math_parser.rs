//! Token-to-noad parsing with typed yields to the engine (`math_parser.py`).
use super::layout::{Layout, TypesettingContext};
use super::math_atoms::{Atom, Field, MathCommand, MathField, MathListFrame, MathState};
use super::math_codes::{DelimiterCode, GLYPH_RADIX, MathCharacterCode};
use super::math_construction::MathContext;
use super::math_families::MathFamilies;
use super::math_spacing::AtomKind;
use super::nodes::integer;
use crate::core::constants::Constants as C;
use crate::core::enums::{
    BoxOperation, CharacterCategory as Cat, Command, GroupKind, ListMode, MathStyle,
};
use crate::core::errors::{Error, Result};
use crate::core::values::GlueSpec;
use crate::language::character_tokens::Token;
use crate::language::code_validation::{
    ACTIVE_MATH_CODE, MAX_DELIMITER_CODE, MAX_DELIMITER_OPERAND,
};
use crate::language::input::InputProcessor;
use crate::language::records::{CommandOperand, Quantity, ValueIndex, ValueKey};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MathStop {
    Brace,
    Shift,
    Right,
}
pub struct MathParseContext<'a, 'b> {
    pub typesetting: &'a mut TypesettingContext<'b>,
    pub layout: &'a mut Layout,
    pub state: &'a mut MathState,
}
pub trait MathParserServices {
    fn input(&self) -> &InputProcessor;
    fn input_mut(&mut self) -> &mut InputProcessor;
    fn expanded_token(&mut self, context: &mut MathParseContext<'_, '_>) -> Result<()>;
    fn scan_int(&mut self, context: &mut MathParseContext<'_, '_>) -> Result<i64>;
    fn scan_char_num(&mut self, context: &mut MathParseContext<'_, '_>) -> Result<i64>;
    fn scan_dimen(&mut self, context: &mut MathParseContext<'_, '_>, mu: bool) -> Result<i64>;
    fn scan_glue(&mut self, context: &mut MathParseContext<'_, '_>, mu: bool) -> Result<GlueSpec>;
    fn begin_group(
        &mut self,
        context: &mut MathParseContext<'_, '_>,
        name: &str,
        kind: Option<GroupKind>,
    ) -> Result<()>;
    fn end_group(&mut self, context: &mut MathParseContext<'_, '_>, name: &str) -> Result<()>;
    fn nonblank(&mut self, context: &mut MathParseContext<'_, '_>) -> Result<()> {
        loop {
            self.expanded_token(context)?;
            if !self.input().current.is_category(Cat::Spacer) {
                return Ok(());
            }
        }
    }
}
pub enum MathParserAction {
    ExpandedToken,
    Scan(crate::language::scanning::ScanRequest),
    BoxOperand,
    ReadBox {
        operation: BoxOperation,
        group: GroupKind,
    },
    ExecuteToken {
        in_box: bool,
    },
    ReadAlignment {
        vertical: bool,
    },
    RenderEquation {
        atoms: Vec<Atom>,
        style: MathStyle,
    },
}
pub enum MathParserValue {
    Unit,
    Character(Option<MathCharacterCode>),
    Delimiter(DelimiterCode),
    Fraction {
        left: DelimiterCode,
        right: DelimiterCode,
        thickness: Option<i64>,
    },
    Scan(crate::language::scanning::ScanValue),
    Field(Field),
    Read {
        atoms: Vec<Atom>,
        closing: Option<DelimiterCode>,
    },
    Box(i64),
    Executed(bool),
    Alignment(i64),
    Rendered(Vec<i64>),
}
impl MathParserValue {
    fn field(self) -> Result<Field> {
        if let Self::Field(field) = self {
            Ok(field)
        } else {
            Err(Error::value("Unexpected math parser field result"))
        }
    }
    fn read(self) -> Result<(Vec<Atom>, Option<DelimiterCode>)> {
        if let Self::Read { atoms, closing } = self {
            Ok((atoms, closing))
        } else {
            Err(Error::value("Unexpected math parser list result"))
        }
    }
    fn box_pointer(self) -> Result<i64> {
        if let Self::Box(pointer) = self {
            Ok(pointer)
        } else {
            Err(Error::value("Unexpected math parser box result"))
        }
    }
}
pub enum MathParserStep {
    Yield(MathParserAction),
    Complete(MathParserValue),
}
enum Request {
    Read(MathStop),
    Field(GroupKind),
    Character,
    Delimiter(bool),
    Fraction(i64),
    External(MathParserAction),
}
type Resume = Box<
    dyn for<'a, 'b> FnOnce(
        &mut MathParser,
        &mut MathParseContext<'a, 'b>,
        &mut dyn MathParserServices,
        MathParserValue,
    ) -> Result<()>,
>;
enum Operation {
    Request(Request),
    ReadNext(Rc<RefCell<ReadFrame>>),
    Resume(Resume),
}
struct ReadFrame {
    stop: MathStop,
    atoms: Rc<RefCell<Vec<Atom>>>,
    segments: Vec<Atom>,
    fraction: Option<Atom>,
}
#[derive(Default)]
pub struct MathParser {
    operations: Vec<Operation>,
    value: Option<MathParserValue>,
    awaiting: bool,
    original_frames: usize,
    original_depth: usize,
}
impl MathParser {
    pub fn start(&mut self, context: &MathParseContext<'_, '_>, stop: MathStop) -> Result<()> {
        if !self.operations.is_empty() {
            return Err(Error::value("Math parser already active"));
        }
        self.value = None;
        self.awaiting = false;
        self.original_frames = context.state.list_frames.len();
        self.original_depth = context.state.depth;
        self.operations
            .push(Operation::Request(Request::Read(stop)));
        Ok(())
    }
    pub fn start_field(
        &mut self,
        context: &MathParseContext<'_, '_>,
        kind: GroupKind,
    ) -> Result<()> {
        self.start(context, MathStop::Brace)?;
        self.operations.clear();
        self.operations
            .push(Operation::Request(Request::Field(kind)));
        Ok(())
    }
    pub fn resume(
        &mut self,
        context: &mut MathParseContext<'_, '_>,
        services: &mut dyn MathParserServices,
        returned: Option<MathParserValue>,
    ) -> Result<MathParserStep> {
        if self.awaiting {
            self.value =
                Some(returned.ok_or_else(|| Error::value("Missing math interpreter result"))?);
            self.awaiting = false;
        } else if returned.is_some() {
            return Err(Error::value("Unrequested math interpreter result"));
        }
        let result = (|| -> Result<MathParserStep> {
            while let Some(operation) = self.operations.pop() {
                match operation {
                    Operation::Request(Request::External(action)) => {
                        self.awaiting = true;
                        return Ok(MathParserStep::Yield(action));
                    }
                    Operation::Request(Request::Read(stop)) => {
                        let atoms = Rc::new(RefCell::new(Vec::new()));
                        context.state.depth += 1;
                        context.state.list_frames.push(MathListFrame {
                            context_depth: context.typesetting.builder.context_stack.len(),
                            atoms: atoms.clone(),
                        });
                        self.operations
                            .push(Operation::ReadNext(Rc::new(RefCell::new(ReadFrame {
                                stop,
                                atoms,
                                segments: Vec::new(),
                                fraction: None,
                            }))));
                    }
                    Operation::Request(Request::Field(kind)) => {
                        self.field_begin(context, services, kind)?
                    }
                    Operation::Request(Request::Character) => {
                        self.character_begin(context, services)?
                    }
                    Operation::Request(Request::Delimiter(numeric)) => {
                        self.delimiter_begin(numeric)
                    }
                    Operation::Request(Request::Fraction(value)) => self.fraction_begin(value),
                    Operation::ReadNext(frame) => self.read_next(context, services, frame)?,
                    Operation::Resume(resume) => {
                        let value = self
                            .value
                            .take()
                            .ok_or_else(|| Error::value("Missing math parser child result"))?;
                        resume(self, context, services, value)?;
                    }
                }
            }
            Ok(MathParserStep::Complete(self.value.take().ok_or_else(
                || Error::value("Missing completed math parser result"),
            )?))
        })();
        if result.is_err() {
            self.operations.clear();
            self.awaiting = false;
            self.value = None;
            context.state.list_frames.truncate(self.original_frames);
            context.state.depth = self.original_depth;
        }
        result
    }
    pub fn abort(&mut self, context: &mut MathParseContext<'_, '_>) {
        self.operations.clear();
        self.value = None;
        self.awaiting = false;
        context.state.list_frames.truncate(self.original_frames);
        context.state.depth = self.original_depth;
    }
    fn defer(
        &mut self,
        request: Request,
        resume: impl for<'a, 'b> FnOnce(
            &mut Self,
            &mut MathParseContext<'a, 'b>,
            &mut dyn MathParserServices,
            MathParserValue,
        ) -> Result<()>
        + 'static,
    ) {
        self.operations.push(Operation::Resume(Box::new(resume)));
        self.operations.push(Operation::Request(request));
    }
    /// Synchronous adapter retained for callers of the original parser API.
    pub fn character(
        context: &mut MathParseContext<'_, '_>,
        services: &mut dyn MathParserServices,
    ) -> Result<Option<MathCharacterCode>> {
        let current = services.input().current.clone();
        let scanned =
            if current.is_command(Command::MathChar) || current.is_command(Command::Delimiter) {
                Some(services.scan_int(context)?)
            } else if current.is_command(Command::CharNum) {
                Some(services.scan_char_num(context)?)
            } else {
                None
            };
        Self::character_finish(context, services, current, scanned)
    }
    /// Synchronous adapter over the same delimiter continuation used by sessions.
    pub fn delimiter(
        context: &mut MathParseContext<'_, '_>,
        services: &mut dyn MathParserServices,
        numeric: bool,
    ) -> Result<DelimiterCode> {
        let mut parser = Self::default();
        parser.start(context, MathStop::Brace)?;
        parser.operations.clear();
        parser
            .operations
            .push(Operation::Request(Request::Delimiter(numeric)));
        let mut returned = None;
        loop {
            match parser.resume(context, services, returned.take())? {
                MathParserStep::Complete(MathParserValue::Delimiter(code)) => return Ok(code),
                MathParserStep::Yield(MathParserAction::Scan(
                    crate::language::scanning::ScanRequest::Nonblank,
                )) => {
                    services.nonblank(context)?;
                    returned = Some(MathParserValue::Scan(
                        crate::language::scanning::ScanValue::Unit,
                    ));
                }
                MathParserStep::Yield(MathParserAction::Scan(
                    crate::language::scanning::ScanRequest::Int,
                )) => {
                    returned = Some(MathParserValue::Scan(
                        crate::language::scanning::ScanValue::Integer(services.scan_int(context)?),
                    ));
                }
                _ => return Err(Error::runtime("Invalid delimiter continuation")),
            }
        }
    }
    fn character_begin(
        &mut self,
        context: &mut MathParseContext<'_, '_>,
        services: &mut dyn MathParserServices,
    ) -> Result<()> {
        use crate::language::scanning::ScanRequest;
        let current = services.input().current.clone();
        let request =
            if current.is_command(Command::MathChar) || current.is_command(Command::Delimiter) {
                Some(ScanRequest::Int)
            } else if current.is_command(Command::CharNum) {
                Some(ScanRequest::CharNum)
            } else {
                None
            };
        if let Some(request) = request {
            self.defer(
                Request::External(MathParserAction::Scan(request)),
                move |parser, context, services, value| {
                    let MathParserValue::Scan(value) = value else {
                        return Err(Error::runtime("Invalid math character scan result"));
                    };
                    parser.value = Some(MathParserValue::Character(Self::character_finish(
                        context,
                        services,
                        current,
                        Some(value.integer()?),
                    )?));
                    Ok(())
                },
            );
        } else {
            self.value = Some(MathParserValue::Character(Self::character_finish(
                context, services, current, None,
            )?));
        }
        Ok(())
    }
    fn character_finish(
        context: &mut MathParseContext<'_, '_>,
        services: &mut dyn MathParserServices,
        current: crate::language::input::CurrentToken,
        scanned: Option<i64>,
    ) -> Result<Option<MathCharacterCode>> {
        let code;
        if current.is_command(Command::MathChar) {
            code = scanned.unwrap();
            if !(0..ACTIVE_MATH_CODE).contains(&code) {
                return Err(Error::value("Bad math character code"));
            }
        } else if current.is_command(Command::MathCharNum) {
            code = current
                .character()
                .ok_or_else(|| Error::value("Missing math character code"))?;
        } else if current.is_command(Command::Delimiter) {
            let value = scanned.unwrap();
            if !(0..=MAX_DELIMITER_OPERAND).contains(&value) {
                return Err(Error::value("Bad delimiter code"));
            }
            code = value / GLYPH_RADIX;
        } else {
            let character = if current.is_command(Command::CharNum) {
                scanned.unwrap()
            } else {
                current
                    .character()
                    .ok_or_else(|| Error::value("Missing math character"))?
            };
            let default = if (65..=90).contains(&character) || (97..=122).contains(&character) {
                0x7100 + character
            } else if (48..=57).contains(&character) {
                0x7000 + character
            } else {
                character
            };
            code = integer(
                context.typesetting.env,
                &ValueKey {
                    table: "mathcode".into(),
                    index: ValueIndex::Number(character),
                },
                default,
            );
            if code == ACTIVE_MATH_CODE {
                let symbol = context
                    .typesetting
                    .env
                    .character_symbol(character as u32, true, true);
                let source = services.input().source();
                services.input_mut().begin_tokens(
                    Arc::from([Token::control_sequence(symbol)]),
                    source,
                    None,
                )?;
                return Ok(None);
            }
        }
        Ok(Some(MathCharacterCode::from_source(code)?))
    }
    fn is_character(current: &crate::language::input::CurrentToken) -> bool {
        current.is_category(Cat::Letter)
            || current.is_category(Cat::OtherChar)
            || current.operation().is_some_and(|command| {
                matches!(
                    command,
                    Command::CharGiven
                        | Command::CharNum
                        | Command::MathChar
                        | Command::MathCharNum
                        | Command::Delimiter
                )
            })
    }
    fn field_begin(
        &mut self,
        _context: &mut MathParseContext<'_, '_>,
        _services: &mut dyn MathParserServices,
        kind: GroupKind,
    ) -> Result<()> {
        self.defer(
            Request::External(MathParserAction::Scan(
                crate::language::scanning::ScanRequest::Nonblank,
            )),
            move |parser, context, services, _| {
                if services.input().current.is_command(Command::Relax)
                    && services.input().current.character() == Some(0)
                {
                    parser
                        .operations
                        .push(Operation::Request(Request::Field(kind)));
                    Ok(())
                } else {
                    parser.field_current(context, services, kind)
                }
            },
        );
        Ok(())
    }
    fn field_current(
        &mut self,
        context: &mut MathParseContext<'_, '_>,
        services: &mut dyn MathParserServices,
        kind: GroupKind,
    ) -> Result<()> {
        let current = services.input().current.clone();
        if Self::is_character(&current) {
            let source = services.input().source();
            self.defer(Request::Character, move |parser, context, _, value| {
                let MathParserValue::Character(code) = value else {
                    return Err(Error::runtime("Invalid math field character"));
                };
                if let Some(code) = code {
                    parser.value = Some(MathParserValue::Field(Some(Arc::new(
                        MathFamilies::char_field(context.typesetting.env, code).with_origin(source),
                    ))));
                } else {
                    parser
                        .operations
                        .push(Operation::Request(Request::Field(GroupKind::MathField)));
                }
                Ok(())
            });
            return Ok(());
        }
        if current.is_category(Cat::LeftBrace) {
            services.begin_group(context, "math_field", Some(kind))?;
            context.typesetting.builder.push_context(
                context.typesetting.fonts,
                context.typesetting.env,
                ListMode::Math,
                true,
            )?;
            self.defer(
                Request::Read(MathStop::Brace),
                |parser, context, services, value| {
                    let (atoms, _) = value.read()?;
                    context.typesetting.builder.pop_context()?;
                    services.end_group(context, "math_field")?;
                    let field = if atoms.len() == 1
                        && atoms[0].kind == AtomKind::Ordinary
                        && matches!(atoms[0].command, MathCommand::Nucleus)
                        && atoms[0].sup.is_none()
                        && atoms[0].sub.is_none()
                        && matches!(
                            atoms[0].nucleus.as_deref(),
                            Some(MathField::Character { .. })
                        ) {
                        atoms[0].nucleus.clone()
                    } else {
                        Some(Arc::new(MathField::List(atoms)))
                    };
                    parser.value = Some(MathParserValue::Field(field));
                    Ok(())
                },
            );
            return Ok(());
        }
        if current.is_command(Command::MakeBox) {
            services.input_mut().back_input()?;
            self.defer(
                Request::External(MathParserAction::BoxOperand),
                |parser, context, _, value| {
                    let pointer = value.box_pointer()?;
                    if pointer != C::NULL {
                        context.state.held.push(pointer);
                    }
                    parser.value = Some(MathParserValue::Field(Some(Arc::new(MathField::Box(
                        pointer,
                    )))));
                    Ok(())
                },
            );
            return Ok(());
        }
        Err(Error::value("Expected math character or braced math field")
            .with_issue(crate::core::recovery::Issue::MathStructure))
    }
    fn delimiter_value(code: i64) -> Result<MathParserValue> {
        if !(0..=MAX_DELIMITER_CODE).contains(&code) {
            return Err(Error::value("Invalid delimiter")
                .with_issue(crate::core::recovery::Issue::MathDelimiter));
        }
        Ok(MathParserValue::Delimiter(DelimiterCode::from_source(code)))
    }
    fn delimiter_begin(&mut self, numeric: bool) {
        use crate::language::scanning::ScanRequest;
        if numeric {
            self.defer(
                Request::External(MathParserAction::Scan(ScanRequest::Int)),
                |parser, _, _, value| {
                    let MathParserValue::Scan(value) = value else {
                        return Err(Error::runtime("Invalid delimiter scan"));
                    };
                    parser.value = Some(Self::delimiter_value(value.integer()?)?);
                    Ok(())
                },
            );
        } else {
            self.defer(
                Request::External(MathParserAction::Scan(ScanRequest::Nonblank)),
                |parser, context, services, _| {
                    let current = services.input().current.clone();
                    if current.is_command(Command::Delimiter) {
                        parser.defer(
                            Request::External(MathParserAction::Scan(ScanRequest::Int)),
                            |parser, _, _, value| {
                                let MathParserValue::Scan(value) = value else {
                                    return Err(Error::runtime("Invalid delimiter scan"));
                                };
                                parser.value = Some(Self::delimiter_value(
                                    value.integer()? & MAX_DELIMITER_CODE,
                                )?);
                                Ok(())
                            },
                        );
                    } else if current.is_category(Cat::Letter)
                        || current.is_category(Cat::OtherChar)
                        || current.is_command(Command::CharGiven)
                    {
                        let code = current.character().ok_or_else(|| {
                            Error::value("Missing delimiter character")
                                .with_issue(crate::core::recovery::Issue::MathDelimiter)
                        })?;
                        let code = integer(
                            context.typesetting.env,
                            &ValueKey {
                                table: "delcode".into(),
                                index: ValueIndex::Number(code),
                            },
                            if code == 46 { 0 } else { -1 },
                        );
                        parser.value = Some(Self::delimiter_value(code)?);
                    } else {
                        return Err(Error::value("Missing delimiter")
                            .with_issue(crate::core::recovery::Issue::MathDelimiter));
                    }
                    Ok(())
                },
            );
        }
    }
    fn fraction_begin(&mut self, value: i64) {
        if value >= 3 {
            self.defer(Request::Delimiter(false), move |parser, _, _, left| {
                let MathParserValue::Delimiter(left) = left else {
                    return Err(Error::runtime("Invalid fraction delimiter"));
                };
                parser.defer(Request::Delimiter(false), move |parser, _, _, right| {
                    let MathParserValue::Delimiter(right) = right else {
                        return Err(Error::runtime("Invalid fraction delimiter"));
                    };
                    parser.fraction_thickness(value, left, right);
                    Ok(())
                });
                Ok(())
            });
        } else {
            self.fraction_thickness(value, DelimiterCode::default(), DelimiterCode::default());
        }
    }
    fn fraction_thickness(&mut self, value: i64, left: DelimiterCode, right: DelimiterCode) {
        if value % 3 < 2 {
            self.value = Some(MathParserValue::Fraction {
                left,
                right,
                thickness: if value % 3 == 0 { None } else { Some(0) },
            });
        } else {
            self.defer(
                Request::External(MathParserAction::Scan(
                    crate::language::scanning::ScanRequest::Dimen {
                        mu: false,
                        infinite: false,
                    },
                )),
                move |parser, _, _, value| {
                    let MathParserValue::Scan(value) = value else {
                        return Err(Error::runtime("Invalid fraction thickness"));
                    };
                    parser.value = Some(MathParserValue::Fraction {
                        left,
                        right,
                        thickness: Some(value.integer()?),
                    });
                    Ok(())
                },
            );
        }
    }
    fn close_fraction(frame: &mut ReadFrame) {
        if let Some(mut fraction) = frame.fraction.take() {
            let atoms = std::mem::take(&mut *frame.atoms.borrow_mut());
            if let MathCommand::Fraction { denominator, .. } = &mut fraction.command {
                *denominator = Some(Arc::new(MathField::List(atoms)));
            }
            frame.atoms.borrow_mut().push(fraction);
        }
    }
    fn finish_read(
        &mut self,
        context: &mut MathParseContext<'_, '_>,
        frame: &Rc<RefCell<ReadFrame>>,
        closing: Option<DelimiterCode>,
    ) {
        let mut frame = frame.borrow_mut();
        Self::close_fraction(&mut frame);
        let mut atoms = std::mem::take(&mut frame.segments);
        atoms.extend(std::mem::take(&mut *frame.atoms.borrow_mut()));
        context.state.list_frames.pop();
        context.state.depth -= 1;
        self.value = Some(MathParserValue::Read { atoms, closing });
    }
    fn append_field(
        &mut self,
        frame: Rc<RefCell<ReadFrame>>,
        kind: AtomKind,
        command: MathCommand,
        source: Option<Arc<crate::core::source::SourceSpan>>,
    ) {
        self.defer(
            Request::Field(GroupKind::MathField),
            move |parser, _, _, value| {
                frame.borrow().atoms.borrow_mut().push(Atom {
                    kind,
                    nucleus: value.field()?,
                    command,
                    source,
                    ..Atom::default()
                });
                parser.operations.push(Operation::ReadNext(frame));
                Ok(())
            },
        );
    }
    fn read_next(
        &mut self,
        _context: &mut MathParseContext<'_, '_>,
        _services: &mut dyn MathParserServices,
        frame: Rc<RefCell<ReadFrame>>,
    ) -> Result<()> {
        self.defer(
            Request::External(MathParserAction::ExpandedToken),
            move |parser, context, services, _| parser.read_current(context, services, frame),
        );
        Ok(())
    }
    fn read_current(
        &mut self,
        context: &mut MathParseContext<'_, '_>,
        services: &mut dyn MathParserServices,
        frame: Rc<RefCell<ReadFrame>>,
    ) -> Result<()> {
        let current = services.input().current.clone();
        let source = services.input().source();
        let stop = frame.borrow().stop;
        let value = current.character().unwrap_or(0);
        if current.is_category(Cat::RightBrace) && stop == MathStop::Brace
            || current.is_category(Cat::MathShift) && stop == MathStop::Shift
        {
            self.finish_read(context, &frame, None);
            return Ok(());
        }
        if current.is_command(Command::MathLeftRight)
            && stop == MathStop::Right
            && (value == 1 || value == 2)
        {
            self.defer(
                Request::Delimiter(false),
                move |parser, context, services, delimiter| {
                    let MathParserValue::Delimiter(delimiter) = delimiter else {
                        return Err(Error::runtime("Invalid closing delimiter"));
                    };
                    if value == 1 {
                        parser.finish_read(context, &frame, Some(delimiter));
                    } else {
                        {
                            let mut reader = frame.borrow_mut();
                            Self::close_fraction(&mut reader);
                            let atoms = std::mem::take(&mut *reader.atoms.borrow_mut());
                            reader.segments.extend(atoms);
                            reader.segments.push(Atom {
                                kind: AtomKind::Closing,
                                command: MathCommand::Middle(delimiter),
                                source: source.clone(),
                                ..Atom::default()
                            });
                        }
                        services.end_group(context, "math_left")?;
                        services.begin_group(context, "math_left", None)?;
                        parser.operations.push(Operation::ReadNext(frame));
                    }
                    Ok(())
                },
            );
            return Ok(());
        }
        if Self::is_character(&current) {
            self.defer(Request::Character, move |parser, context, _, value| {
                let MathParserValue::Character(code) = value else {
                    return Err(Error::runtime("Invalid math list character"));
                };
                if let Some(code) = code {
                    frame.borrow().atoms.borrow_mut().push(Atom {
                        kind: code.kind,
                        nucleus: Some(Arc::new(
                            MathFamilies::char_field(context.typesetting.env, code)
                                .with_origin(source.clone()),
                        )),
                        source: source.clone(),
                        ..Atom::default()
                    });
                }
                parser.operations.push(Operation::ReadNext(frame));
                Ok(())
            });
            return Ok(());
        }
        if current.is_category(Cat::LeftBrace) || current.is_command(Command::MakeBox) {
            services.input_mut().back_input()?;
            self.append_field(
                frame,
                AtomKind::Ordinary,
                MathCommand::Nucleus,
                source.clone(),
            );
            return Ok(());
        }
        if current.is_category(Cat::SupMark) || current.is_category(Cat::SubMark) {
            let sup = current.is_category(Cat::SupMark);
            {
                let reader = frame.borrow();
                let mut atoms = reader.atoms.borrow_mut();
                if atoms
                    .last()
                    .is_none_or(|atom| atom.command.needs_new_script_atom())
                {
                    atoms.push(Atom::default());
                }
                let atom = atoms.last().unwrap();
                if if sup {
                    atom.sup.is_some()
                } else {
                    atom.sub.is_some()
                } {
                    return Err(Error::value("Double math script")
                        .with_issue(crate::core::recovery::Issue::MathStructure));
                }
            }
            self.defer(
                Request::Field(GroupKind::MathField),
                move |parser, _, _, value| {
                    {
                        let reader = frame.borrow();
                        let mut atoms = reader.atoms.borrow_mut();
                        let atom = atoms.last_mut().unwrap();
                        if sup {
                            atom.sup = value.field()?;
                        } else {
                            atom.sub = value.field()?;
                        }
                    }
                    parser.operations.push(Operation::ReadNext(frame));
                    Ok(())
                },
            );
            return Ok(());
        }
        match current.operation() {
            Some(Command::MathNonscript) => frame.borrow().atoms.borrow_mut().push(Atom {
                command: MathCommand::Nonscript,
                source: source.clone(),
                ..Atom::default()
            }),
            Some(Command::MathStyle) => {
                let style = match current.value {
                    CommandOperand::MathStyle(style) => style,
                    _ => return Err(Error::value("Invalid math style operand")),
                };
                frame.borrow().atoms.borrow_mut().push(Atom {
                    command: MathCommand::Style(style),
                    source: source.clone(),
                    ..Atom::default()
                });
            }
            Some(Command::MathAtom) => {
                self.append_field(
                    frame,
                    AtomKind::from_code(value)?,
                    MathCommand::Nucleus,
                    source.clone(),
                );
                return Ok(());
            }
            Some(Command::MathFraction) => {
                if frame.borrow().fraction.is_some() {
                    return Err(Error::value("Ambiguous generalized fraction")
                        .with_issue(crate::core::recovery::Issue::MathStructure));
                }
                self.defer(Request::Fraction(value), move |parser, _, _, value| {
                    let MathParserValue::Fraction {
                        left,
                        right,
                        thickness,
                    } = value
                    else {
                        return Err(Error::runtime("Invalid fraction operands"));
                    };
                    {
                        let mut reader = frame.borrow_mut();
                        let numerator = std::mem::take(&mut *reader.atoms.borrow_mut());
                        reader.fraction = Some(Atom {
                            kind: AtomKind::Inner,
                            nucleus: Some(Arc::new(MathField::List(numerator))),
                            command: MathCommand::Fraction {
                                thickness,
                                left,
                                right,
                                denominator: None,
                            },
                            source: source.clone(),
                            ..Atom::default()
                        });
                    }
                    parser.operations.push(Operation::ReadNext(frame));
                    Ok(())
                });
                return Ok(());
            }
            Some(Command::MathLeftRight) if value == 0 => {
                self.defer(
                    Request::Delimiter(false),
                    move |parser, context, services, value| {
                        let MathParserValue::Delimiter(left) = value else {
                            return Err(Error::runtime("Invalid left delimiter"));
                        };
                        services.begin_group(context, "math_left", None)?;
                        parser.defer(
                            Request::Read(MathStop::Right),
                            move |parser, context, services, value| {
                                let (atoms, closing) = value.read()?;
                                services.end_group(context, "math_left")?;
                                let right = closing.ok_or_else(|| {
                                    Error::value("Unmatched left delimiter")
                                        .with_issue(crate::core::recovery::Issue::MathDelimiter)
                                })?;
                                frame.borrow().atoms.borrow_mut().push(Atom {
                                    kind: AtomKind::Inner,
                                    nucleus: Some(Arc::new(MathField::List(atoms))),
                                    command: MathCommand::Delimited { left, right },
                                    source: source.clone(),
                                    ..Atom::default()
                                });
                                parser.operations.push(Operation::ReadNext(frame));
                                Ok(())
                            },
                        );
                        Ok(())
                    },
                );
                return Ok(());
            }
            Some(Command::MathLeftRight) => {
                return Err(Error::value("Unmatched right or middle delimiter")
                    .with_issue(crate::core::recovery::Issue::MathDelimiter));
            }
            Some(Command::MathConstruct) => {
                if value == 0 || value == 1 {
                    self.defer(
                        if value == 0 {
                            Request::Delimiter(true)
                        } else {
                            Request::External(MathParserAction::Scan(
                                crate::language::scanning::ScanRequest::Int,
                            ))
                        },
                        move |parser, _, _, operand| {
                            let command = match operand {
                                MathParserValue::Delimiter(code) if value == 0 => {
                                    MathCommand::Radical(code)
                                }
                                MathParserValue::Scan(code) if value == 1 => MathCommand::Accent(
                                    MathCharacterCode::from_source(code.integer()?)?,
                                ),
                                _ => {
                                    return Err(Error::runtime(
                                        "Invalid math construction operand",
                                    ));
                                }
                            };
                            parser.append_field(frame, AtomKind::Ordinary, command, source.clone());
                            Ok(())
                        },
                    );
                    return Ok(());
                }
                let command = match value {
                    2 => MathCommand::Overline,
                    3 => MathCommand::Underline,
                    4 => MathCommand::Vcenter,
                    _ => return Err(Error::value("Invalid math construction")),
                };
                if value == 4 {
                    self.defer(
                        Request::External(MathParserAction::ReadBox {
                            operation: BoxOperation::Vbox,
                            group: GroupKind::Vcenter,
                        }),
                        move |parser, context, _, value| {
                            let pointer = value.box_pointer()?;
                            context.state.held.push(pointer);
                            frame.borrow().atoms.borrow_mut().push(Atom {
                                nucleus: Some(Arc::new(MathField::Box(pointer))),
                                command,
                                source: source.clone(),
                                ..Atom::default()
                            });
                            parser.operations.push(Operation::ReadNext(frame));
                            Ok(())
                        },
                    );
                } else {
                    self.append_field(frame, AtomKind::Ordinary, command, source.clone());
                }
                return Ok(());
            }
            Some(Command::MathLimits) => {
                let reader = frame.borrow();
                let mut atoms = reader.atoms.borrow_mut();
                let atom = atoms
                    .last_mut()
                    .filter(|atom| atom.kind == AtomKind::Operator)
                    .ok_or_else(|| {
                        Error::value("Limits must follow a math operator")
                            .with_issue(crate::core::recovery::Issue::MathStructure)
                    })?;
                atom.limits = value;
            }
            Some(Command::MathChoice) => {
                self.choice_next(frame, Vec::new());
                return Ok(());
            }
            Some(Command::MathGlue) | Some(Command::MathKern) => {
                let glue = current.is_command(Command::MathGlue);
                let request = if glue {
                    crate::language::scanning::ScanRequest::Glue { mu: true }
                } else {
                    crate::language::scanning::ScanRequest::Dimen {
                        mu: true,
                        infinite: false,
                    }
                };
                self.defer(
                    Request::External(MathParserAction::Scan(request)),
                    move |parser, _, _, value| {
                        let MathParserValue::Scan(value) = value else {
                            return Err(Error::runtime("Invalid math spacing operand"));
                        };
                        let command = if glue {
                            MathCommand::MuGlue(match value {
                                crate::language::scanning::ScanValue::Glue(spec) => spec,
                                _ => return Err(Error::runtime("Invalid math glue result")),
                            })
                        } else {
                            MathCommand::MuKern(value.integer()?)
                        };
                        frame.borrow().atoms.borrow_mut().push(Atom {
                            command,
                            source: source.clone(),
                            ..Atom::default()
                        });
                        parser.operations.push(Operation::ReadNext(frame));
                        Ok(())
                    },
                );
                return Ok(());
            }
            Some(Command::ShiftBox) => {
                self.defer(
                    Request::External(MathParserAction::ExecuteToken { in_box: true }),
                    move |parser, context, _, _| {
                        let pointer = context
                            .typesetting
                            .builder
                            .take_list(context.typesetting.fonts, context.typesetting.env)?;
                        if pointer != C::NULL {
                            context.state.held.push(pointer);
                            frame.borrow().atoms.borrow_mut().push(Atom {
                                nucleus: Some(Arc::new(MathField::Box(pointer))),
                                source: source.clone(),
                                ..Atom::default()
                            });
                        }
                        parser.operations.push(Operation::ReadNext(frame));
                        Ok(())
                    },
                );
                return Ok(());
            }
            Some(Command::Align)
                if stop == MathStop::Shift && !context.typesetting.builder.context.restricted =>
            {
                if !frame.borrow().atoms.borrow().is_empty() || value != 0 {
                    return Err(Error::value(
                        "Display alignment must be the only formula material",
                    )
                    .with_issue(crate::core::recovery::Issue::MathStructure));
                }
                self.defer(
                    Request::External(MathParserAction::ReadAlignment { vertical: false }),
                    move |parser, context, _, value| {
                        let MathParserValue::Alignment(head) = value else {
                            return Err(Error::value("Unexpected alignment result"));
                        };
                        context
                            .state
                            .held
                            .extend(context.typesetting.builder.siblings(head)?);
                        frame.borrow().atoms.borrow_mut().push(Atom {
                            command: MathCommand::Alignment(head),
                            source: source.clone(),
                            ..Atom::default()
                        });
                        parser.operations.push(Operation::ReadNext(frame));
                        Ok(())
                    },
                );
                return Ok(());
            }
            Some(Command::MathEqno)
                if stop == MathStop::Shift && !context.typesetting.builder.context.restricted =>
            {
                services.begin_group(context, "math_eqno", None)?;
                context.typesetting.builder.push_context(
                    context.typesetting.fonts,
                    context.typesetting.env,
                    ListMode::Math,
                    true,
                )?;
                context.typesetting.env.assign_value(
                    ValueKey::parameter("fam"),
                    Quantity::Integer(-1),
                    false,
                );
                if let Some(Quantity::Tokens(tokens)) = context
                    .typesetting
                    .env
                    .values
                    .get(&ValueKey::parameter("everymath"))
                {
                    if !tokens.is_empty() {
                        let source = services.input().source();
                        services
                            .input_mut()
                            .begin_tokens(tokens.clone(), source, None)?;
                    }
                }
                let left = value != 0;
                self.defer(
                    Request::Read(MathStop::Shift),
                    move |parser, context, _services, value| {
                        let (atoms, _) = value.read()?;
                        context.typesetting.builder.pop_context()?;
                        parser.defer(
                            Request::External(MathParserAction::ExpandedToken),
                            move |parser, _, services, _| {
                                if !services.input().current.is_category(Cat::MathShift) {
                                    return Err(Error::value("Display math should end with $$")
                                        .with_issue(crate::core::recovery::Issue::MathStructure));
                                }
                                parser.defer(
                                    Request::External(MathParserAction::RenderEquation {
                                        atoms,
                                        style: MathStyle::Text,
                                    }),
                                    move |parser, context, services, value| {
                                        let MathParserValue::Rendered(nodes) = value else {
                                            return Err(Error::value(
                                                "Unexpected equation render result",
                                            ));
                                        };
                                        let mut math = MathContext {
                                            typesetting: context.typesetting,
                                            layout: context.layout,
                                        };
                                        let number = math.hbox(&nodes)?;
                                        context.state.held.push(number);
                                        services.end_group(context, "math_eqno")?;
                                        context.state.equation_number = Some((left, number));
                                        parser.finish_read(context, &frame, None);
                                        Ok(())
                                    },
                                );
                                Ok(())
                            },
                        );
                        Ok(())
                    },
                );
                return Ok(());
            }
            _ if current.is_category(Cat::Spacer) => {}
            _ if current.is_category(Cat::RightBrace) || current.is_category(Cat::MathShift) => {
                return Err(Error::value("Unbalanced math group")
                    .with_issue(crate::core::recovery::Issue::MathStructure));
            }
            _ => {
                let before = context.typesetting.builder.context.cur_list_tail;
                self.defer(
                    Request::External(MathParserAction::ExecuteToken { in_box: true }),
                    move |parser, context, _, value| {
                        let MathParserValue::Executed(continued) = value else {
                            return Err(Error::value("Unexpected math command result"));
                        };
                        if !continued {
                            return Err(Error::value("Document end in unfinished math shift")
                                .with_issue(crate::core::recovery::Issue::RunawayArgument));
                        }
                        if context.typesetting.builder.context.cur_list_tail != before {
                            let head = context
                                .typesetting
                                .builder
                                .take_list(context.typesetting.fonts, context.typesetting.env)?;
                            let pointers = context.typesetting.builder.siblings(head)?;
                            context.state.held.extend(pointers.iter().copied());
                            frame
                                .borrow()
                                .atoms
                                .borrow_mut()
                                .extend(pointers.into_iter().map(|pointer| Atom {
                                    command: MathCommand::Raw(pointer),
                                    source: source.clone(),
                                    ..Atom::default()
                                }));
                        }
                        parser.operations.push(Operation::ReadNext(frame));
                        Ok(())
                    },
                );
                return Ok(());
            }
        }
        self.operations.push(Operation::ReadNext(frame));
        Ok(())
    }
    fn choice_next(&mut self, frame: Rc<RefCell<ReadFrame>>, mut choices: Vec<Field>) {
        if choices.len() == 4 {
            let choices: [Field; 4] = choices.try_into().ok().unwrap();
            frame.borrow().atoms.borrow_mut().push(Atom {
                command: MathCommand::Choice(choices),
                source: None,
                ..Atom::default()
            });
            self.operations.push(Operation::ReadNext(frame));
            return;
        }
        self.defer(
            Request::Field(GroupKind::MathChoice),
            move |parser, _, _, value| {
                choices.push(value.field()?);
                parser.choice_next(frame, choices);
                Ok(())
            },
        );
    }
}
