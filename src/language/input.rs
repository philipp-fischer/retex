//! One physical/token cursor with live meanings (`language/input.py`).
use super::character_tokens::{Token, character_token, token_category};
use super::input_frames::PhysicalStream;
use super::records::{CommandOperand, InputState, MeaningCommand, Quantity, ValueIndex, ValueKey};
use super::state::Environment;
use crate::core::capacities::Capacities;
use crate::core::characters::{LanguageProfile, unicode_category};
use crate::core::constants::Constants as C;
use crate::core::enums::{CharacterCategory as Cat, Command, InputStateKind};
use crate::core::errors::{Error, Result};
use crate::core::persistence::PersistentComponent;
use crate::core::source::SourceSpan;
use crate::resources::search::InputResources;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Write;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScannerStatus {
    #[default]
    Normal,
    Defining,
    Matching,
    Absorbing,
    Aligning,
    Skipping,
}
impl ScannerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Defining => "defining",
            Self::Matching => "matching",
            Self::Absorbing => "absorbing",
            Self::Aligning => "aligning",
            Self::Skipping => "skipping",
        }
    }
}
#[derive(Clone, Debug)]
pub struct CurrentToken {
    pub command: MeaningCommand,
    pub value: CommandOperand,
    pub control_sequence: i64,
    pub token: Token,
}
impl Default for CurrentToken {
    fn default() -> Self {
        Self {
            command: MeaningCommand::Operation(Command::Relax),
            value: CommandOperand::Number(0),
            control_sequence: 0,
            token: Token::Packed(0),
        }
    }
}
impl CurrentToken {
    pub fn character(&self) -> Option<i64> {
        match self.value {
            CommandOperand::Number(value) => Some(value),
            _ => None,
        }
    }
    pub fn category(&self) -> Option<Cat> {
        match self.command {
            MeaningCommand::Character(category) => Some(category),
            _ => None,
        }
    }
    pub fn operation(&self) -> Option<Command> {
        match self.command {
            MeaningCommand::Operation(command) => Some(command),
            _ => None,
        }
    }
    pub fn is_category(&self, category: Cat) -> bool {
        self.category() == Some(category)
    }
    pub fn is_command(&self, command: Command) -> bool {
        self.operation() == Some(command)
    }
    pub fn literal_category(&self, category: Cat) -> bool {
        token_category(self.token) == Some(category)
    }
}
#[derive(Clone, Debug)]
pub enum InputEvent {
    Token {
        current: CurrentToken,
        name: String,
        source: Option<Arc<SourceSpan>>,
        count: usize,
    },
    ScannerState {
        status: ScannerStatus,
        name: String,
        source: Option<Arc<SourceSpan>>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputSnapshot {
    pub catcodes: Vec<Cat>,
    pub par_loc: i64,
    pub par_token: Token,
}
pub struct InputProcessor {
    pub profile: LanguageProfile,
    pub ini: bool,
    pub capacities: Capacities,
    pub cursor: InputState,
    pub first: usize,
    pub max_buf_stack: usize,
    pub input_stack: Vec<InputState>,
    pub max_in_stack: usize,
    /// Actual observed frame depth for scheduling/ownership measurements.
    /// max_in_stack retains its legacy reference representation.
    pub peak_frame_depth: usize,
    pub in_open: usize,
    pub open_parens: usize,
    pub input_file: Vec<Option<Box<dyn PhysicalStream>>>,
    pub current: CurrentToken,
    pub cat_code: Vec<Cat>,
    pub align_state: i64,
    pub par_loc: i64,
    pub par_token: Token,
    pub log_file: Option<Box<dyn Write>>,
    pub log_opened: bool,
    pub token_count: usize,
    pub end_input_requested: bool,
    pub eof_seen: BTreeSet<usize>,
    pub job_name: Option<String>,
    pub scanning_filename: bool,
    pub scanner_status: ScannerStatus,
    pub resources: InputResources,
    pub warning_cs: i64,
    pub source_occurrences: BTreeMap<String, u64>,
    pub input_occurrences: Vec<u64>,
    pub source_resources: BTreeMap<String, String>,
    pub last_input_error: Option<String>,
    pub(crate) last_missing_resource: Option<crate::core::errors::MissingResource>,
    pub recovery: crate::core::recovery::RecoveryState,
    pub events: Vec<InputEvent>,
    pub tracing: bool,
    pub read_line_only: bool,
    pub alignment_handler:
        Option<std::rc::Rc<std::cell::RefCell<crate::typesetting::alignments::EntryHandler>>>,
}
impl InputProcessor {
    pub(crate) fn checkpoint_clone(&self) -> Option<Self> {
        if self.log_file.is_some() || self.alignment_handler.is_some() {
            return None;
        }
        let mut files = Vec::with_capacity(self.input_file.len());
        for stream in &self.input_file {
            files.push(match stream {
                None => None,
                Some(stream) => Some(stream.fork()?),
            });
        }
        Some(Self {
            profile: self.profile.clone(),
            ini: self.ini.clone(),
            capacities: self.capacities.clone(),
            cursor: self.cursor.clone(),
            first: self.first.clone(),
            max_buf_stack: self.max_buf_stack.clone(),
            input_stack: self.input_stack.clone(),
            max_in_stack: self.max_in_stack.clone(),
            peak_frame_depth: self.peak_frame_depth.clone(),
            in_open: self.in_open.clone(),
            open_parens: self.open_parens.clone(),
            input_file: files,
            current: self.current.clone(),
            cat_code: self.cat_code.clone(),
            align_state: self.align_state.clone(),
            par_loc: self.par_loc.clone(),
            par_token: self.par_token.clone(),
            log_file: None,
            log_opened: self.log_opened.clone(),
            token_count: self.token_count.clone(),
            end_input_requested: self.end_input_requested.clone(),
            eof_seen: self.eof_seen.clone(),
            job_name: self.job_name.clone(),
            scanning_filename: self.scanning_filename.clone(),
            scanner_status: self.scanner_status.clone(),
            resources: self.resources.clone(),
            warning_cs: self.warning_cs.clone(),
            source_occurrences: self.source_occurrences.clone(),
            input_occurrences: self.input_occurrences.clone(),
            source_resources: self.source_resources.clone(),
            last_input_error: self.last_input_error.clone(),
            last_missing_resource: self.last_missing_resource.clone(),
            recovery: self.recovery.clone(),
            events: self.events.clone(),
            tracing: self.tracing.clone(),
            read_line_only: self.read_line_only.clone(),
            alignment_handler: None,
        })
    }

    pub fn new(
        profile: LanguageProfile,
        ini: bool,
        capacities: Capacities,
        resources: InputResources,
    ) -> Self {
        Self {
            profile,
            ini,
            capacities,
            cursor: InputState {
                buffer: Some(Arc::new(vec![0])),
                columns: Some(Arc::new(vec![1])),
                byte_columns: Some(Arc::new(vec![1])),
                ..InputState::default()
            },
            first: 0,
            max_buf_stack: 0,
            input_stack: Vec::new(),
            max_in_stack: 0,
            peak_frame_depth: 0,
            in_open: 0,
            open_parens: 0,
            input_file: Vec::new(),
            current: CurrentToken::default(),
            cat_code: vec![Cat::OtherChar; 256],
            align_state: 0,
            par_loc: C::NULL_CS,
            par_token: Token::Packed(0),
            log_file: None,
            log_opened: false,
            token_count: 0,
            end_input_requested: false,
            eof_seen: BTreeSet::new(),
            job_name: None,
            scanning_filename: false,
            scanner_status: ScannerStatus::Normal,
            resources,
            warning_cs: 0,
            source_occurrences: BTreeMap::new(),
            input_occurrences: Vec::new(),
            source_resources: BTreeMap::new(),
            last_input_error: None,
            last_missing_resource: None,
            recovery: Default::default(),
            events: Vec::new(),
            tracing: false,
            read_line_only: false,
            alignment_handler: None,
        }
    }
    pub fn recover(
        &mut self,
        issue: crate::core::recovery::Issue,
        error: Error,
        action: &str,
    ) -> Result<()> {
        self.recovery.report(issue, error, self.source(), action)
    }
    pub fn recover_with_fix(
        &mut self,
        issue: crate::core::recovery::Issue,
        error: Error,
        action: &str,
        replacement: &str,
    ) -> Result<()> {
        let source = self.source();
        self.recover(issue, error, action)?;
        if let Some(source) =
            source.filter(|s| s.byte_column.is_some() && s.end_byte_column.is_some())
        {
            let mut point = (*source).clone();
            point.end_byte_column = point.byte_column;
            if let Some(record) = self.recovery.pending.last_mut() {
                Arc::make_mut(record.details.as_mut().unwrap()).fixes.push(
                    crate::core::recovery::SuggestedEdit {
                        title: format!("Insert {replacement}"),
                        source: Arc::new(point),
                        replacement: replacement.into(),
                    },
                );
            }
        }
        Ok(())
    }
    pub fn source(&self) -> Option<Arc<SourceSpan>> {
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::count("source_requests", 1);
        self.cursor.source.clone()
    }
    pub fn integer(env: &Environment, key: &ValueKey, default: i64) -> i64 {
        let result = match env.values.get(key) {
            Some(Quantity::Integer(value)) => *value,
            _ => default,
        };
        #[cfg(feature = "latency-diagnostics")]
        if crate::output_diagnostics::detailed() {
            crate::output_diagnostics::event("integer_read", format!("{key:?}={result}"));
        }
        result
    }
    pub fn parameter(env: &Environment, name: &str, default: i64) -> i64 {
        Self::integer(env, &ValueKey::parameter(name), default)
    }
    pub fn scoped_scan<T>(
        &mut self,
        env: &mut Environment,
        status: ScannerStatus,
        warning_cs: Option<i64>,
        scan: impl FnOnce(&mut Self, &mut Environment) -> Result<T>,
    ) -> Result<T> {
        let previous = (self.scanner_status, self.warning_cs);
        self.scanner_status = status;
        if let Some(warning_cs) = warning_cs {
            self.warning_cs = warning_cs;
        }
        self.scanner_event(env);
        let result = scan(self, env);
        (self.scanner_status, self.warning_cs) = previous;
        self.scanner_event(env);
        result
    }
    fn scanner_event(&mut self, env: &Environment) {
        if self.tracing {
            self.events.push(InputEvent::ScannerState {
                status: self.scanner_status,
                name: env.cs_name(self.warning_cs),
                source: self.source(),
            });
        }
    }
    pub fn check_scanner_validity(&self, env: &Environment, file_end: bool) -> Result<()> {
        if self.scanner_status == ScannerStatus::Normal {
            return Ok(());
        }
        if file_end
            || (self.current.is_command(Command::Macro)
                && matches!(&self.current.value, CommandOperand::Macro(definition) if definition.outer))
        {
            let eof_message = match self.scanner_status {
                ScannerStatus::Defining => "Unexpected EOF in macro definition",
                ScannerStatus::Matching => "Unexpected EOF in macro argument",
                ScannerStatus::Absorbing => "Unexpected EOF in balanced token list",
                ScannerStatus::Aligning => "Unexpected EOF in alignment preamble",
                ScannerStatus::Skipping => "Incomplete conditional at EOF",
                ScannerStatus::Normal => unreachable!(),
            };
            let subject = if file_end {
                format!("{eof_message}: File ended")
            } else {
                format!(
                    "Forbidden outer control sequence {}",
                    env.cs_name(self.current.control_sequence)
                )
            };
            let target = env.cs_name(self.warning_cs);
            return Err(Error::value(format!(
                "{subject} while scanning {}{}",
                self.scanner_status.as_str(),
                if target.is_empty() {
                    String::new()
                } else {
                    format!(" of {target}")
                }
            ))
            .with_issue(crate::core::recovery::Issue::RunawayArgument));
        }
        Ok(())
    }
    pub fn load_meaning(&mut self, env: &mut Environment) {
        if let Some((command, value)) = env.meaning_value(self.current.control_sequence) {
            self.current.command = command;
            self.current.value = value;
        } else {
            self.current.command = MeaningCommand::Operation(Command::UndefinedCs);
            self.current.value = CommandOperand::Number(C::NULL);
        }
    }
    pub fn resolve_primitive(&mut self, env: &mut Environment) -> Result<()> {
        while self.current.is_command(Command::IgnoreSpaces) && self.current.character() == Some(1)
        {
            self.get_token(env)?;
            let public = self.current.control_sequence;
            let frozen = match env.frozen_primitives.get(&public).copied() {
                Some(frozen) => frozen,
                None => self
                    .primitive_token(env, Command::Relax, &CommandOperand::Number(0))?
                    .control_sequence_id()
                    .unwrap(),
            };
            self.current.control_sequence = frozen;
            self.current.token = Token::control_sequence(frozen);
            self.load_meaning(env);
        }
        Ok(())
    }
    pub fn primitive_token(
        &self,
        env: &mut Environment,
        command: Command,
        value: &CommandOperand,
    ) -> Result<Token> {
        let primitives: Vec<_> = env
            .frozen_primitives
            .iter()
            .map(|(&public, &frozen)| (public, frozen))
            .collect();
        for (public, frozen) in primitives {
            if env.original_primitive(public).is_some_and(|meaning| {
                meaning.command == MeaningCommand::Operation(command) && &meaning.value == value
            }) {
                return Ok(Token::control_sequence(frozen));
            }
        }
        Err(Error::value(format!(
            "Missing primitive meaning: {command}"
        )))
    }
    pub fn initialize_cat_codes(&mut self) {
        for code in 0..256 {
            self.cat_code[code] = if self.profile == LanguageProfile::Unicode {
                unicode_category(code as u32)
            } else {
                Cat::OtherChar
            };
        }
        for (code, category) in [
            (13, Cat::CarRet),
            (32, Cat::Spacer),
            (92, Cat::Escape),
            (37, Cat::Comment),
            (0, Cat::Ignore),
            (127, Cat::InvalidChar),
        ] {
            self.cat_code[code] = category;
        }
        if !self.ini {
            for (code, category) in [
                (9, Cat::Spacer),
                (123, Cat::LeftBrace),
                (125, Cat::RightBrace),
                (35, Cat::MacParam),
                (36, Cat::MathShift),
                (94, Cat::SupMark),
                (95, Cat::SubMark),
                (38, Cat::TabMark),
            ] {
                self.cat_code[code] = category;
            }
        }
        for code in 65..=90 {
            self.cat_code[code] = Cat::Letter;
            self.cat_code[code + 32] = Cat::Letter;
        }
    }
    pub fn category_code(&self, env: &Environment, code: u32) -> Result<Cat> {
        if let Some(Quantity::Integer(value)) = env.values.get(&ValueKey {
            table: "catcode".into(),
            index: ValueIndex::Number(code as i64),
        }) {
            return Cat::from_code(*value).ok_or_else(|| Error::value("Invalid category code"));
        }
        Ok(self
            .cat_code
            .get(code as usize)
            .copied()
            .unwrap_or_else(|| unicode_category(code)))
    }
    /// A peek can select the ordinary literal path only when get_token cannot
    /// cross an input boundary, expand a control sequence or intercept alignment.
    pub(crate) fn next_is_literal_character(&self, env: &Environment) -> bool {
        if self.alignment_handler.is_some() {
            return false;
        }
        let category = if self.cursor.state == InputStateKind::TokenList {
            self.cursor
                .token_list
                .as_ref()
                .and_then(|tokens| tokens.get(self.cursor.token_index))
                .and_then(|&token| super::character_tokens::token_category(token))
        } else if self.in_open != 0 && self.cursor.loc as isize <= self.cursor.limit {
            self.cursor
                .buffer
                .as_ref()
                .and_then(|buffer| buffer.get(self.cursor.loc))
                .and_then(|&code| self.category_code(env, code).ok())
        } else {
            None
        };
        matches!(category, Some(Cat::Letter | Cat::OtherChar))
    }
    pub fn get_token(&mut self, env: &mut Environment) -> Result<()> {
        self.get_next(env)?;
        Ok(())
    }
    pub fn get_next(&mut self, env: &mut Environment) -> Result<()> {
        let mut replacements = 0;
        loop {
            loop {
                if self.in_open == 0 && self.cursor.state != InputStateKind::TokenList {
                    return Err(super::input_frames::end_of_input());
                }
                self.current.control_sequence = 0;
                let available = if self.cursor.state != InputStateKind::TokenList {
                    self.input_from_file(env)?
                } else {
                    self.input_from_token_list(env)?
                };
                if available {
                    break;
                }
            }
            self.current.token = if self.current.control_sequence != 0 {
                Token::control_sequence(self.current.control_sequence)
            } else {
                character_token(
                    self.current
                        .category()
                        .ok_or_else(|| Error::value("Missing character category"))?,
                    self.current
                        .character()
                        .ok_or_else(|| Error::value("Missing character code"))?,
                )?
            };
            self.check_scanner_validity(env, false)?;
            if self.current.control_sequence == 0 {
                if self.current.is_category(Cat::LeftBrace) {
                    self.align_state += 1;
                } else if self.current.is_category(Cat::RightBrace) {
                    self.align_state -= 1;
                }
            }
            let handler = self.alignment_handler.clone();
            if let Some(handler) = handler {
                if handler.borrow_mut().intercept(self)? {
                    replacements += 1;
                    continue;
                }
            }
            break;
        }
        for _ in 0..=replacements {
            self.token_count += 1;
            if self.tracing {
                self.events.push(InputEvent::Token {
                    current: self.current.clone(),
                    name: env.cs_name(self.current.control_sequence),
                    source: self.source(),
                    count: self.token_count,
                });
            }
        }
        Ok(())
    }
}
impl PersistentComponent for InputProcessor {
    type State = InputSnapshot;
    fn dump_state(&self) -> Self::State {
        InputSnapshot {
            catcodes: self.cat_code.clone(),
            par_loc: self.par_loc,
            par_token: self.par_token,
        }
    }
    fn restore_state(&mut self, state: Self::State) -> Result<()> {
        if state.catcodes.len() != 256 {
            return Err(Error::value("Invalid format category table"));
        }
        self.cat_code = state.catcodes;
        self.par_loc = state.par_loc;
        self.par_token = state.par_token;
        Ok(())
    }
}

/// The engine supplies expansion and alignment dispatch around this raw cursor.
pub trait ExpandedInput {
    fn input(&self) -> &InputProcessor;
    fn input_mut(&mut self) -> &mut InputProcessor;
    fn expanded_token(&mut self) -> Result<()>;
    fn raw_token(&mut self) -> Result<()>;
    fn back_input(&mut self) -> Result<()>;
    fn back_token(&mut self, token: Token, source: Option<Arc<SourceSpan>>) -> Result<()>;
}
pub fn scan_left_brace(input: &mut impl ExpandedInput) -> Result<()> {
    loop {
        input.expanded_token()?;
        let current = &input.input().current;
        if !current.is_category(Cat::Spacer) && !current.is_command(Command::Relax) {
            break;
        }
    }
    if !input.input().current.is_category(Cat::LeftBrace) {
        return Err(Error::value("Expected a left brace")
            .with_issue(crate::core::recovery::Issue::MissingBrace));
    }
    Ok(())
}
pub fn scan_filename(input: &mut impl ExpandedInput) -> Result<OsString> {
    use super::filename::{FilenameAction, FilenameExecution, FilenameStep};
    let mut execution = FilenameExecution::default();
    let mut returned = None;
    loop {
        match execution.resume(input.input_mut(), returned.take())? {
            FilenameStep::Complete(name) => return Ok(name),
            FilenameStep::Yield(action) => {
                returned = Some(match action {
                    FilenameAction::ExpandedToken => input.expanded_token(),
                    FilenameAction::BackInput => input.back_input(),
                });
            }
        }
    }
}
