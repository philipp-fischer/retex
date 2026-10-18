//! Reentrant page output, retained effects and reusable form capture.
use super::*;
use crate::typesetting::model::NodeFields;
use crate::{
    language::{
        font_commands::FontCommandServices,
        pdf_extensions::ExtensionServices,
        pdf_navigation::NavigationCommandServices,
        services::{LanguageServices, ListServices},
        specials::Specials,
    },
    pdf::{
        backend::PdfWriteOptions,
        document::PDFDocument,
        emission::PDFEmissionState,
        forms::{Form, FormServices},
        images::ImageOptions,
        navigation::{
            Action, AnnotationTemplate, DestinationEffect, Dimensions, NavigationContext,
            ThreadTemplate,
        },
        resources::PageUse,
    },
    typesetting::{
        box_commands::{BoxCommands, BoxServices},
        display::Placement,
        layout::TypesettingContext,
        model::{IoEffect, LiteralMode, Node, NodePayload, PdfEffect, SpecialEffect},
        pages::PageServices,
        shipout::ShipoutServices,
        vertical::Vertical,
    },
};
pub(super) struct PageController<'a> {
    pub core: &'a mut EngineCore,
    pub hyphenation: &'a mut Hyphenation,
    pub math: &'a mut MathState,
    pub alignments: &'a mut Alignments,
}
struct Shipment<'a> {
    core: &'a mut EngineCore,
}
impl Shipment<'_> {
    fn eval<'a>(
        &'a mut self,
        l: &'a mut Layout,
        c: &'a mut TypesettingContext<'_>,
    ) -> EvalView<'a> {
        EvalView {
            core: self.core,
            env: c.env,
            fonts: c.fonts,
            builder: c.builder,
            layout: l,
        }
    }
}
impl PageServices for PageController<'_> {
    fn recover(
        &mut self,
        issue: crate::core::recovery::Issue,
        error: Error,
        action: &str,
    ) -> Result<()> {
        self.core.input.recover(issue, error, action)
    }
    fn shipout(&mut self) -> &mut dyn ShipoutServices {
        self
    }
    fn read_output_box(
        &mut self,
        pages: &mut PageBuilder,
        vertical: &mut Vertical,
        layout: &mut Layout,
        c: &mut TypesettingContext<'_>,
        tokens: Arc<[Token]>,
    ) -> Result<i64> {
        self.core.input.begin_tokens(tokens, None, None)?;
        let mut engine = EngineView {
            core: self.core,
            env: c.env,
            fonts: c.fonts,
            builder: c.builder,
            layout,
            pages,
            vertical,
            hyphenation: self.hyphenation,
            math: self.math,
            alignments: self.alignments,
        };
        BoxCommands::read_box(
            &mut engine,
            crate::core::enums::BoxOperation::Vbox,
            None,
            None,
            false,
            false,
            Some(crate::core::enums::GroupKind::Output),
        )
    }
    fn handle_page(
        &mut self,
        _: &mut PageBuilder,
        _: &mut Vertical,
        l: &mut Layout,
        c: &mut TypesettingContext<'_>,
        p: i64,
    ) -> Result<()> {
        l.ship_box(c, p, self)
    }
}
macro_rules! forward_shipout {
    () => {
        fn begin_page(
            &mut self,
            l: &mut Layout,
            c: &mut TypesettingContext<'_>,
            h: i64,
            x: i64,
            y: i64,
        ) -> Result<usize> {
            Shipment { core: self.core }.begin_page(l, c, h, x, y)
        }
        fn finish_page(
            &mut self,
            l: &mut Layout,
            c: &mut TypesettingContext<'_>,
            p: usize,
        ) -> Result<()> {
            Shipment { core: self.core }.finish_page(l, c, p)
        }
        fn use_font(
            &mut self,
            l: &mut Layout,
            c: &mut TypesettingContext<'_>,
            n: usize,
        ) -> Result<usize> {
            Shipment { core: self.core }.use_font(l, c, n)
        }
        fn perform_io(
            &mut self,
            l: &mut Layout,
            c: &mut TypesettingContext<'_>,
            e: &IoEffect,
        ) -> Result<()> {
            Shipment { core: self.core }.perform_io(l, c, e)
        }
        fn ship_special(
            &mut self,
            l: &mut Layout,
            c: &mut TypesettingContext<'_>,
            e: &SpecialEffect,
        ) -> Result<Option<(String, String)>> {
            Shipment { core: self.core }.ship_special(l, c, e)
        }
        fn ship_special_text(
            &mut self,
            l: &mut Layout,
            c: &mut TypesettingContext<'_>,
            t: &str,
        ) -> Result<Option<(String, String)>> {
            Shipment { core: self.core }.ship_special_text(l, c, t)
        }
        fn ship_extension(
            &mut self,
            l: &mut Layout,
            c: &mut TypesettingContext<'_>,
            e: &PdfEffect,
            x: i64,
            y: i64,
            b: bool,
            n: &Node,
        ) -> Result<Option<(String, String)>> {
            Shipment { core: self.core }.ship_extension(l, c, e, x, y, b, n)
        }
        fn enter_box(
            &mut self,
            l: &mut Layout,
            c: &mut TypesettingContext<'_>,
            n: &Node,
            x: i64,
            y: i64,
            b: bool,
        ) -> Result<()> {
            Shipment { core: self.core }.enter_box(l, c, n, x, y, b)
        }
        fn leave_box(&mut self, l: &mut Layout, c: &mut TypesettingContext<'_>) -> Result<()> {
            Shipment { core: self.core }.leave_box(l, c)
        }
        fn capture_paint(
            &mut self,
            l: &mut Layout,
            c: &mut TypesettingContext<'_>,
            p: Placement,
        ) -> Result<Vec<Placement>> {
            Shipment { core: self.core }.capture_paint(l, c, p)
        }
    };
}
impl ShipoutServices for PageController<'_> {
    forward_shipout!();
}
impl ShipoutServices for Shipment<'_> {
    fn begin_page(
        &mut self,
        l: &mut Layout,
        c: &mut TypesettingContext<'_>,
        height: i64,
        x: i64,
        y: i64,
    ) -> Result<usize> {
        let mut e = self.eval(l, c);
        e.freeze_font_options()?;
        e.with_document(|d, r, s| {
            d.begin_page(s, r, height, x, y)?;
            Ok(r.pages.len() - 1)
        })
    }
    fn finish_page(
        &mut self,
        l: &mut Layout,
        c: &mut TypesettingContext<'_>,
        _: usize,
    ) -> Result<()> {
        let mut e = self.eval(l, c);
        let page = e
            .core
            .backend
            .document
            .current_page
            .clone()
            .ok_or_else(|| Error::runtime("Missing shipment page"))?;
        e.flush_resources(&page)?;
        e.with_document(|d, r, s| d.finish_page(s, r))
    }
    fn use_font(
        &mut self,
        l: &mut Layout,
        c: &mut TypesettingContext<'_>,
        n: usize,
    ) -> Result<usize> {
        self.eval(l, c).use_font(n)
    }
    fn perform_io(
        &mut self,
        l: &mut Layout,
        c: &mut TypesettingContext<'_>,
        effect: &IoEffect,
    ) -> Result<()> {
        self.eval(l, c).with_streams(|st, s| st.perform(s, effect))
    }
    fn ship_special(
        &mut self,
        l: &mut Layout,
        c: &mut TypesettingContext<'_>,
        effect: &SpecialEffect,
    ) -> Result<Option<(String, String)>> {
        Ok(Specials::ship(&mut self.eval(l, c), effect)?.map(|(m, t)| (m.as_str().into(), t)))
    }
    fn ship_special_text(
        &mut self,
        l: &mut Layout,
        c: &mut TypesettingContext<'_>,
        text: &str,
    ) -> Result<Option<(String, String)>> {
        Ok(
            Specials::ship_text(&mut self.eval(l, c), text, LiteralMode::Special)
                .map(|(m, t)| (m.as_str().into(), t)),
        )
    }
    fn ship_extension(
        &mut self,
        l: &mut Layout,
        c: &mut TypesettingContext<'_>,
        effect: &PdfEffect,
        x: i64,
        y: i64,
        leaders: bool,
        node: &Node,
    ) -> Result<Option<(String, String)>> {
        let mag = l.magnification.unwrap_or(1000);
        let mut e = self.eval(l, c);
        if matches!(effect, PdfEffect::FakeSpace) {
            e.dummy_space_font()?;
        }
        let source = e.builder.nodes.effect_source(node);
        e.with_document(|d, r, s| d.ship(s, r, effect, x, y, leaders, source, mag))
    }
    fn enter_box(
        &mut self,
        l: &mut Layout,
        c: &mut TypesettingContext<'_>,
        node: &Node,
        x: i64,
        y: i64,
        leaders: bool,
    ) -> Result<()> {
        let mag = l.magnification.unwrap_or(1000);
        self.eval(l, c).with_document(|d, r, s| {
            let precision = d.precision(s);
            let exact = s.view.core.config.exact_repro;
            let margins = [
                InputProcessor::parameter(s.view.env, "pdflinkmargin", 0),
                InputProcessor::parameter(s.view.env, "pdfdestmargin", 0),
                InputProcessor::parameter(s.view.env, "pdfthreadmargin", 0),
            ];
            let suppress =
                InputProcessor::parameter(s.view.env, "pdfsuppresswarningdupdest", 0) > 0;
            let mut warn = |t: &str| s.view.warning(t);
            let mut ctx = NavigationContext {
                resources: r,
                page: d
                    .current_page
                    .as_mut()
                    .ok_or_else(|| Error::runtime("Missing shipment page"))?,
                page_height: d.page_height,
                shipping_page: d.shipping_page,
                exact_repro: exact,
                link_margin: margins[0],
                dest_margin: margins[1],
                thread_margin: margins[2],
                suppress_duplicate: suppress,
                precision,
                magnification: mag,
                warning: &mut warn,
            };
            d.navigation.enter_box(&mut ctx, node, x, y, leaders)
        })
    }
    fn leave_box(&mut self, _: &mut Layout, _: &mut TypesettingContext<'_>) -> Result<()> {
        self.core.backend.document.navigation.leave_box();
        Ok(())
    }
    fn capture_paint(
        &mut self,
        l: &mut Layout,
        c: &mut TypesettingContext<'_>,
        p: Placement,
    ) -> Result<Vec<Placement>> {
        let mut extraction = std::mem::take(&mut self.core.backend.document.extraction);
        let parameter = p.parameter;
        if !self.core.backend.document.shipping_page {
            if let (Some(o), Some(parameter)) = (&c.builder.nodes.observer, parameter) {
                o.borrow_mut()
                    .misses
                    .insert(parameter.candidate, "non_page_paint_sink");
            }
        }
        let observer = c.builder.nodes.observer.clone();
        if parameter.is_none()
            && !matches!(
                p.kind.as_str(),
                "hbox" | "vbox" | "glue" | "vglue" | "kern" | "vkern" | "penalty"
            )
        {
            if let Some(o) = &observer {
                o.borrow_mut().paint_epoch += 1;
            }
        }
        if let (Some(o), Some(parameter)) = (&observer, parameter.filter(|p| p.root)) {
            let mut evidence = o.borrow_mut();
            if evidence.paint.len() < 256 {
                let epoch = evidence.paint_epoch;
                evidence.paint.push(crate::checked_material::PaintSite {
                    parameter,
                    page: self.core.backend.resources.pages.len(),
                    placement: p.placement_id,
                    entry_epoch: epoch,
                    exit_epoch: epoch,
                    entry: extraction.clone(),
                    exit: extraction.clone(),
                });
            } else {
                evidence
                    .misses
                    .insert(parameter.candidate, "paint_site_bound");
            }
        }
        let result = extraction.capture(&mut self.eval(l, c), p);
        if let (Some(o), Some(parameter)) = (&observer, parameter) {
            let mut evidence = o.borrow_mut();
            let epoch = evidence.paint_epoch;
            if let Some(site) = evidence.paint.last_mut().filter(|s| {
                s.parameter.candidate == parameter.candidate && s.parameter.line == parameter.line
            }) {
                site.exit = extraction.clone();
                site.exit_epoch = epoch;
            }
        }
        self.core.backend.document.extraction = extraction;
        result
    }
}
impl EvalView<'_> {
    pub(super) fn emit_form(&mut self, n: usize) -> Result<()> {
        let mut form = self
            .core
            .backend
            .document
            .forms
            .get(&n)
            .cloned()
            .ok_or_else(|| {
                Error::value("Cannot find referenced PDF form")
                    .with_issue(crate::core::recovery::Issue::PdfOperation)
            })?;
        if form.emitting {
            return Err(Error::value("Cycle in PDF form references")
                .with_issue(crate::core::recovery::Issue::PdfOperation));
        }
        if form.data.is_some() {
            return Ok(());
        }
        self.core
            .backend
            .document
            .forms
            .get_mut(&n)
            .unwrap()
            .emitting = true;
        let result = crate::pdf::forms::emit(&mut FormAdapter { view: self }, &mut form);
        self.core.backend.document.forms.insert(n, form);
        result
    }
    pub(super) fn flush_resources(&mut self, page: &PageUse) -> Result<()> {
        for &n in &page.objects {
            self.with_document(|d, r, s| d.emit(s, r, n))?
        }
        for &n in page.xobjects.values() {
            if self.core.backend.document.forms.contains_key(&n) {
                self.emit_form(n)?
            } else {
                self.with_document(|d, r, s| crate::pdf::images::emit(d, r, s, n))?
            }
        }
        Ok(())
    }
    pub(super) fn pdf_write_options(&mut self) -> Result<PdfWriteOptions> {
        let (version, precision, compression, information) = self.with_document(|d, _, s| {
            Ok((
                d.check_version(s)?,
                d.precision(s),
                PDFDocument::compression(s),
                d.information(s, s.view.core.pdf_utilities.creation_time)?,
            ))
        })?;
        let tokens = match self.env.values.get(&ValueKey::parameter("pdfpagesattr")) {
            Some(Quantity::Tokens(t)) => t.clone(),
            _ => Arc::from([]),
        };
        Ok(PdfWriteOptions {
            version,
            precision,
            compression,
            information,
            omit_charset: self.parameter("pdfomitcharset", 0) > 0,
            unicode_enabled: self.parameter("pdfgentounicode", 0) > 0,
            omit_procset: self.parameter("pdfomitprocset", 0),
            omit_info: self.parameter("pdfomitinfodict", 0) > 0,
            pages_attributes: crate::language::streams::display_tokens(self, &tokens)?,
        })
    }
}
impl EngineView<'_> {
    pub(super) fn pdf_write_options(&mut self) -> Result<PdfWriteOptions> {
        self.eval().pdf_write_options()
    }
}
pub(super) struct FormCaptureScope {
    roots: RootHold,
    page_height: i64,
    current_page: Option<PageUse>,
    saved_positions: Vec<(i64, i64)>,
    shipping_page: bool,
    matrix: crate::pdf::graphics::Affine,
    saved_matrices: Vec<crate::pdf::graphics::Affine>,
    boxes: Vec<crate::pdf::navigation::BoxContext>,
}
impl FormCaptureScope {
    pub(super) fn restore(self, e: &mut EvalView<'_>) {
        let d = &mut e.core.backend.document;
        d.page_height = self.page_height;
        d.current_page = self.current_page;
        d.saved_positions = self.saved_positions;
        d.shipping_page = self.shipping_page;
        d.navigation.matrix = self.matrix;
        d.navigation.saved_matrices = self.saved_matrices;
        d.navigation.boxes = self.boxes;
        drop(self.roots);
    }
}
impl EvalView<'_> {
    pub(super) fn form_emission_step(
        &mut self,
        execution: &mut crate::pdf::forms::FormEmission,
        form: &mut Form,
        returned: Option<Result<()>>,
    ) -> Result<crate::pdf::forms::FormStep> {
        execution.resume(&mut FormAdapter { view: self }, form, returned)
    }
    pub(super) fn begin_form_capture(&mut self, f: &Form) -> FormCaptureScope {
        let roots = self.builder.ownership.hold([f.box_ptr]);
        let d = &mut self.core.backend.document;
        let scope = FormCaptureScope {
            roots,
            page_height: d.page_height,
            current_page: d.current_page.take(),
            saved_positions: std::mem::take(&mut d.saved_positions),
            shipping_page: d.shipping_page,
            matrix: d.navigation.matrix,
            saved_matrices: std::mem::take(&mut d.navigation.saved_matrices),
            boxes: std::mem::take(&mut d.navigation.boxes),
        };
        d.page_height = f.height + f.depth;
        d.current_page = Some(PageUse {
            resources: f.resources,
            ..Default::default()
        });
        d.shipping_page = false;
        d.navigation.matrix = Default::default();
        scope
    }
    pub(super) fn render_form_capture(
        &mut self,
        f: &mut Form,
        line: crate::typesetting::display::LineDisplayList,
    ) -> Result<PageUse> {
        if !self.core.backend.document.saved_positions.is_empty() {
            return Err(Error::value("Unmatched PDF save at end of form shipment")
                .with_issue(crate::core::recovery::Issue::PdfOperation));
        }
        let mut warn = |_: &str| {};
        let mut state = PDFEmissionState::default();
        let (operators, _) = self.core.backend.render_line(
            &line,
            f.height + f.depth,
            &mut state,
            true,
            self.fonts,
            self.env,
            &mut warn,
        )?;
        f.data = Some(crate::pdf::writer::latin1(&operators.join("\n"))?.into());
        for p in line.placements {
            if p.kind == "glyph" || p.kind == "extraction_space" {
                f.codes.entry(p.font).or_default().insert(p.code);
            }
        }
        let page = self.core.backend.document.current_page.clone().unwrap();
        self.core
            .backend
            .document
            .reserve_object_stream(&mut self.core.backend.resources, 1);
        Ok(page)
    }
    pub(super) fn finish_form_resources(&mut self, f: &mut Form, page: PageUse) {
        f.xobjects = page.xobjects;
        f.objects = page.objects;
        f.group = page.group;
    }
}
struct FormAdapter<'a, 'b> {
    view: &'a mut EvalView<'b>,
}
impl FormServices for FormAdapter<'_, '_> {
    fn check_version(&mut self) -> Result<()> {
        self.view
            .with_document(|d, _, s| d.check_version(s).map(|_| ()))
    }
    fn precision(&mut self) -> usize {
        self.view
            .with_document(|d, _, s| Ok(d.precision(s)))
            .unwrap_or(9)
    }
    fn compression(&self) -> i64 {
        InputProcessor::parameter(self.view.env, "pdfcompresslevel", 0).clamp(0, 9)
    }
    fn reserve(&mut self) -> usize {
        self.view.core.backend.resources.reserve_object()
    }
    fn release_box(&mut self, p: i64) {
        if p != C::NULL {
            let _ = self.view.builder.release(p, false);
        }
    }
    fn capture_form(&mut self, f: &mut Form) -> Result<()> {
        let e = &mut self.view;
        let scope = e.begin_form_capture(f);
        let result = (|| {
            let mut warn = |_: &str| {};
            let mut ctx = TypesettingContext {
                builder: e.builder,
                fonts: e.fonts,
                env: e.env,
                warn: &mut warn,
            };
            let mut shipment = Shipment { core: e.core };
            let line = e.layout.place_line(
                &mut ctx,
                f.box_ptr,
                Some(0),
                Some(f.height),
                Some(&mut shipment),
                false,
                None,
                None,
                None,
            )?;
            let page = e.render_form_capture(f, line)?;
            e.flush_resources(&page)?;
            e.finish_form_resources(f, page);
            Ok(())
        })();
        scope.restore(e);
        result
    }
}
impl NavigationCommandServices for EngineView<'_> {
    fn append_pdf(&mut self, e: PdfEffect, w: i64, h: i64, d: i64) -> Result<()> {
        let p = self.builder.allocate(Node::from(NodeFields {
            node_type: crate::core::enums::NodeKind::Whatsit,
            whatsit: "pdf".into(),
            width: w,
            height: h,
            depth: d,
            payload: NodePayload::Pdf(e),
            origins: vec![self.core.input.source()],
            ..Default::default()
        }))?;
        self.builder.tail_append(p)
    }
    fn reserve_annotation(&mut self) -> usize {
        let n = self.core.backend.resources.reserve_object();
        self.core
            .backend
            .document
            .navigation
            .annotations
            .insert(n, None);
        n
    }
    fn validate_annotation(&mut self, n: usize) -> Result<()> {
        if self
            .core
            .backend
            .document
            .navigation
            .annotations
            .get(&n)
            .is_some_and(Option::is_none)
        {
            Ok(())
        } else {
            Err(Error::value("Invalid PDF annotation reservation"))
        }
    }
    fn set_last_annotation(&mut self, n: usize, link: bool) {
        if link {
            self.core.backend.document.navigation.last_link = n;
        } else {
            self.core.backend.document.navigation.last_annotation = n;
        }
    }
    fn publish_annotation(&mut self, t: AnnotationTemplate) {
        self.core
            .backend
            .document
            .navigation
            .annotation_templates
            .insert(t.number, t);
    }
    fn publish_destination(&mut self, t: DestinationEffect) -> Result<Option<usize>> {
        if self
            .core
            .backend
            .document
            .navigation
            .destinations
            .get(&(t.structured != 0, t.identifier.clone()))
            .is_some_and(|d| d.array.is_some())
        {
            if self.parameter("pdfsuppresswarningdupdest", 0) <= 0 {
                self.warning(&format!(
                    "Duplicate PDF destination ignored: {}",
                    t.identifier
                ));
            }
            return Ok(None);
        }

        let n = self
            .core
            .backend
            .document
            .navigation
            .destination_effects
            .len();
        self.core
            .backend
            .document
            .navigation
            .destination_effects
            .push(t);
        Ok(Some(n))
    }
    fn publish_thread(&mut self, t: ThreadTemplate) -> usize {
        let n = self.core.backend.document.navigation.thread_templates.len();
        self.core
            .backend
            .document
            .navigation
            .thread_templates
            .push(t);
        n
    }
    fn outline(&mut self, a: Action, title: String, count: i64, attributes: String) -> Result<()> {
        self.core.backend.document.navigation.outline(
            &mut self.core.backend.resources,
            a,
            title,
            count,
            attributes,
        )
    }
}
impl ExtensionServices for EngineView<'_> {
    fn document(&self) -> &PDFDocument {
        &self.core.backend.document
    }
    fn document_mut(&mut self) -> &mut PDFDocument {
        &mut self.core.backend.document
    }
    fn reserve_pdf_object(&mut self) -> usize {
        self.core.backend.resources.reserve_object()
    }
    fn reserve_source_object(&mut self, n: Option<usize>) -> Result<usize> {
        self.eval().with_document(|d, r, s| Ok(d.reserve(s, r, n)))
    }
    fn check_pdf_version(&mut self) -> Result<()> {
        self.eval()
            .with_document(|d, _, s| d.check_version(s).map(|_| ()))
    }
    fn emit_source_object(&mut self, n: usize) -> Result<()> {
        self.eval().with_document(|d, r, s| d.emit(s, r, n))
    }
    fn emit_form(&mut self, n: usize) -> Result<()> {
        self.eval().emit_form(n)
    }
    fn load_image(
        &mut self,
        n: usize,
        name: String,
        spec: Dimensions,
        options: ImageOptions,
    ) -> Result<()> {
        let directory = self.source_directory();
        let path = self
            .core
            .input
            .resources
            .resolve_literal(
                &name,
                self.core.config.language_profile == LanguageProfile::Unicode,
                Some(&directory),
                ".tex",
                true,
                true,
            )?
            .ok_or_else(|| {
                let error = Error::value(format!("Cannot find image: {name}"));
                if self.core.input.resources.last_error.is_some() {
                    return error;
                }
                let filename = if self.core.config.language_profile == LanguageProfile::Unicode {
                    std::ffi::OsString::from(&name)
                } else {
                    match crate::resources::search::filesystem_name(&name) {
                        Ok(filename) => filename,
                        Err(error) => return error,
                    }
                };
                error.with_missing_resource(crate::core::errors::MissingResource {
                    kind: "image".into(),
                    name: crate::language::input_frames::filesystem_bytes(&filename),
                    directory: if self.core.input.resources.native {
                        None
                    } else {
                        Some(crate::language::input_frames::filesystem_bytes(
                            directory.as_os_str(),
                        ))
                    },
                })
            })?;
        let source = crate::resources::host::read(&path)?;
        self.eval().with_document(|d, r, s| {
            let image = crate::pdf::images::load(
                d,
                r,
                s,
                n,
                path.to_string_lossy().into_owned(),
                source,
                &spec,
                options,
            )?;
            d.last_image = n;
            d.last_image_pages = image.pages;
            d.last_image_depth = image.color_depth;
            d.images.insert(n, image);
            Ok(())
        })
    }
    fn emit_image(&mut self, n: usize) -> Result<()> {
        self.eval()
            .with_document(|d, r, s| crate::pdf::images::emit(d, r, s, n))
    }
    fn take_box_register(&mut self, n: i64) -> Result<i64> {
        self.take_register_box(n, false)
    }
    fn pdf_action_string(&mut self, a: Action) -> Result<String> {
        self.core
            .backend
            .document
            .navigation
            .action_string(&mut self.core.backend.resources, &a)
    }
}
