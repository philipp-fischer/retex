//! Typed language, document and command adapters over short-lived component borrows.
use super::evaluation::{Request, Value};
use super::*;
use crate::{
    core::{
        enums::{CharacterCategory as Cat, Command},
        values::GlueSpec,
    },
    language::{
        assignments::{AssignmentRequest, AssignmentValue},
        conversions::TokenPrinter,
        expansion::{ExpansionContext, ExpansionRequest, ExpansionValue},
        expressions::{self, ExpressionInput},
        extended::ExtendedServices,
        font_commands::{FontCommandServices, FontCommands},
        input::ExpandedInput,
        internal_values::{InternalServices, InternalValues},
        pdf_commands::PdfUtilityServices,
        records::{QuantityKind, ValueIndex},
        scanning::{InternalValue, ScanRequest, ScanValue},
        services::{LanguageServices, ListServices},
        streams::StreamServices,
    },
    pdf::{
        document::{DocumentServices, PDFDocument},
        graphics::ColorStack,
        images::ImageServices,
        imports::{PDFImportGraph, SourceKey},
        parsing::PdfValue,
        resources::PDFResources,
    },
    typesetting::{classic_lists::ClassicLists, penalties::Penalties},
};
use num_traits::ToPrimitive;
macro_rules! common_language {
    ($ty:ident) => {
        impl ExpandedInput for $ty<'_> {
            fn input(&self) -> &InputProcessor {
                &self.core.input
            }
            fn input_mut(&mut self) -> &mut InputProcessor {
                &mut self.core.input
            }
            fn expanded_token(&mut self) -> Result<()> {
                self.run_expansion(ExpansionRequest::XToken { protected: false })?;
                Ok(())
            }
            fn raw_token(&mut self) -> Result<()> {
                self.core.input.get_token(self.env)?;
                Ok(())
            }
            fn back_input(&mut self) -> Result<()> {
                self.core.input.back_input()
            }
            fn back_token(
                &mut self,
                token: Token,
                source: Option<Arc<crate::core::source::SourceSpan>>,
            ) -> Result<()> {
                self.core.input.back_token(token, source)
            }
        }
        impl LanguageServices for $ty<'_> {
            fn event(&mut self, kind: &str, fields: serde_json::Value) {
                $ty::event(self, kind, fields)
            }
            fn env(&self) -> &Environment {
                self.env
            }
            fn env_mut(&mut self) -> &mut Environment {
                self.env
            }
            fn fonts(&self) -> &FontCatalog {
                self.fonts
            }
            fn fonts_mut(&mut self) -> &mut FontCatalog {
                self.fonts
            }
            fn scan(&mut self, request: ScanRequest) -> Result<ScanValue> {
                self.run_scan(request)
            }
            fn general_text(&mut self, expanded: bool) -> Result<Arc<[Token]>> {
                self.run_expansion(ExpansionRequest::GeneralText { expanded })?
                    .tokens()
            }
            fn get_r_token(&mut self) -> Result<()> {
                let font_name = |n| FontCommands::name(self.fonts, n);
                let mut context = ExpansionContext {
                    input: &mut self.core.input,
                    env: self.env,
                    font_name: &font_name,
                };
                Expander::get_r_token(&mut context)
            }
            fn text(&mut self) -> Result<String> {
                let tokens = self.general_text(true)?;
                crate::language::streams::display_tokens(self, &tokens)
            }
        }
        impl ListServices for $ty<'_> {
            fn builder(&self) -> &NodeBuilder {
                self.builder
            }
            fn builder_mut(&mut self) -> &mut NodeBuilder {
                self.builder
            }
            fn display_token(&mut self, token: Token) -> Result<String> {
                let name = |n| FontCommands::name(self.fonts, n);
                TokenPrinter {
                    input: &self.core.input,
                    env: self.env,
                    primitives: &self.core.expansion.primitive_names,
                    font_name: &name,
                }
                .display_token(token, '#')
            }
            fn warning(&mut self, text: &str) {
                self.core.diagnostics.warning(text, self.core.interaction)
            }
        }
    };
}
common_language!(EvalView);
common_language!(EngineView);
impl EngineView<'_> {
    pub(super) fn run_expansion(&mut self, r: ExpansionRequest) -> Result<ExpansionValue> {
        self.eval().run_expansion(r)
    }
    pub(super) fn run_scan(&mut self, r: ScanRequest) -> Result<ScanValue> {
        self.eval().run_scan(r)
    }
}
impl EvalView<'_> {
    pub(super) fn run_expansion(&mut self, r: ExpansionRequest) -> Result<ExpansionValue> {
        if let Value::Expansion(v) = self.evaluate(Request::Expand(r))? {
            Ok(v)
        } else {
            Err(Error::runtime("Unexpected expansion driver result"))
        }
    }
    pub(super) fn run_scan(&mut self, r: ScanRequest) -> Result<ScanValue> {
        if let Value::Scan(v) = self.evaluate(Request::Scan(r))? {
            Ok(v)
        } else {
            Err(Error::runtime("Unexpected scanner driver result"))
        }
    }
    pub(super) fn run_assignment(&mut self, r: AssignmentRequest) -> Result<AssignmentValue> {
        if let Value::Assignment(v) = self.evaluate(Request::Assignment(r))? {
            Ok(v)
        } else {
            Err(Error::runtime("Unexpected assignment driver result"))
        }
    }
    pub(super) fn meaning(&self) -> Result<String> {
        let name = |n| FontCommands::name(self.fonts, n);
        let printer = TokenPrinter {
            input: &self.core.input,
            env: self.env,
            primitives: &self.core.expansion.primitive_names,
            font_name: &name,
        };
        printer.meaning(
            self.core.input.current.command,
            &self.core.input.current.value,
        )
    }
    pub(super) fn with_streams<T>(
        &mut self,
        f: impl FnOnce(&mut Streams, &mut Self) -> Result<T>,
    ) -> Result<T> {
        self.core.stream_eof = self
            .core
            .streams
            .readers
            .keys()
            .map(|&n| (n, self.core.streams.at_eof(n)))
            .collect();
        let mut streams = std::mem::take(&mut self.core.streams);
        let result = f(&mut streams, self);
        self.core.stream_eof.clear();
        self.core.streams = streams;
        result
    }
    pub(super) fn with_document<T>(
        &mut self,
        f: impl FnOnce(&mut PDFDocument, &mut PDFResources, &mut DocServices<'_, '_>) -> Result<T>,
    ) -> Result<T> {
        let mut document = std::mem::take(&mut self.core.backend.document);
        let mut resources = std::mem::take(&mut self.core.backend.resources);
        let dummy_font = document.space_font;
        let mut services = DocServices {
            view: self,
            dummy_font,
        };
        let result = f(&mut document, &mut resources, &mut services);
        self.core.backend.document = document;
        self.core.backend.resources = resources;
        result
    }
    pub(super) fn dummy_space_font(&mut self) -> Result<usize> {
        let number = if let Some(number) = self.core.backend.document.space_font {
            number
        } else {
            let directory = crate::resources::host::current_dir()?;
            let name = self.core.backend.document.space_font_name.clone();
            let number = self.fonts.load(&name, &directory, None, 1000)?;
            let font_directory = self.fonts.directory.clone();
            let mut warnings = Vec::new();
            self.fonts.map_command(
                "=pdftexspace PdfTeX-Space <pdftexspace.pfb",
                true,
                &font_directory,
                false,
                &mut |s| warnings.push(s.to_string()),
            )?;
            for warning in warnings {
                self.warning(&warning)
            }
            self.core.backend.document.space_font = Some(number);
            number
        };
        FontCommandServices::use_font(self, number)?;
        self.fonts.items[number]
            .materialization
            .included_codes
            .insert(32);
        Ok(number)
    }
    pub(super) fn freeze_font_options(&mut self) -> Result<()> {
        let text = if let Some(Quantity::Tokens(tokens)) =
            self.env.values.get(&ValueKey::parameter("pdfpkmode"))
        {
            let tokens = tokens.clone();
            crate::language::streams::display_tokens(self, &tokens)?
        } else {
            String::new()
        };
        self.fonts
            .freeze_pdf_options(&mut self.env.values, &|_| text.clone())
    }
}
impl InternalServices for EvalView<'_> {
    fn resolve_primitive(&mut self) -> Result<()> {
        self.core.input.resolve_primitive(self.env)
    }
    fn read_penalty(&mut self, code: usize) -> Result<i64> {
        let index = self.scan_int()?;
        Ok(Penalties::at(self.env, code as i64, index))
    }

    fn read_extended(&mut self, code: usize) -> Result<InternalValue> {
        let e = ExtendedState {
            timer_start: self.core.extended.timer_start,
        };
        e.read(self, code)
    }
    fn read_expression(&mut self, code: usize) -> Result<InternalValue> {
        let kind = *crate::language::assignments::KINDS
            .get(code)
            .ok_or_else(|| Error::value("Invalid expression type"))?;
        let mut input = ExpressionReader {
            view: self,
            error: None,
        };
        let result = expressions::Expressions::scan(&mut input, kind, false);
        if let Some(error) = input.error {
            return Err(error);
        }
        Assignments::internal(kind, result?)
    }
    fn read_last_item(&mut self, code: i64) -> Result<(QuantityKind, Quantity)> {
        if self.env.reuse_stateful.get() && code != 3 {
            self.grouped_tail_read(code);
        }
        let mut warning = |_: &str| {};
        let context = TypesettingContext {
            builder: self.builder,
            fonts: self.fonts,
            env: self.env,
            warn: &mut warning,
        };
        ClassicLists::read(
            &context,
            &self.core.page_observation,
            &self.core.math_observation,
            code,
            self.core.input.cursor.line,
            if code == 4 {
                self.layout.last_badness()
            } else {
                0
            },
        )
    }
    fn read_pdf_item(&self, name: &str) -> Result<i64> {
        self.core
            .local
            .evidence
            .borrow_mut()
            .paint_resource_observation();
        crate::language::pdf_extensions::item(&self.core.backend.document, name)
    }
    fn page_dimension(&self, code: usize) -> Result<i64> {
        self.core.page_observation.dimension(code)
    }
    fn page_dead_cycles(&self) -> i64 {
        self.core.page_observation.dead_cycles
    }
    fn page_insert_penalties(&self) -> i64 {
        self.core.page_observation.insert_penalties
    }
    fn interaction(&self) -> u8 {
        self.core.interaction
    }
    fn vertical_prev_graf(&self) -> i64 {
        if self.builder.context.mode == crate::core::enums::ListMode::Vertical {
            return self.builder.context.prev_graf;
        }
        if let Some((index, context)) = self
            .builder
            .context_stack
            .iter()
            .enumerate()
            .rev()
            .find(|(_, c)| c.mode == crate::core::enums::ListMode::Vertical)
        {
            if self.env.reuse_stateful.get() && index < self.env.reuse_base_context_depth.get() {
                self.env
                    .reuse_outer_prev
                    .borrow_mut()
                    .entry(index)
                    .or_insert(context.prev_graf);
            }
            context.prev_graf
        } else {
            0
        }
    }
    fn read_assignment(&mut self) -> Result<Option<(QuantityKind, Quantity)>> {
        if let AssignmentValue::Quantity(value) = self.run_assignment(AssignmentRequest::Read)? {
            Ok(value)
        } else {
            Err(Error::runtime("Unexpected assignment read result"))
        }
    }
}
struct ExpressionReader<'a, 'b> {
    view: &'a mut EvalView<'b>,
    error: Option<Error>,
}
impl ExpressionInput for ExpressionReader<'_, '_> {
    fn literal_category(&self, c: Cat) -> bool {
        self.view.core.input.current.literal_category(c)
    }
    fn cur_chr(&self) -> i64 {
        self.view.core.input.current.character().unwrap_or(-1)
    }
    fn cur_command(&self) -> Option<Command> {
        self.view.core.input.current.operation()
    }
    fn nonblank(&mut self) -> Result<()> {
        self.view.nonblank()
    }
    fn back_input(&mut self) {
        if let Err(error) = self.view.core.input.back_input() {
            self.error = Some(error)
        }
    }
    fn scan_int(&mut self) -> Result<i64> {
        self.view.scan_int()
    }
    fn scan_dimen(&mut self) -> Result<i64> {
        self.view.scan_dimen()
    }
    fn scan_glue(&mut self, mu: bool) -> Result<GlueSpec> {
        self.view.scan_glue(mu)
    }
}
impl ExtendedServices for EvalView<'_> {
    fn conditional_depth(&self) -> usize {
        self.core.conditionals.stack.len()
    }
    fn conditional_frame(&self) -> Option<crate::language::conditionals::ConditionalFrame> {
        self.core
            .conditionals
            .stack
            .last()
            .map(|c| c.borrow().clone())
    }
    fn random_seed(&self) -> Result<i64> {
        self.core
            .random_numbers
            .seed
            .to_i64()
            .ok_or_else(|| Error::value("Random seed exceeds integer range"))
    }
}
impl FontCommandServices for EvalView<'_> {
    fn use_font(&mut self, n: usize) -> Result<usize> {
        let mode = self.core.interaction;
        let mut warning = |text: &str| self.core.diagnostics.warning(text, mode);
        self.core
            .backend
            .use_font(n, self.fonts, self.env, &mut warning)
    }
}
impl FontCommandServices for EngineView<'_> {
    fn use_font(&mut self, n: usize) -> Result<usize> {
        self.eval().use_font(n)
    }
}
impl PdfUtilityServices for EvalView<'_> {
    fn exact_repro(&self) -> bool {
        self.core.config.exact_repro
    }
    fn define_unicode(&mut self, glyph: &str, value: &str) {
        let mode = self.core.interaction;
        self.core
            .backend
            .unicode_maps
            .define(glyph, value, &mut |text| {
                self.core.diagnostics.warning(text, mode)
            });
    }
    fn image(&self, n: usize) -> Option<&crate::pdf::image_records::ImageUse> {
        self.core
            .local
            .evidence
            .borrow_mut()
            .paint_resource_observation();
        self.core.backend.document.images.get(&n)
    }
    fn form_ordinal(&self, n: usize) -> Option<usize> {
        self.core
            .local
            .evidence
            .borrow_mut()
            .paint_resource_observation();
        self.core.backend.document.forms.get(&n).map(|f| f.ordinal)
    }
    fn add_color_stack(&mut self, stack: ColorStack) -> usize {
        self.core
            .local
            .evidence
            .borrow_mut()
            .paint_resource_observation();
        let number = self.core.backend.document.colors.len();
        self.core.backend.document.colors.push(stack);
        number
    }
    fn page_reference(&mut self, page: i64) -> Result<usize> {
        self.core
            .local
            .evidence
            .borrow_mut()
            .paint_resource_observation();
        self.core.backend.resources.page_reference(page)
    }
    fn font_reference(&mut self, font: usize) -> Result<(usize, usize)> {
        self.core
            .local
            .evidence
            .borrow_mut()
            .paint_resource_observation();
        let index = FontCommandServices::use_font(self, font)?;
        let g = &self.core.backend.resources.font_groups[index];
        Ok((g.font, g.object_number))
    }
    fn uniform_deviate(&mut self, extent: i64) -> String {
        self.core.random_numbers.uniform(&extent.into()).to_string()
    }
    fn normal_deviate(&mut self) -> Result<i64> {
        self.core.random_numbers.normal()
    }
    fn insertion_size(&self, n: i64) -> Option<i64> {
        self.core
            .page_observation
            .insertions
            .get(&n)
            .map(|i| i.size)
    }
}
impl StreamServices for EvalView<'_> {
    fn interaction(&self) -> u8 {
        self.core.interaction
    }
    fn eight_bit_output(&self) -> bool {
        self.core.config.eight_bit_output
    }
    fn observe_terminal_write(&mut self, text: &str) -> Result<()> {
        if self.core.input.recovery.enabled() {
            self.core.diagnostics.collect_terminal(text);
            let trimmed = text.trim_start();
            let source = self.core.input.source();
            let mut context = source.clone();
            let mut semantic = false;
            for _ in 0..16 {
                let Some(at) = context else { break };
                if at.expansion.as_deref().is_some_and(|name| {
                    [
                        "macro:GenericWarning",
                        "macro:PackageWarning",
                        "macro:ClassWarning",
                        "macro:@latex@warning",
                        "macro:@latex@warning@no@line",
                    ]
                    .contains(&name)
                }) {
                    semantic = true;
                    break;
                }
                context = at.call_site.clone();
            }
            if semantic
                || trimmed.starts_with("LaTeX Warning:")
                || ((trimmed.starts_with("Package ") || trimmed.starts_with("Class "))
                    && trimmed.contains(" Warning:"))
            {
                self.core.diagnostics.collect_warning(text, source, true);
                if !semantic {
                    if let Some(record) = self.core.diagnostics.records.last_mut() {
                        if let Some(details) = record.details.as_mut() {
                            Arc::make_mut(details).explanation="Recognized from terminal output. The location is the reporting context, which may differ from the cause.".into();
                        }
                    }
                }
            }
        }
        // LaTeX probes optional files before reporting a genuinely required
        // missing input. At that report, produce the same typed host request as
        // a failed physical input, before its obsolete terminal prompt runs.
        let mut context = self.core.input.source();
        let mut missing_file_report = false;
        for _ in 0..64 {
            let Some(at) = context else { break };
            if at.expansion.as_deref() == Some("macro:@missingfileerror") {
                missing_file_report = true;
                break;
            }
            context = at.call_site.clone();
        }
        if missing_file_report {
            if let Some((_, rest)) = text.split_once("LaTeX Error: File `") {
                if let Some((name, _)) = rest.split_once("' not found") {
                    if !name.is_empty()
                        && name.len() <= 200
                        && name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_+.-".contains(&b))
                    {
                        return Err(
                            Error::value(format!("Required TeX file `{name}' not found"))
                                .with_missing_resource(crate::core::errors::MissingResource {
                                    kind: "tex".into(),
                                    name: name.as_bytes().to_vec(),
                                    directory: None,
                                }),
                        );
                    }
                }
            }
        }
        Ok(())
    }
    fn terminal_write(&mut self, text: &str) -> Result<()> {
        crate::language::conversions::print_tex(
            &mut crate::resources::host::ConsoleWriter { error: false },
            text,
            "",
            self.core.config.language_profile == LanguageProfile::Unicode,
        )
    }
}
pub(super) struct DocServices<'a, 'b> {
    pub view: &'a mut EvalView<'b>,
    pub dummy_font: Option<usize>,
}
impl DocumentServices for DocServices<'_, '_> {
    fn parameter(&self, name: &str) -> i64 {
        InputProcessor::parameter(self.view.env, name, 0)
    }
    fn token_parameter(&mut self, name: &str) -> Result<String> {
        let tokens = match self.view.env.values.get(&ValueKey::parameter(name)) {
            Some(Quantity::Tokens(tokens)) => tokens.clone(),
            _ => Arc::from([]),
        };
        crate::language::streams::display_tokens(self.view, &tokens)
    }
    fn set_parameter(&mut self, name: &str, n: i64) {
        self.view
            .env
            .values
            .insert(ValueKey::parameter(name), Quantity::Integer(n));
    }
    fn exact_repro(&self) -> bool {
        self.view.core.config.exact_repro
    }
    fn decimal_assigned(&self) -> bool {
        matches!(self.view.env.values.get(&ValueKey{table:"pdf_policy".into(),index:ValueIndex::Name("decimal_assigned".into())}),Some(Quantity::Integer(n))if *n!=0)
    }
    fn warning(&mut self, text: &str) {
        ListServices::warning(self.view, text)
    }
    fn resolve_file(&mut self, name: &str, directory: &str) -> Result<Vec<u8>> {
        let path = self
            .view
            .core
            .input
            .resources
            .resolve_literal(
                name,
                self.view.core.config.language_profile == LanguageProfile::Unicode,
                Some(Path::new(directory)),
                ".tex",
                true,
                true,
            )?
            .ok_or_else(|| Error::value(format!("Cannot open file for PDF embedding: {name}")))?;
        Ok(crate::resources::host::read(path)?)
    }
    fn job_name(&self) -> String {
        self.view.core.input.cursor.name.clone().unwrap_or_default()
    }
    fn dummy_space_font(&mut self) -> Result<usize> {
        self.dummy_font
            .ok_or_else(|| Error::runtime("Missing prepared extraction-space font"))
    }
}
impl ImageServices for DocServices<'_, '_> {
    fn replace_inclusion_font(
        &mut self,
        g: &mut PDFImportGraph<'_>,
        value: &PdfValue,
        key: &SourceKey,
        pdf: &lopdf::Document,
        copy_fonts: bool,
    ) -> Result<Option<Vec<u8>>> {
        let suppress = InputProcessor::parameter(self.view.env, "pdfsuppresswarningdupmap", 0) > 0;
        let mode = self.view.core.interaction;
        crate::pdf::inclusion_fonts::replace_font(
            self.view.fonts,
            suppress,
            &mut |text| self.view.core.diagnostics.warning(text, mode),
            g,
            value,
            key,
            pdf,
            copy_fonts,
        )
    }
}
impl crate::pdf::extraction::ExtractionServices for EvalView<'_> {
    fn fonts(&self) -> &FontCatalog {
        self.fonts
    }
    fn fonts_mut(&mut self) -> &mut FontCatalog {
        self.fonts
    }
    fn precision(&mut self) -> Result<u32> {
        self.with_document(|d, _, s| Ok(d.precision(s) as u32))
    }
    fn dummy_space_font(&mut self) -> Result<usize> {
        EvalView::dummy_space_font(self)
    }
}
impl crate::diagnostics::classic::ClassicServices for EngineView<'_> {
    fn font_identifier(&mut self, n: usize) -> Result<String> {
        Ok(FontCommands::identifier(self, n))
    }
    fn show_meaning(&mut self) -> Result<(String, String)> {
        let e = self.eval();
        let name = |n| FontCommands::name(e.fonts, n);
        let p = TokenPrinter {
            input: &e.core.input,
            env: e.env,
            primitives: &e.core.expansion.primitive_names,
            font_name: &name,
        };
        Ok((
            p.string_token(e.core.input.current.token)?,
            p.meaning(e.core.input.current.command, &e.core.input.current.value)?,
        ))
    }
    fn show_the(&mut self) -> Result<String> {
        let result = InternalValues::the_text(&mut self.eval())?;
        match result {
            ExpansionValue::Text(text) => Ok(text),
            ExpansionValue::Tokens(tokens) => {
                crate::language::streams::display_tokens(self, &tokens)
            }
            _ => Err(Error::value("Invalid showthe result")),
        }
    }
    fn box_register(&self, n: i64) -> i64 {
        self.env.box_registers.get(&n).copied().unwrap_or(C::NULL)
    }
    fn page_nodes(&self) -> Vec<i64> {
        self.pages.page.clone()
    }
    fn show_event(&mut self, code: i64, lines: &[String]) {
        self.event(
            "show",
            json!({"command":code,"lines":lines,"source":self.core.input.source()}),
        );
    }
}
