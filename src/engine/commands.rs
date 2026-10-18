//! The shared executable command dispatcher and scoped list lifecycle.
use super::command_execution::{
    BoxIntent, BoxTask, CommandExecution, CommandProgress, CommandRequest, CommandStep,
    OperandTask, ParagraphTask, Prefix, RuleTask, ScalarTask, StartedCommand,
};
use super::evaluation::Request;
use super::*;
use crate::{
    core::enums::{CharacterCategory as Cat, Command as Cmd, GroupKind, ListMode, NodeKind},
    language::{
        assignments::AssignmentRequest,
        commands::traits,
        expansion::{ExpansionContext, ExpansionRequest},
        font_commands::FontCommands,
        font_execution::{FontExecution, FontKind},
        input::ExpandedInput,
        records::{CommandOperand as Operand, MeaningCommand, ValueIndex},
        services::{LanguageServices, ListServices},
    },
    typesetting::{
        box_commands::BoxServices,
        box_operands::{AccentExecution, DiscretionaryExecution, UnpackExecution},
        classic_lists::ClassicLists,
        math_layout::MathExecution,
        nodes::ArenaStateServices,
    },
};
impl EngineView<'_> {
    pub(super) fn append_character(&mut self, code: u32) -> Result<()> {
        let font = &self.fonts.items[self.fonts.current];
        if font.metric_mode() == "approximate"
            || font
                .materialization
                .resource_error
                .as_ref()
                .is_some_and(|e| {
                    e.starts_with("Invalid font encoding") || e.starts_with("Missing font encoding")
                })
        {
            return Err(Error::value(
                font.materialization
                    .resource_error
                    .clone()
                    .unwrap_or_else(|| format!("Font {} has no valid metrics", font.name)),
            ));
        }
        self.start_paragraph(true)?;
        self.fix_language(None)?;
        self.builder.handle_char(code, self.fonts, self.env)?;
        Ok(())
    }
    pub(super) fn finish_message(&mut self, text: String, error_message: bool) -> Result<()> {
        let mut text = crate::diagnostics::messages::noninteractive_message(text);
        if text.trim().is_empty() {
            if !error_message {
                return Ok(());
            }
            text = "TeX reported an error".into();
        }
        self.event(
            "message",
            json!({"text":text,"error":error_message,"source":self.core.input.source()}),
        );
        if error_message {
            self.core.failure_help = match self.env.values.get(&ValueKey::parameter("errhelp")) {
                Some(Quantity::Tokens(t)) => t.clone(),
                _ => Arc::from([]),
            };
            self.core.input.recover(
                crate::core::recovery::Issue::PackageError,
                Error::value(text),
                "Continued so the document or package can run its own recovery",
            )?;
            if !self.core.failure_help.is_empty() {
                let help = self.core.failure_help.clone();
                if let Ok(help) = crate::language::streams::display_tokens(&mut self.eval(), &help)
                {
                    let help = crate::diagnostics::messages::noninteractive_message(help);
                    if let Some(record) = self.core.input.recovery.pending.last_mut() {
                        Arc::make_mut(record.details.as_mut().unwrap())
                            .explanation
                            .push_str(&format!("\n{help}"));
                    }
                }
                self.core.failure_help = Arc::from([]);
            }
            return Ok(());
        }
        if self.core.interaction != 0 {
            crate::language::conversions::print_tex(
                &mut crate::resources::host::ConsoleWriter { error: false },
                &crate::language::conversions::printed_bytes(
                    &text,
                    self.parameter("newlinechar", 10),
                    self.core.config.eight_bit_output,
                    self.core.config.language_profile == LanguageProfile::Unicode,
                ),
                " ",
                self.core.config.language_profile == LanguageProfile::Unicode,
            )?
        }
        Ok(())
    }
    pub(super) fn begin_group_owned(&mut self, kind: &str, group: Option<GroupKind>) -> Result<()> {
        self.env.begin_group(kind, group)?;
        let level = self.env.cur_level;
        let source = self.core.input.source();
        self.event_if_requested(
            "group_begin",
            || json!({"group":kind,"level":level,"source":source}),
        );
        Ok(())
    }
    pub(super) fn end_group_owned(&mut self, kind: &str, flush: bool) -> Result<()> {
        if self.core.input.recovery.enabled()
            && self
                .env
                .groups
                .last()
                .is_none_or(|frame| frame.kind != kind)
        {
            self.core.input.recover(
                crate::core::recovery::Issue::ExtraCloser,
                Error::value("Extra or mismatched group closing token"),
                "Ignored this closing token",
            )?;
            return Ok(());
        }
        if flush {
            self.builder.flush_lig_stack(self.fonts, self.env, true)?
        }
        self.refresh_roots();
        let mut state = ArenaStateServices {
            builder: self.builder,
            error: None,
            restorations: Vec::new(),
        };
        let after = self
            .env
            .end_group(kind, &mut self.fonts.current, Some(&mut state))?;
        if let Some(error) = state.error {
            return Err(error);
        }
        if flush && !after.is_empty() {
            self.core
                .input
                .begin_tokens(after.into(), self.core.input.source(), None)?
        }
        let level = self.env.cur_level;
        let source = self.core.input.source();
        self.event_if_requested(
            "group_end",
            || json!({"group":kind,"level":level,"source":source}),
        );
        self.finish_grouped_reuse(flush);
        Ok(())
    }
    pub(super) fn finish_main_control(&mut self, explicit: bool) -> Result<()> {
        let mode = self.core.interaction;
        self.core
            .conditionals
            .finish(explicit, &mut |s| self.core.diagnostics.warning(s, mode))?;
        if !self.env.groups.is_empty() {
            if !explicit {
                return Err(Error::value("Unclosed group at document end")
                    .with_issue(crate::core::recovery::Issue::MissingBrace));
            }
            self.warning(&format!(
                "Document ended with {} unfinished group(s)",
                self.env.groups.len()
            ));
        }
        Ok(())
    }
    pub(super) fn command_operation(&mut self, in_box: bool, global: bool) -> Result<bool> {
        self.drive_request(CommandRequest::Command { in_box, global })
    }
    pub(super) fn drive_request(&mut self, request: CommandRequest) -> Result<bool> {
        stacker::maybe_grow(256 * 1024, 2 * 1024 * 1024, || {
            let mut execution = CommandExecution::from_request(request, self.core)?;
            loop {
                match execution.advance(self, usize::MAX) {
                    CommandProgress::Yielded => {}
                    CommandProgress::Complete(result) => return result,
                }
            }
        })
    }
    pub(super) fn start_command(
        &mut self,
        in_box: bool,
        mut global: bool,
    ) -> Result<StartedCommand> {
        if !global && self.try_grouped_reuse()? {
            self.core.local.rejected = true;
            self.core.local.anchors.clear();
            if let Some(frontier) = &mut self.core.local.frontier {
                frontier.rejected = true;
            }
            return Ok(StartedCommand {
                pending: None,
                step: CommandStep::Complete(true),
            });
        }
        self.core.input.resolve_primitive(self.env)?;
        let assignment = self
            .core
            .input
            .current
            .operation()
            .is_some_and(|c| traits(c).assignment);
        if assignment {
            global = self.env.assignment_global(global)
        }
        let mut pending = if assignment {
            self.core.assignments.after_assignment.take()
        } else {
            None
        };
        if self.core.input.current.is_command(Cmd::SetBox) {
            self.core.box_after_assignment = pending.take()
        }
        let step = self.dispatch(in_box, global)?;
        Ok(StartedCommand { pending, step })
    }
    fn dispatch(&mut self, in_box: bool, global: bool) -> Result<CommandStep> {
        let current = self.core.input.current.clone();
        if self.core.config.exact_local_updates
            && self.builder.context.vertical_paragraph
            && self.builder.context.mode == ListMode::Horizontal
            && matches!(
                current.category(),
                Some(Cat::Letter | Cat::OtherChar | Cat::Spacer)
            )
        {
            self.core.local.literal_fence(&self.core.input, self.env);
        }
        self.grouped_command();
        self.core.reuse.command(self.env, &current);
        if self.core.config.exact_local_updates {
            self.core.local.command(&current);
        }
        // Memo guards run without a material-preparation grant. A cached
        // consumer's whole-node predicate is an observation too.
        let _composition = matches!(
            current.category(),
            Some(Cat::Letter | Cat::OtherChar | Cat::Spacer)
        )
        .then(|| {
            self.builder
                .nodes
                .contract(crate::checked_material::Consumer::Preparation)
        });
        let command = current.operation();
        let category = current.category();
        let value = current.value.clone();
        let number = current.character().unwrap_or(0);
        #[cfg(feature = "latency-diagnostics")]
        if crate::output_diagnostics::detailed()
            && matches!(
                command,
                Some(Cmd::PdfExtension | Cmd::ShipOut | Cmd::OutStream)
            )
        {
            crate::output_diagnostics::event(
                "effect_dispatch",
                format!("{command:?}|operand={number}"),
            );
        }
        self.builder.source = self.core.input.source();
        if self.parameter("tracingcommands", 0) != 0 {
            let meaning = self.eval().meaning()?;
            self.event(
                "command",
                json!({"meaning":meaning,"mode":self.builder.context.mode}),
            );
        }
        let characters = matches!(category, Some(Cat::Letter | Cat::OtherChar))
            || matches!(command, Some(Cmd::CharGiven | Cmd::CharNum));
        let needs_par = characters
            || category == Some(Cat::MathShift)
            || matches!(
                command,
                Some(
                    Cmd::Hskip | Cmd::Accent | Cmd::Discretionary | Cmd::NoBoundary | Cmd::ExSpace
                )
            )
            || command == Some(Cmd::UnHbox)
                && matches!(
                    value,
                    Operand::Unpack(
                        crate::core::enums::UnpackOperation::Hbox
                            | crate::core::enums::UnpackOperation::Hcopy
                    )
                )
            || command == Some(Cmd::Rule) && number != 0;
        if self.builder.context.mode == ListMode::Vertical && needs_par {
            self.back_input()?;
            return Ok(CommandStep::Yield(CommandRequest::Paragraph(
                ParagraphTask::start(true, false),
            )));
        }
        if !characters && command != Some(Cmd::NoBoundary) {
            self.builder.flush_lig_stack(self.fonts, self.env, true)?
        }
        if let Some(command) = command.filter(|c| traits(*c).prefix) {
            return Ok(CommandStep::Yield(CommandRequest::Prefix(Prefix::new(
                command, in_box, global,
            ))));
        }
        if characters {
            if command == Some(Cmd::CharNum) {
                return Ok(CommandStep::Yield(CommandRequest::Scalar(
                    ScalarTask::character(),
                )));
            }
            self.append_character(number as u32)?;
            return Ok(CommandStep::Complete(true));
        }
        match current.command {
            MeaningCommand::Character(Cat::LeftBrace) => self.begin_group_owned("brace", None)?,
            MeaningCommand::Character(Cat::RightBrace) => self.end_group_owned("brace", true)?,
            MeaningCommand::Character(Cat::MathShift) => {
                return Ok(CommandStep::Yield(CommandRequest::Math(
                    MathExecution::default(),
                )));
            }
            MeaningCommand::Character(Cat::Spacer) => {
                if self.builder.context.mode == ListMode::Horizontal {
                    self.builder.handle_space(self.fonts, self.env, false)?
                }
            }
            MeaningCommand::Character(c) => {
                let label = match c {
                    Cat::TabMark => "alignment tab",
                    Cat::SupMark => "superscript",
                    Cat::SubMark => "subscript",
                    Cat::MacParam => "macro parameter",
                    _ => "character category",
                };
                return Err(Error::value(format!("Unsupported executable {label}")));
            }
            MeaningCommand::Operation(cmd) => match cmd {
                Cmd::EndCsName => {
                    self.core.input.recover(
                        crate::core::recovery::Issue::ExtraCloser,
                        Error::value("Extra endcsname outside control sequence name"),
                        "Ignored the extra endcsname",
                    )?;
                }
                Cmd::BeginGroup => self.begin_group_owned("explicit", None)?,
                Cmd::EndGroup => self.end_group_owned("explicit", true)?,
                Cmd::ParEnd => {
                    if self.builder.context.vertical_paragraph {
                        return Ok(CommandStep::Yield(CommandRequest::Paragraph(
                            ParagraphTask::finish(false),
                        )));
                    }
                    self.handle_par()?;
                }
                Cmd::Relax =>
                {
                    #[cfg(feature = "latency-diagnostics")]
                    if crate::output_diagnostics::enabled() {
                        let name = self.env.cs_name(current.control_sequence);
                        if matches!(
                            name.as_str(),
                            "RTDiagEndOutputdblcol" | "RTDiagEndShipoutcont"
                        ) {
                            crate::output_diagnostics::observe_copies(self.builder);
                        }
                        crate::output_diagnostics::marker(&name);
                    }
                }
                Cmd::EndJob => {
                    if !in_box {
                        return Ok(CommandStep::Yield(CommandRequest::Paragraph(
                            ParagraphTask::end(),
                        )));
                    }
                    return Ok(CommandStep::Complete(false));
                }
                Cmd::Dump => {
                    if !self.core.config.ini {
                        return Err(Error::value("Dump is allowed only during initialization"));
                    }
                    if in_box {
                        return Err(Error::value("Cannot dump a format inside a box"));
                    }
                    self.core.format_snapshot = Some(super::formats::capture(self)?);
                    return Ok(CommandStep::Complete(false));
                }
                Cmd::DefFont => {
                    let kind = if matches!(&value,Operand::Text(s)if s=="letterspace") {
                        FontKind::Letterspace
                    } else if number != 0 {
                        FontKind::Copy
                    } else {
                        FontKind::Define
                    };
                    return Ok(CommandStep::Yield(CommandRequest::Font(
                        FontExecution::new(kind, global),
                    )));
                }
                Cmd::PdfFontCommand => {
                    return Ok(CommandStep::Yield(CommandRequest::Font(
                        FontExecution::new(FontKind::Pdf(number as u8), false),
                    )));
                }
                Cmd::PrevGraf => {
                    self.optional_equals()?;
                    let n = self.scan_int()?;
                    if n < 0 {
                        return Err(Error::value("Negative prevgraf"));
                    }
                    self.vertical_context_mut()?.prev_graf = n
                }
                Cmd::PageDimen => {
                    self.optional_equals()?;
                    let n = self.scan_dimen()?;
                    self.pages.set_dimension(number as usize, n)?
                }
                Cmd::PageInt => {
                    self.optional_equals()?;
                    let n = self.scan_int()?;
                    match number {
                        2 => self.set_interaction(n)?,
                        0 => self.pages.dead_cycles = n,
                        _ => self.pages.insert_penalties = n,
                    }
                }
                Cmd::SetInteraction => self.set_interaction(number)?,
                Cmd::ParShape => {
                    self.optional_equals()?;
                    let n = self.scan_int()?;
                    if self
                        .builder
                        .node_limit
                        .is_some_and(|limit| n > limit as i64)
                    {
                        return Err(Error::value("Paragraph shape exceeds resource limit"));
                    }
                    let mut shape = Vec::new();
                    for _ in 0..n.max(0) {
                        shape.push((self.scan_dimen()?, self.scan_dimen()?))
                    }
                    self.env.assign_value(
                        ValueKey {
                            table: "parshape".into(),
                            index: ValueIndex::Number(0),
                        },
                        Quantity::Shape(shape),
                        global,
                    )
                }
                Cmd::Penalties => {
                    self.optional_equals()?;
                    let n = self.scan_int()?;
                    if self
                        .builder
                        .node_limit
                        .is_some_and(|limit| n > limit as i64)
                    {
                        return Err(Error::value("Penalty list exceeds resource limit"));
                    }
                    let mut penalties = Vec::new();
                    for _ in 0..n.max(0) {
                        penalties.push(self.scan_int()?)
                    }
                    self.env.assign_value(
                        ValueKey {
                            table: "penalties".into(),
                            index: ValueIndex::Number(number),
                        },
                        Quantity::Penalties(penalties),
                        global,
                    )
                }
                Cmd::Message => {
                    return Ok(CommandStep::Yield(CommandRequest::Operand(
                        OperandTask::message(number == 1),
                    )));
                }
                Cmd::CaseShift => {
                    let crate::language::expansion::ExpansionValue::SourcedTokens {
                        tokens,
                        sources,
                    } = self
                        .run_expansion(ExpansionRequest::GeneralTextSourced { expanded: false })?
                    else {
                        return Err(Error::runtime("Missing case-shift provenance"));
                    };
                    let table = if number != 0 { "uccode" } else { "lccode" };
                    let mut shifted = Vec::new();
                    for token in tokens.iter().copied() {
                        if let Some(code) =
                            crate::language::character_tokens::token_character(token).filter(|_| {
                                !crate::language::character_tokens::is_control_sequence(token)
                            })
                        {
                            let mapped = self.case_code(table, code)?;
                            shifted.push(if mapped != 0 {
                                crate::language::character_tokens::character_token(
                                    crate::language::character_tokens::token_category(token)
                                        .unwrap(),
                                    mapped,
                                )?
                            } else {
                                token
                            })
                        } else if let Some(cs) = token.control_sequence_id() {
                            if let Some(code) = self.env.active_character(cs) {
                                let mapped = self.case_code(table, code)?;
                                shifted.push(if mapped != 0 {
                                    Token::Packed(
                                        crate::language::character_tokens::CS_TOKEN_FLAG
                                            + self.env.character_symbol(mapped as u32, true, true),
                                    )
                                } else {
                                    token
                                })
                            } else {
                                shifted.push(token)
                            }
                        } else {
                            shifted.push(token)
                        }
                    }
                    self.core.input.begin_tokens(
                        shifted.into(),
                        self.core.input.source(),
                        Some(sources),
                    )?
                }
                Cmd::Xray => crate::diagnostics::classic::show(self, number)?,
                Cmd::InStream | Cmd::ReadStream | Cmd::OutStream => {
                    use crate::language::stream_execution::StreamRequest;
                    let request = match command {
                        Some(Cmd::InStream) => StreamRequest::Input {
                            opening: number != 0,
                        },
                        Some(Cmd::ReadStream) => StreamRequest::Read {
                            raw: number != 0,
                            global,
                        },
                        _ => StreamRequest::Output {
                            operation: number as u8,
                            immediate: false,
                        },
                    };
                    return Ok(CommandStep::Yield(CommandRequest::Evaluation(
                        Request::Stream(request),
                    )));
                }
                Cmd::Special => {
                    return Ok(CommandStep::Yield(CommandRequest::Evaluation(
                        Request::Special(number != 0),
                    )));
                }
                Cmd::PdfExtension => {
                    let Operand::Text(name) = value else {
                        return Err(Error::value("Invalid PDF extension"));
                    };
                    if matches!(name.as_str(), "pdfxform" | "pdfrefxform") {
                        return Ok(CommandStep::Yield(CommandRequest::FormDefinition(
                            super::command_execution::FormDefinitionTask::new(
                                name == "pdfrefxform",
                                false,
                            ),
                        )));
                    }
                    if matches!(name.as_str(), "pdfximage" | "pdfrefximage") {
                        return Ok(CommandStep::Yield(CommandRequest::Image(
                            super::command_execution::ImageTask::new(name == "pdfrefximage", false),
                        )));
                    }
                    if crate::language::pdf_navigation::OPERATIONS.contains(&name.as_str()) {
                        return Ok(CommandStep::Yield(CommandRequest::Navigation(
                            super::command_execution::NavigationTask::new(name),
                        )));
                    }
                    if crate::language::pdf_operand_execution::PdfOperandExecution::supports(&name)
                    {
                        return Ok(CommandStep::Yield(CommandRequest::PdfOperand(
                            super::command_execution::PdfOperandTask::new(name, false),
                        )));
                    }
                    crate::language::pdf_extensions::execute(self, &name, false)?
                }
                Cmd::PdfFontMap | Cmd::Mark => {
                    return Ok(CommandStep::Yield(CommandRequest::Operand(
                        if command == Some(Cmd::PdfFontMap) {
                            OperandTask::font_map(number)
                        } else {
                            OperandTask::mark(number != 0)
                        },
                    )));
                }
                Cmd::MathFamily => {
                    let family = self.scan_register(Some(
                        crate::language::code_validation::MATH_FAMILY_COUNT as i64 - 1,
                    ))?;
                    self.optional_equals()?;
                    let font = self.scan_font_ident()?;
                    self.env.assign_value(
                        ValueKey {
                            table: "mathfont".into(),
                            index: ValueIndex::Pair(number, family),
                        },
                        Quantity::Integer(font as i64),
                        global,
                    )
                }
                Cmd::Insert => {
                    return Ok(CommandStep::Yield(CommandRequest::BoxTask(BoxTask::new(
                        BoxIntent::Insert,
                    ))));
                }
                Cmd::Vadjust => {
                    if self.builder.context.mode == ListMode::Vertical {
                        return Err(Error::value("Vertical adjustment in vertical mode")
                            .with_issue(crate::core::recovery::Issue::Mode));
                    }
                    return Ok(CommandStep::Yield(CommandRequest::BoxTask(BoxTask::new(
                        BoxIntent::Adjust,
                    ))));
                }
                Cmd::Immediate => {
                    return Ok(CommandStep::Yield(CommandRequest::Operand(
                        OperandTask::immediate(),
                    )));
                }
                Cmd::SetFont => {
                    self.env
                        .assign_font(&mut self.fonts.current, number as usize, global);
                    self.event("font_select",json!({"font":number,"global_assignment":global,"source":self.core.input.source()}));
                }
                Cmd::FontInt => {
                    let code = self.core.input.current.character().unwrap_or(-1);
                    return Ok(CommandStep::Yield(CommandRequest::Font(
                        FontExecution::new(FontKind::Integer(code), false),
                    )));
                }
                Cmd::FontCode => {
                    return Ok(CommandStep::Yield(CommandRequest::Font(
                        FontExecution::new(FontKind::Code(value), false),
                    )));
                }
                Cmd::ItalCorr => {
                    if self.builder.context.mode == ListMode::Math {
                        let p = self.builder.new_kern(0)?;
                        self.builder.tail_append(p)?
                    } else if self.builder.context.mode != ListMode::Horizontal {
                        return Err(Error::value("Italic correction outside horizontal mode")
                            .with_issue(crate::core::recovery::Issue::Mode));
                    } else {
                        let tail = self.builder.context.cur_list_tail;
                        if tail != C::NULL {
                            let node = self.builder.node(tail)?.clone();
                            if matches!(node.node_type, NodeKind::Char | NodeKind::Ligature) {
                                let width = self.fonts.items[node.font].get_char_italic(node.char);
                                let p = self.builder.new_kern(width)?;
                                self.builder.node_mut(p)?.kern_kind = "explicit".into();
                                self.builder.tail_append(p)?
                            }
                        }
                    }
                }
                Cmd::Def => {
                    let Operand::Definition(operation) = value else {
                        return Err(Error::value("Invalid macro definition"));
                    };
                    return Ok(CommandStep::Yield(CommandRequest::Evaluation(
                        Request::Expand(ExpansionRequest::Define {
                            operation,
                            global,
                            long: false,
                            outer: false,
                            protected: false,
                        }),
                    )));
                }
                Cmd::Let => {
                    let font_name = |n| FontCommands::name(self.fonts, n);
                    Expander::handle_let(
                        &mut ExpansionContext {
                            input: &mut self.core.input,
                            env: self.env,
                            font_name: &font_name,
                        },
                        global,
                    )?
                }
                Cmd::SetBox => {
                    if !self.core.setbox_allowed {
                        return Err(Error::value("Improper setbox while constructing an accent"));
                    }
                    return Ok(CommandStep::Yield(CommandRequest::BoxTask(BoxTask::new(
                        BoxIntent::Set { global },
                    ))));
                }
                Cmd::Parameter
                | Cmd::Register
                | Cmd::RegAlias
                | Cmd::CodeAssign
                | Cmd::FontDimen
                | Cmd::SpaceFactor
                | Cmd::PrevDepth => {
                    return Ok(CommandStep::Yield(CommandRequest::Evaluation(
                        Request::Assignment(AssignmentRequest::Assign { global }),
                    )));
                }
                Cmd::Shorthand => {
                    return Ok(CommandStep::Yield(CommandRequest::Evaluation(
                        Request::Assignment(AssignmentRequest::Shorthand {
                            code: number as usize,
                            global,
                        }),
                    )));
                }
                Cmd::Arithmetic => {
                    return Ok(CommandStep::Yield(CommandRequest::Evaluation(
                        Request::Assignment(AssignmentRequest::Arithmetic {
                            operation: number as u8,
                            global,
                        }),
                    )));
                }
                Cmd::AfterAssignment => {
                    self.raw_token()?;
                    self.core.assignments.after_assignment = Some(self.core.input.current.token)
                }
                Cmd::AfterGroup => {
                    self.raw_token()?;
                    self.env.save_after(self.core.input.current.token)
                }
                Cmd::PartokenName => {
                    self.raw_token()?;
                    if self.core.input.current.control_sequence != 0 {
                        self.core.input.par_loc = self.core.input.current.control_sequence;
                        self.core.input.par_token = self.core.input.current.token
                    }
                }
                Cmd::SetRandomSeed => {
                    let n = self.scan_int()?;
                    self.core.random_numbers.reset(n.into())
                }
                Cmd::Kern => {
                    return Ok(CommandStep::Yield(CommandRequest::Scalar(
                        ScalarTask::kern(),
                    )));
                }
                Cmd::Penalty => {
                    return Ok(CommandStep::Yield(CommandRequest::Scalar(
                        ScalarTask::penalty(),
                    )));
                }
                Cmd::Discretionary => {
                    return Ok(CommandStep::Yield(CommandRequest::Discretionary(
                        DiscretionaryExecution::new(number == 1),
                    )));
                }
                Cmd::IgnoreSpaces => {
                    self.nonblank()?;
                    self.back_input()?
                }
                Cmd::RemoveItem => {
                    let Operand::NodeKind(kind) = value else {
                        return Err(Error::value("Invalid item kind"));
                    };
                    self.with_lists(|ctx, pages, math| {
                        ClassicLists::remove(ctx, pages, math, kind)
                    })?
                }
                Cmd::Leaders => {
                    return Ok(CommandStep::Yield(CommandRequest::Operand(
                        OperandTask::leaders(number),
                    )));
                }
                Cmd::Hskip => {
                    return Ok(CommandStep::Yield(CommandRequest::Scalar(
                        ScalarTask::hskip(number),
                    )));
                }
                Cmd::Vskip => {
                    return Ok(CommandStep::Yield(CommandRequest::Operand(
                        OperandTask::vskip(number),
                    )));
                }
                Cmd::Input => {
                    return Ok(CommandStep::Yield(CommandRequest::Evaluation(
                        Request::Input,
                    )));
                }
                Cmd::StartPar => {
                    if self.builder.context.mode == ListMode::Vertical {
                        return Ok(CommandStep::Yield(CommandRequest::Paragraph(
                            ParagraphTask::start(number != 0, false),
                        )));
                    } else if number == 1 {
                        let p = self.builder.new_null_box()?;
                        self.builder.node_mut(p)?.width = self.parameter("parindent", 0);
                        if self.builder.context.mode == ListMode::Horizontal {
                            self.builder.context.space_factor = 1000
                        }
                        self.builder.tail_append(p)?
                    }
                }
                Cmd::ShipOut => {
                    return Ok(CommandStep::Yield(CommandRequest::BoxTask(BoxTask::new(
                        BoxIntent::Ship,
                    ))));
                }
                Cmd::Accent => {
                    return Ok(CommandStep::Yield(CommandRequest::Accent(
                        AccentExecution::new(),
                    )));
                }
                Cmd::NoBoundary => {
                    self.start_paragraph(true)?;
                    self.builder.flush_lig_stack(self.fonts, self.env, false)?;
                    self.expanded_token()?;
                    self.builder.context.cancel_boundary = matches!(
                        self.core.input.current.category(),
                        Some(Cat::Letter | Cat::OtherChar)
                    ) || matches!(
                        self.core.input.current.operation(),
                        Some(Cmd::CharGiven | Cmd::CharNum)
                    );
                    self.back_input()?
                }
                Cmd::ShiftBox => {
                    return Ok(CommandStep::Yield(CommandRequest::Operand(
                        OperandTask::shift(number as u8),
                    )));
                }
                Cmd::Rule => {
                    return Ok(CommandStep::Yield(CommandRequest::Rule(RuleTask::new(
                        number != 0,
                    ))));
                }
                Cmd::Align => {
                    return Ok(CommandStep::Yield(CommandRequest::Alignment {
                        vertical: number != 0,
                        enter: true,
                    }));
                }
                Cmd::MakeBox => {
                    let Operand::Box(_operation) = value else {
                        return Err(Error::value("Invalid box operation"));
                    };
                    let _name = self.env.cs_name(current.control_sequence);
                    self.back_input()?;
                    let adjusted = self.builder.context.mode == ListMode::Vertical;
                    return Ok(CommandStep::Yield(CommandRequest::BoxTask(BoxTask::new(
                        BoxIntent::Make { adjusted },
                    ))));
                }
                Cmd::UnHbox => {
                    let Operand::Unpack(op) = value else {
                        return Err(Error::value("Invalid unpack operation"));
                    };
                    return Ok(CommandStep::Yield(CommandRequest::Unpack(
                        UnpackExecution::new(op),
                    )));
                }
                Cmd::BoxDimen => {
                    #[cfg(feature = "latency-diagnostics")]
                    let _timing = crate::output_diagnostics::operation("box_dimension");
                    let n = self.scan_register(None)?;
                    self.optional_equals()?;
                    let amount = self.scan_dimen()?;
                    let p = self.env.box_registers.get(&n).copied().unwrap_or(C::NULL);
                    #[cfg(feature = "latency-diagnostics")]
                    if crate::output_diagnostics::detailed() {
                        crate::output_diagnostics::event(
                            "box_dimension_set",
                            format!(
                                "register={n}|dimension={number}|value={amount}|void={}",
                                p == C::NULL
                            ),
                        );
                    }
                    if p != C::NULL {
                        let node = self.builder.node_mut(p)?;
                        match number {
                            0 => node.width = amount,
                            1 => node.height = amount,
                            _ => node.depth = amount,
                        }
                    }
                }
                Cmd::HyphData => {
                    let mut h = std::mem::take(self.hyphenation);
                    let result = h.read(self, number != 0);
                    *self.hyphenation = h;
                    result?
                }
                Cmd::SetLanguage => {
                    let n = self.scan_int()?;
                    self.fix_language(Some(n))?
                }
                Cmd::ResetTimer => self.core.extended.reset_timer(),
                Cmd::GlyphToUnicode => {
                    let u = std::mem::take(&mut self.core.pdf_utilities);
                    let result = crate::language::pdf_commands::PDFUtilities::define_unicode(
                        &mut self.eval(),
                    );
                    self.core.pdf_utilities = u;
                    result?
                }
                Cmd::ExSpace => {
                    self.start_paragraph(true)?;
                    self.builder.handle_space(self.fonts, self.env, true)?
                }
                Cmd::MathCharNum => {
                    return Err(Error::value("Math character outside supported math mode"));
                }
                Cmd::UndefinedCs => {
                    return Err(Error::value(format!(
                        "Undefined control sequence: {}",
                        self.env.cs_name(current.control_sequence)
                    )));
                }
                _ => {
                    return Err(Error::value(format!(
                        "Unsupported executable command {cmd:?}: {}",
                        self.env.cs_name(current.control_sequence)
                    )));
                }
            },
        }
        Ok(CommandStep::Complete(true))
    }
    fn case_code(&mut self, table: &str, code: u32) -> Result<i64> {
        let mut warn = |_: &str| {};
        let mut ctx = TypesettingContext {
            builder: self.builder,
            fonts: self.fonts,
            env: self.env,
            warn: &mut warn,
        };
        let ctx = crate::language::assignments::AssignmentContext {
            typesetting: &mut ctx,
            input: &mut self.core.input,
        };
        let value = Assignments::get(
            &ctx,
            crate::language::records::QuantityKind::Count,
            &ValueKey {
                table: table.into(),
                index: ValueIndex::Number(code as i64),
            },
        )?;
        if let Quantity::Integer(value) = value {
            Ok(value)
        } else {
            Err(Error::runtime("Invalid case mapping"))
        }
    }
    pub(super) fn source_directory(&self) -> std::path::PathBuf {
        self.core
            .input
            .cursor
            .name
            .as_deref()
            .and_then(|n| Path::new(n).parent())
            .unwrap_or(Path::new("."))
            .to_path_buf()
    }
    fn set_interaction(&mut self, n: i64) -> Result<()> {
        if !(0..=3).contains(&n) {
            return Err(Error::value("Invalid interaction mode"));
        }
        self.core.interaction = n as u8;
        self.event("interaction", json!({"mode":n}));
        Ok(())
    }
    pub(super) fn vertical_context_mut(
        &mut self,
    ) -> Result<&mut crate::typesetting::nodes::ListContext> {
        if self.builder.context.mode == ListMode::Vertical {
            return Ok(&mut self.builder.context);
        }
        self.builder
            .context_stack
            .iter_mut()
            .rev()
            .find(|c| c.mode == ListMode::Vertical)
            .ok_or_else(|| Error::value("Paragraph without enclosing vertical list"))
    }
    pub(super) fn fix_language(&mut self, n: Option<i64>) -> Result<()> {
        let mut warnings = Vec::new();
        let mut ctx = TypesettingContext {
            builder: self.builder,
            fonts: self.fonts,
            env: self.env,
            warn: &mut |s| warnings.push(s.to_string()),
        };
        self.hyphenation
            .fix_language(&mut ctx, self.core.config.language_profile, n)?;
        for s in warnings {
            self.warning(&s)
        }
        Ok(())
    }
}
impl crate::typesetting::hyphenation::HyphenationInput for EngineView<'_> {
    fn environment(&self) -> &Environment {
        self.env
    }
    fn scan_char_num(&mut self) -> Result<i64> {
        LanguageServices::scan_char_num(self).map(i64::from)
    }
}
