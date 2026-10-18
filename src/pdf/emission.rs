//! Ordered display-list painting with page-wide graphics and historical text cursors.
use crate::{
    core::{
        errors::{Error, Result},
        units::{bp_to_sp, sp_to_bp},
    },
    fonts::catalog::FontCatalog,
    language::state::Environment,
    pdf::{
        document::PDFDocument,
        graphics::Affine,
        resources::PDFResources,
        syntax::character_string,
        text::{HistoricalOrigin, HistoricalText},
    },
    typesetting::display::{EmittedGlyph, LineDisplayList, Placement},
};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, Default)]
pub struct PDFEmissionState {
    pub(crate) source_geometry: Vec<crate::source_map::Geometry>,
    pub(crate) mapping_unsafe: bool,
    pub(crate) preview_spans: Vec<crate::paragraph_preview::Span>,
    pub(crate) local_spans: Vec<LocalSpan>,
    pub(crate) local_text: Option<(Option<usize>, Option<i64>, bool)>,
    pub(crate) fragment: bool,
    pub origin: (f64, f64),
    pub matrix: Affine,
    pub saved: Vec<((f64, f64), Affine)>,
    pub historical_text: Option<HistoricalText>,
    pub historical_origin: Option<HistoricalOrigin>,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LocalSpan {
    pub candidate: usize,
    pub line: usize,
    pub placement: usize,
    pub start: usize,
    pub end: usize,
    pub entry: PDFEmissionState,
    pub exit: PDFEmissionState,
}
impl PartialEq for PDFEmissionState {
    fn eq(&self, b: &Self) -> bool {
        self.local_spans == b.local_spans
            && self.local_text == b.local_text
            && self.mapping_unsafe == b.mapping_unsafe
            && self.fragment == b.fragment
            && self.origin == b.origin
            && self.matrix == b.matrix
            && self.saved == b.saved
            && self.historical_text == b.historical_text
            && self.historical_origin == b.historical_origin
    }
}
pub struct EmissionContext<'a> {
    pub fonts: &'a mut FontCatalog,
    pub env: &'a mut Environment,
    pub resources: &'a mut PDFResources,
    pub document: &'a mut PDFDocument,
    pub precision: usize,
    pub exact_repro: bool,
    pub resource_suffix: String,
    pub warn: &'a mut dyn FnMut(&str),
}
pub struct LineEmitter<'a, 'b> {
    context: &'a mut EmissionContext<'b>,
    page_height: i64,
    state: &'a mut PDFEmissionState,
    operators: Vec<String>,
    observed: Vec<EmittedGlyph>,
    historical_text: Option<HistoricalText>,
    current_font: Option<usize>,
    current_baseline: Option<i64>,
    text_open: bool,
    spans: BTreeMap<usize, String>,
    span_ends: BTreeSet<usize>,
}
impl<'a, 'b> LineEmitter<'a, 'b> {
    pub fn new(
        context: &'a mut EmissionContext<'b>,
        page_height: i64,
        state: &'a mut PDFEmissionState,
    ) -> Self {
        if context.exact_repro && state.historical_origin.is_none() {
            state.historical_origin = Some(HistoricalOrigin {
                h: 0,
                v: page_height,
                decimals: context.precision as u32,
            })
        }
        let text = state.historical_text.take();
        let (current_font, current_baseline, text_open) = state.local_text.take().unwrap_or((
            text.as_ref().and_then(|t| t.font),
            text.as_ref().map(|t| t.baseline),
            text.is_some(),
        ));
        Self {
            context,
            page_height,
            current_font,
            current_baseline,
            text_open,
            historical_text: text,
            state,
            operators: Vec::new(),
            observed: Vec::new(),
            spans: BTreeMap::new(),
            span_ends: BTreeSet::new(),
        }
    }
    fn cursor(&self) -> PDFEmissionState {
        PDFEmissionState {
            source_geometry: Vec::new(),
            mapping_unsafe: self.state.mapping_unsafe,
            local_spans: Vec::new(),
            preview_spans: Vec::new(),
            local_text: Some((self.current_font, self.current_baseline, self.text_open)),
            fragment: false,
            historical_text: self.historical_text.clone(),
            origin: self.state.origin,
            matrix: self.state.matrix,
            saved: self.state.saved.clone(),
            historical_origin: self.state.historical_origin.clone(),
        }
    }
    fn number(&self, value: f64) -> String {
        format_number(value, self.context.precision)
    }
    fn font_number(&self, value: f64) -> String {
        format_number(value, if self.context.precision <= 4 { 4 } else { 9 })
    }
    fn rule_number(&self, value: f64) -> String {
        format!("{value:.p$}", p = self.context.precision)
    }
    fn end_text(&mut self) {
        if self.text_open {
            self.operators.push("ET".into())
        }
        self.text_open = false;
        self.current_font = None;
        self.current_baseline = None;
        self.historical_text = None
    }
    fn move_origin(&mut self, h: i64, v: i64) -> Result<()> {
        if let Some((dx, dy)) = self
            .state
            .historical_origin
            .as_mut()
            .unwrap()
            .move_to(h, v)?
        {
            self.operators.push(format!(
                "1 0 0 1 {} {} cm",
                self.number(dx),
                self.number(dy)
            ));
            self.state.origin.0 += dx;
            self.state.origin.1 += dy;
        }
        Ok(())
    }
    fn unicode_spans(&mut self, items: &[Placement]) {
        let mut start = 0;
        while start < items.len() {
            let first = &items[start];
            if first.kind != "glyph" || first.shaped.is_none() {
                start += 1;
                continue;
            }
            let mut end = start + 1;
            while end < items.len() {
                let next = &items[end];
                if next.kind != "glyph" || next.shaped.is_none() || next.font != first.font {
                    break;
                }
                end += 1
            }
            self.spans.insert(
                start,
                items[start..end]
                    .iter()
                    .map(|g| g.shaped.as_ref().unwrap().text.as_str())
                    .collect(),
            );
            self.span_ends.insert(end - 1);
            start = end;
        }
    }
    pub fn render(
        mut self,
        line: &LineDisplayList,
        finish_text: bool,
    ) -> Result<(Vec<String>, Vec<EmittedGlyph>)> {
        self.unicode_spans(&line.placements);
        let warning_started = crate::source_map::profile_start();
        let mut warnings: BTreeMap<_, (Vec<_>, std::collections::HashSet<_>)> = line
            .placements
            .iter()
            .filter(|p| p.box_warning.is_some())
            .map(|p| {
                (
                    p.placement_id,
                    (Vec::new(), std::collections::HashSet::new()),
                )
            })
            .collect();
        if !warnings.is_empty() {
            // Placement records are a depth-first traversal (including virtual
            // packets). Track the current ancestry once, rather than looking up
            // every parent of every glyph. A fragment's absent external parent
            // deliberately contributes no diagnostic ownership.
            let mut path = Vec::new();
            let mut active: Vec<(usize, usize)> = Vec::new();
            for item in &line.placements {
                while path.last().copied() != item.parent_id {
                    let Some(id) = path.pop() else { break };
                    if active.last().is_some_and(|&(warning, _)| warning == id) {
                        active.pop();
                    }
                }
                for &(id, depth) in active.iter().rev() {
                    if path.len() - depth >= 256 {
                        break;
                    }
                    let (origins, seen) = warnings.get_mut(&id).unwrap();
                    for source in item.origins.iter().flatten() {
                        if origins.len() < 4096 {
                            if let Some(range) = crate::source_map::Range::physical(source) {
                                if seen.insert(range) {
                                    origins.push(source.clone());
                                }
                            }
                        }
                    }
                }
                path.push(item.placement_id);
                if item.box_warning.is_some() {
                    active.push((item.placement_id, path.len()));
                }
            }
        }
        // Diagnostic references describe a union of source intervals, not
        // individual glyph ownership. Merge only touching physical ranges;
        // their union is unchanged, including definition and included sources.
        for (origins, _) in warnings.values_mut() {
            origins.sort_unstable_by(|a, b| {
                (&a.file, a.line, a.byte_column, a.end_byte_column).cmp(&(
                    &b.file,
                    b.line,
                    b.byte_column,
                    b.end_byte_column,
                ))
            });
            let mut merged: Vec<std::sync::Arc<crate::core::source::SourceSpan>> = Vec::new();
            for source in std::mem::take(origins) {
                if let Some(last) = merged.last_mut() {
                    if last.file == source.file
                        && last.line == source.line
                        && source.byte_column <= last.end_byte_column
                    {
                        if source.end_byte_column > last.end_byte_column {
                            std::sync::Arc::make_mut(last).end_byte_column = source.end_byte_column;
                        }
                        continue;
                    }
                }
                merged.push(source);
            }
            *origins = merged;
        }
        crate::source_map::record_warning_time(warning_started);
        let mut active: Option<LocalSpan> = None;
        let mut preview: Option<crate::paragraph_preview::Span> = None;
        for (index, item) in line.placements.iter().enumerate() {
            if preview.as_ref().is_some_and(|s| {
                item.preview_owner.as_ref().is_none_or(|o| {
                    o.root || o.region.key != s.owner.region.key || o.line != s.owner.line
                })
            }) {
                let mut s = preview.take().unwrap();
                s.end = self.operators.len();
                self.state.preview_spans.push(s);
            }
            if let Some(owner) = item.preview_owner.as_ref().filter(|o| o.root) {
                preview = Some(crate::paragraph_preview::Span {
                    owner: owner.clone(),
                    start: self.operators.len(),
                    end: 0,
                    entry: self.cursor(),
                    x: item.x_sp,
                    baseline: item.baseline_y_sp,
                    shift: line
                        .nodes
                        .iter()
                        .find(|n| n.node_id == item.node_id)
                        .map_or(0, |n| n.shift_sp),
                });
            }
            if active.as_ref().is_some_and(|span| {
                item.parameter
                    .is_none_or(|p| p.root || p.candidate != span.candidate || p.line != span.line)
            }) {
                let mut span = active.take().unwrap();
                span.end = self.operators.len();
                span.exit = self.cursor();
                self.state.local_spans.push(span);
            }
            if let Some(p) = item.parameter.filter(|p| p.root) {
                active = Some(LocalSpan {
                    candidate: p.candidate,
                    line: p.line,
                    placement: item.placement_id,
                    start: self.operators.len(),
                    end: 0,
                    entry: self.cursor(),
                    exit: Default::default(),
                });
            }
            if !self.state.mapping_unsafe && (item.kind == "rule" || item.box_warning.is_some()) {
                let x = sp_to_bp(item.x_sp);
                let y = sp_to_bp(self.page_height - item.baseline_y_sp);
                self.geometry(
                    item,
                    x,
                    y,
                    sp_to_bp(item.width_sp),
                    warnings
                        .get(&item.placement_id)
                        .map(|(origins, _)| origins.clone()),
                );
            }
            match item.kind.as_str() {
                "literal" => self.literal(item)?,
                "rule" => self.rule(item),
                "glyph" | "extraction_space" => self.glyph(item, index)?,
                _ => {}
            }
        }
        if let Some(mut s) = preview {
            s.end = self.operators.len();
            self.state.preview_spans.push(s);
        }
        if let Some(mut span) = active {
            span.end = self.operators.len();
            span.exit = self.cursor();
            self.state.local_spans.push(span);
        }
        if finish_text || (self.historical_text.is_none() && !self.state.fragment) {
            self.end_text()
        }
        if self.state.fragment {
            self.state.local_text =
                Some((self.current_font, self.current_baseline, self.text_open));
        }
        self.state.historical_text = self.historical_text;
        Ok((self.operators, self.observed))
    }
    fn geometry(
        &mut self,
        item: &Placement,
        x: f64,
        y: f64,
        width: f64,
        origins: Option<Vec<std::sync::Arc<crate::core::source::SourceSpan>>>,
    ) {
        if self.state.source_geometry.len() >= 65536 {
            return;
        }
        let top = y + sp_to_bp(item.height_sp);
        let bottom = y - sp_to_bp(item.depth_sp);
        let matrix = self.state.matrix;
        let quad = [(x, top), (x + width, top), (x + width, bottom), (x, bottom)].map(|(x, y)| {
            let (x, y) = matrix.point(x, y);
            [x, y]
        });
        if let Some(geometry) = crate::source_map::geometry(item, quad, origins) {
            self.state.source_geometry.push(geometry);
        }
    }
    fn object_origin(&self, item: &Placement) -> (f64, f64) {
        (
            sp_to_bp(item.x_sp) - self.state.origin.0,
            sp_to_bp(self.page_height - item.baseline_y_sp - item.depth_sp) - self.state.origin.1,
        )
    }
    fn image(&mut self, item: &Placement) -> Result<()> {
        self.end_text();
        let n = item
            .literal_text
            .parse::<usize>()
            .map_err(|_| Error::value("Invalid image placement"))?;
        let image = &self.context.document.images[&n];
        let (mut x, mut y) = self.object_origin(item);
        let (mut sx, mut sy) = (
            sp_to_bp(item.width_sp),
            sp_to_bp(item.height_sp + item.depth_sp),
        );
        if image.format == "PDF" {
            sx = item.width_sp as f64 / image.natural_width as f64;
            sy = (item.height_sp + item.depth_sp) as f64 / image.natural_height as f64;
            x -= image.bbox[0] * sx;
            y -= image.bbox[1] * sy;
        }
        let transform = if self.context.exact_repro {
            let digits = if image.format == "PDF" { 6 } else { 4 };
            format!(
                "{sx:.digits$} 0 0 {sy:.digits$} {x:.p$} {y:.p$}",
                p = self.context.precision
            )
        } else {
            format!(
                "{} 0 0 {} {} {}",
                self.number(sx),
                self.number(sy),
                self.number(x),
                self.number(y)
            )
        };
        self.operators.push(format!(
            "q {transform} cm /Im{}{} Do Q",
            image.ordinal, self.context.resource_suffix
        ));
        Ok(())
    }
    fn form(&mut self, item: &Placement) {
        self.end_text();
        let (x, y) = self.object_origin(item);
        self.operators.push(format!(
            "q 1 0 0 1 {} {} cm /{} Do Q",
            self.number(x),
            self.number(y),
            item.literal_text
        ));
    }
    fn literal(&mut self, item: &Placement) -> Result<()> {
        if item.literal_mode == "image" {
            return self.image(item);
        }
        if item.literal_mode == "xform" {
            self.form(item);
            return Ok(());
        }
        if item.literal_mode != "direct" {
            self.end_text()
        }
        let (mut x, mut y) = (
            sp_to_bp(item.x_sp),
            sp_to_bp(self.page_height - item.baseline_y_sp),
        );
        if matches!(
            item.literal_mode.as_str(),
            "origin" | "matrix" | "save" | "restore"
        ) {
            if self.context.exact_repro {
                self.move_origin(item.x_sp, item.baseline_y_sp)?;
                (x, y) = self.state.origin
            } else {
                let (ox, oy) = self.state.origin;
                self.operators.push(format!(
                    "1 0 0 1 {} {} cm",
                    self.number(x - ox),
                    self.number(y - oy)
                ));
                self.state.origin = (x, y)
            }
        }
        match item.literal_mode.as_str() {
            "matrix" => {
                self.state.matrix =
                    Affine::around(&item.literal_text, x, y)?.then(self.state.matrix);
                self.operators.push(format!("{} 0 0 cm", item.literal_text))
            }
            "save" => {
                self.state
                    .saved
                    .push((self.state.origin, self.state.matrix));
                self.operators.push("q".into())
            }
            "restore" => {
                self.operators.push("Q".into());
                if let Some((origin, matrix)) = self.state.saved.pop() {
                    self.state.origin = origin;
                    self.state.matrix = matrix
                }
            }
            _ => {
                let safe = lopdf::content::Content::decode(&crate::pdf::writer::latin1(
                    &item.literal_text,
                )?)
                .is_ok_and(|content| {
                    content.operations.iter().all(|op| {
                        matches!(
                            op.operator.as_str(),
                            "g" | "G" | "rg" | "RG" | "k" | "K" | "w" | "J" | "j" | "M" | "d"
                        )
                    })
                });
                self.state.mapping_unsafe |= !safe;
                self.operators.push(item.literal_text.clone());
            }
        }
        Ok(())
    }
    fn rule(&mut self, item: &Placement) {
        let height = item.height_sp + item.depth_sp;
        if item.width_sp <= 0 || height <= 0 {
            return;
        }
        let (mut x, mut y) = self.object_origin(item);
        self.end_text();
        let (width, thickness) = (sp_to_bp(item.width_sp), sp_to_bp(height));
        let op = if !self.context.exact_repro {
            format!(
                "q {} {} {} {} re f Q",
                self.number(x),
                self.number(y),
                self.number(width),
                self.number(thickness)
            )
        } else if height <= bp_to_sp(1.0) {
            y += sp_to_bp((height + 1) / 2);
            format!(
                "q 1 0 0 1 {} {} cm []0 d 0 J {} w 0 0 m {} 0 l S Q",
                self.rule_number(x),
                self.rule_number(y),
                self.rule_number(thickness),
                self.rule_number(width)
            )
        } else if item.width_sp <= bp_to_sp(1.0) {
            x += sp_to_bp((item.width_sp + 1) / 2);
            format!(
                "q 1 0 0 1 {} {} cm []0 d 0 J {} w 0 0 m 0 {} l S Q",
                self.rule_number(x),
                self.rule_number(y),
                self.rule_number(width),
                self.rule_number(thickness)
            )
        } else {
            format!(
                "q {} {} {} {} re f Q",
                self.rule_number(x),
                self.rule_number(y),
                self.rule_number(width),
                self.rule_number(thickness)
            )
        };
        self.operators.push(op)
    }
    fn prepare_text(&mut self, item: &Placement, historical: bool) -> Result<()> {
        if !historical
            && item.shaped.is_none()
            && self
                .current_baseline
                .is_some_and(|b| b != item.baseline_y_sp)
            || self.historical_text.is_some() && !historical
        {
            self.end_text()
        }
        if !self.text_open {
            if self.context.exact_repro {
                self.move_origin(0, self.page_height)?
            }
            self.operators.push("BT".into());
            self.text_open = true
        }
        if historical && self.historical_text.is_none() {
            if self.current_font.is_some() {
                self.end_text();
                self.move_origin(0, self.page_height)?;
                self.operators.push("BT".into());
                self.text_open = true;
            }
            let origin = self.state.historical_origin.as_ref().unwrap();
            self.historical_text = Some(HistoricalText::new(
                self.page_height,
                Some(self.context.precision as u32),
                Some((origin.h, origin.v)),
            ));
        }
        Ok(())
    }
    fn modern_glyph(
        &mut self,
        item: &Placement,
        index: usize,
        resource: &str,
        cid: Option<u16>,
        mut size: f64,
        mut advance: Option<f64>,
        expansion: f64,
    ) -> (f64, f64, f64, Option<f64>) {
        let (mut x, mut y) = (
            sp_to_bp(item.x_sp),
            sp_to_bp(self.page_height - item.baseline_y_sp),
        );
        let origin = self.state.origin;
        if self.current_font != Some(item.font) {
            self.operators
                .push(format!("/{resource} {} Tf", self.font_number(size)))
        }
        if let Some(text) = self.spans.get(&index) {
            let original = std::iter::once(0xfeff)
                .chain(text.encode_utf16())
                .map(|c| format!("{c:04X}"))
                .collect::<String>();
            self.operators
                .push(format!("/Span << /ActualText <{original}> >> BDC"));
        }
        self.operators.push(format!(
            "{} 0 0 1 {} {} Tm",
            self.font_number(expansion),
            self.number(x - origin.0),
            self.number(y - origin.1)
        ));
        self.operators.push(if let Some(cid) = cid {
            format!("<{cid:04X}> Tj")
        } else {
            format!("({}) Tj", character_string(item.code))
        });
        if self.span_ends.contains(&index) {
            self.operators.push("EMC".into())
        }
        if self.context.precision < 9 {
            let actual = self.font_number(size).parse::<f64>().unwrap();
            x = origin.0 + self.number(x - origin.0).parse::<f64>().unwrap();
            y = origin.1 + self.number(y - origin.1).parse::<f64>().unwrap();
            advance = advance.map(|a| a * actual / size);
            size = actual;
        }
        (x, y, size, advance)
    }
    fn glyph(&mut self, item: &Placement, index: usize) -> Result<()> {
        let id = item.font;
        let code = item.code;
        let group_index = self.context.resources.use_font(
            id,
            self.context.fonts,
            self.context.env,
            self.context.warn,
        )?;
        let group = &mut self.context.resources.font_groups[group_index];
        let cid = item
            .shaped
            .as_ref()
            .map(|g| group.unicode_code(g))
            .transpose()?;
        let canonical = group.font;
        let selected = self.context.fonts.items[id].clone();
        let size = sp_to_bp(selected.size);
        let mut advance = if let Some(g) = &item.shaped {
            let font = selected.unicode_font.as_ref().unwrap();
            Some(font.advances[g.glyph_id as usize] as f64 * size / font.upem as f64)
        } else {
            self.context.fonts.emitted_advance(id, code)?
        };
        let first = &self.context.fonts.items[canonical];
        let bitmap = first
            .materialization
            .resource
            .as_deref()
            .is_some_and(|r| matches!(r.kind(), "pk" | "pgc"));
        let scalable = first.materialization.embedding_available
            && first
                .tfm_widths
                .as_ref()
                .is_some_and(|w| w.contains_key(&code))
            && !bitmap;
        if scalable {
            advance = Some(first.pdf_char_width(code)? * size / 1000.0)
        }
        let expansion = if selected.expansion_policy.is_some_and(|p| p.automatic) {
            selected.expansion_ratio as f64 / 1000.0 + 1.0
        } else {
            1.0
        };
        advance = advance.map(|n| n * expansion);
        let historical = self.context.exact_repro
            && item.shaped.is_none()
            && advance.is_some()
            && (scalable || bitmap);
        self.prepare_text(item, historical)?;
        let resource = format!("F{canonical}{}", self.context.resource_suffix);
        let (x, y, size, advance) = if historical {
            let (x, y, size, advance) = self.historical_text.as_mut().unwrap().emit(
                item,
                &selected,
                &resource,
                self.state.origin,
                advance.unwrap(),
                &mut self.operators,
                if expansion != 1.0 {
                    selected.expansion_ratio
                } else {
                    0
                },
                if bitmap {
                    self.context.fonts.pdf_options.as_ref()
                } else {
                    None
                },
            )?;
            (x, y, size, Some(advance))
        } else {
            self.modern_glyph(item, index, &resource, cid, size, advance, expansion)
        };
        self.current_font = Some(id);
        self.current_baseline = Some(item.baseline_y_sp);
        let quality = if historical {
            "historical_text_cursor"
        } else if advance.is_some() {
            "explicit_matrix"
        } else {
            "explicit_matrix_unknown_advance"
        };
        if !self.state.mapping_unsafe && item.kind == "glyph" {
            self.geometry(
                item,
                x,
                y,
                advance.unwrap_or_else(|| sp_to_bp(item.width_sp)),
                None,
            );
        }
        let (x, y) = self.state.matrix.point(x, y);
        self.observed.push(EmittedGlyph {
            placement_id: item.placement_id,
            code: Some(code),
            font: id,
            size_bp: size,
            x_bp: Some(x),
            baseline_y_bp: y,
            advance_bp: advance,
            position_quality: quality.into(),
        });
        Ok(())
    }
}
pub fn format_number(value: f64, precision: usize) -> String {
    let text = format!("{value:.precision$}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() {
        "0".into()
    } else {
        text.into()
    }
}
