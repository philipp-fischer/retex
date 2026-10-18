//! Resource binding, frozen evidence and complete PDF file assembly.
use crate::{
    core::{
        constants::Constants as C,
        errors::{Error, Result},
        units::sp_to_bp,
        values::round_ratio,
    },
    fonts::{catalog::FontCatalog, materialization::FontResource},
    language::{input::InputProcessor, state::Environment},
    pdf::{
        document::PDFDocument,
        emission::{EmissionContext, LineEmitter, PDFEmissionState},
        font_embedding::{create_embedded_font, write_bitmap_font},
        geometry::magnified_dimension,
        resources::PDFResources,
        syntax::pdf_name,
        unicode_fonts::embed_unicode_font,
        unicode_maps::UnicodeMaps,
        writer::latin1,
    },
    typesetting::display::{LineDisplayList, PageDisplayList, Placement},
};
use md5::{Digest, Md5};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
#[derive(Clone, Default)]
pub struct PDFBackend {
    pub resources: PDFResources,
    pub document: PDFDocument,
    pub unicode_maps: UnicodeMaps,
    pub exact_repro: bool,
}
pub struct PdfWriteOptions {
    pub version: (u8, u8),
    pub precision: usize,
    pub compression: i64,
    pub omit_charset: bool,
    pub unicode_enabled: bool,
    pub omit_procset: i64,
    pub omit_info: bool,
    pub information: String,
    pub pages_attributes: String,
}
impl PDFBackend {
    pub fn use_font(
        &mut self,
        n: usize,
        fonts: &mut FontCatalog,
        env: &mut Environment,
        warn: &mut dyn FnMut(&str),
    ) -> Result<usize> {
        self.resources.use_font(n, fonts, env, warn)
    }
    pub fn materialize_fonts(
        &mut self,
        fonts: &mut FontCatalog,
        env: &Environment,
        warn: &mut dyn FnMut(&str),
    ) -> Result<()> {
        let mut groups = self.resources.font_groups.clone();
        groups.sort_by_key(|g| std::cmp::Reverse(g.font));
        let suppress = InputProcessor::parameter(env, "pdfsuppresswarningdupmap", 0) > 0;
        for g in groups {
            fonts.materialize(g.font, suppress, warn)?;
            let first = fonts.items[g.font].materialization.clone();
            for member in g.members {
                if member == g.font {
                    continue;
                }
                let other = &mut fonts.items[member].materialization;
                other.resource = first.resource.clone();
                other.pfb_file = first.pfb_file.clone();
                other.pfb_data = first.pfb_data.clone();
                other.native_encoding = first.native_encoding.clone();
                other.pdf_name = first.pdf_name.clone();
                other.encoding = first.encoding.clone();
                other.enc_file = first.enc_file.clone();
                other.resource_error = first.resource_error.clone();
                other.resource_ready = true;
                other.embedding_available = first.embedding_available;
            }
        }
        Ok(())
    }
    pub fn render_line(
        &mut self,
        line: &LineDisplayList,
        height: i64,
        state: &mut PDFEmissionState,
        finish: bool,
        fonts: &mut FontCatalog,
        env: &mut Environment,
        warn: &mut dyn FnMut(&str),
    ) -> Result<(Vec<String>, Vec<crate::typesetting::display::EmittedGlyph>)> {
        let precision = self.document.decimal_digits.unwrap_or(9);
        let suffix = self.document.resource_suffix.clone().unwrap_or_default();
        let mut c = EmissionContext {
            fonts,
            env,
            resources: &mut self.resources,
            document: &mut self.document,
            precision,
            exact_repro: self.exact_repro,
            resource_suffix: suffix,
            warn,
        };
        LineEmitter::new(&mut c, height, state).render(line, finish)
    }
    pub fn prepare_pages(
        &mut self,
        pages: &[std::sync::Arc<PageDisplayList>],
        fonts: &mut FontCatalog,
        env: &mut Environment,
        warn: &mut dyn FnMut(&str),
    ) -> Result<Vec<PageDisplayList>> {
        self.prepare_page_range(pages, 0, fonts, env, warn)
    }
    pub(crate) fn prepare_page_range(
        &mut self,
        pages: &[std::sync::Arc<PageDisplayList>],
        first: usize,
        fonts: &mut FontCatalog,
        env: &mut Environment,
        warn: &mut dyn FnMut(&str),
    ) -> Result<Vec<PageDisplayList>> {
        self.prepare_page_range_impl(pages, first, fonts, env, warn, true)
    }
    pub(crate) fn prepare_preview_page_range(
        &mut self,
        pages: &[std::sync::Arc<PageDisplayList>],
        first: usize,
        fonts: &mut FontCatalog,
        env: &mut Environment,
        warn: &mut dyn FnMut(&str),
    ) -> Result<Vec<PageDisplayList>> {
        self.prepare_page_range_impl(pages, first, fonts, env, warn, false)
    }
    fn prepare_page_range_impl(
        &mut self,
        pages: &[std::sync::Arc<PageDisplayList>],
        first: usize,
        fonts: &mut FontCatalog,
        env: &mut Environment,
        warn: &mut dyn FnMut(&str),
        retain_nodes: bool,
    ) -> Result<Vec<PageDisplayList>> {
        if !pages.is_empty() && self.document.draft != Some(true) {
            self.materialize_fonts(fonts, env, warn)?
        }
        if self.document.draft == Some(true) {
            return Ok(pages.iter().map(|page| page.as_ref().clone()).collect());
        }
        let mut result = Vec::new();
        for (index, page) in pages.iter().enumerate() {
            let allocated = &self.resources.pages[first + index];
            let mag = page.magnification;
            let scale = mag as f64 / 1000.0;
            let width = magnified_dimension(page.width_sp, mag, self.exact_repro)? as i64;
            let height = magnified_dimension(page.height_sp, mag, self.exact_repro)? as i64;
            let dimension =
                |value: i64| round_ratio(value as i128 * mag as i128, 1000).map(|n| n as i64);
            let baseline = |value: i64| dimension(page.height_sp - value).map(|n| height - n);
            let mut state = PDFEmissionState::default();
            let startup = LineDisplayList {
                source_geometry: Vec::new(),
                preview_spans: Vec::new(),
                local_spans: Vec::new(),
                root_node: C::NULL,
                baseline_y_sp: 0,
                placements: allocated
                    .startup
                    .iter()
                    .map(|p| Placement {
                        kind: "literal".into(),
                        literal_mode: p.literal_mode.clone(),
                        literal_text: p.literal_text.clone(),
                        x_sp: p.x_sp,
                        baseline_y_sp: p.baseline_y_sp,
                        ..Placement::default()
                    })
                    .collect(),
                nodes: Vec::new(),
                natural_width_sp: 0,
                target_width_sp: 0,
                overfull_sp: 0,
                operators: Vec::new(),
                emitted: Vec::new(),
                position_adjustment_sp: 0,
            };
            let (mut initial, _) =
                self.render_line(&startup, page.height_sp, &mut state, true, fonts, env, warn)?;
            let mut lines = Vec::new();
            for (i, original) in page.lines.iter().enumerate() {
                let mut line = LineDisplayList {
                    source_geometry: Vec::new(),
                    preview_spans: Vec::new(),
                    local_spans: Vec::new(),
                    root_node: original.root_node,
                    baseline_y_sp: original.baseline_y_sp,
                    placements: if retain_nodes {
                        original.placements.clone()
                    } else {
                        Vec::new()
                    },
                    nodes: if retain_nodes {
                        original.nodes.clone()
                    } else {
                        Vec::new()
                    },
                    natural_width_sp: original.natural_width_sp,
                    target_width_sp: original.target_width_sp,
                    overfull_sp: original.overfull_sp,
                    operators: Vec::new(),
                    emitted: Vec::new(),
                    position_adjustment_sp: original.position_adjustment_sp,
                };
                for item in &mut line.placements {
                    if item.kind == "glyph" && item.shaped.is_none() {
                        let (name, mapping) = fonts.glyph_name(item.font, item.code);
                        item.glyph_name = name;
                        item.mapping = mapping.into();
                    }
                }
                let (ops, mut emitted) = self.render_line(
                    if retain_nodes { &line } else { original },
                    page.height_sp,
                    &mut state,
                    i + 1 == page.lines.len(),
                    fonts,
                    env,
                    warn,
                )?;
                line.source_geometry = std::mem::take(&mut state.source_geometry);
                line.preview_spans = std::mem::take(&mut state.preview_spans);
                for span in &mut line.preview_spans {
                    span.start += initial.len();
                    span.end += initial.len();
                }
                line.local_spans = std::mem::take(&mut state.local_spans);
                for span in &mut line.local_spans {
                    span.start += initial.len();
                    span.end += initial.len();
                }
                let operators = if initial.is_empty() {
                    ops
                } else {
                    initial.extend(ops);
                    std::mem::take(&mut initial)
                };
                if mag != 1000 {
                    line.baseline_y_sp = baseline(line.baseline_y_sp)?;
                    for item in &mut line.placements {
                        item.x_sp = dimension(item.x_sp)?;
                        item.baseline_y_sp = baseline(item.baseline_y_sp)?;
                        item.width_sp = dimension(item.width_sp)?;
                        item.height_sp = dimension(item.height_sp)?;
                        item.depth_sp = dimension(item.depth_sp)?;
                    }
                    for geometry in &mut line.source_geometry {
                        for point in &mut std::sync::Arc::make_mut(&mut geometry.data).quad {
                            point[0] *= scale;
                            point[1] *= scale;
                        }
                    }
                    for g in &mut emitted {
                        g.size_bp *= scale;
                        g.x_bp = g.x_bp.map(|v| v * scale);
                        g.baseline_y_bp *= scale;
                        g.advance_bp = g.advance_bp.map(|v| v * scale);
                    }
                }
                line.operators = operators;
                line.emitted = emitted;
                lines.push(line)
            }
            result.push(PageDisplayList {
                page: page.page,
                width_sp: width,
                height_sp: height,
                lines,
                magnification: page.magnification,
            });
        }
        Ok(result)
    }
    pub fn write_pdf(
        &mut self,
        pages: &[PageDisplayList],
        filename: &Path,
        fonts: &FontCatalog,
        options: &PdfWriteOptions,
        warn: &mut dyn FnMut(&str),
    ) -> Result<bool> {
        let Some(bytes) = self.pdf_bytes(pages, fonts, options, warn)? else {
            return Ok(false);
        };
        use std::io::Write;
        crate::resources::host::open_output(filename, false)?.write_all(&bytes)?;
        crate::resources::host::console_line(
            false,
            format!("PDF written to {}", filename.display()),
        );
        Ok(true)
    }
    /// Serialization is separate from publishing a file. Browser download bytes
    /// must not become auxiliary inputs or speculative TeX output files.
    pub fn pdf_bytes(
        &mut self,
        pages: &[PageDisplayList],
        fonts: &FontCatalog,
        options: &PdfWriteOptions,
        warn: &mut dyn FnMut(&str),
    ) -> Result<Option<Vec<u8>>> {
        if pages.is_empty() {
            crate::resources::host::console_line(true, format!("No pages to output"));
            return Ok(None);
        }
        if self.document.draft == Some(true) {
            crate::resources::host::console_line(
                false,
                format!("PDF draft mode: output PDF left unchanged"),
            );
            return Ok(None);
        }
        let mut pdf = self.resources.writer();
        pdf.compress_level = options.compression.clamp(0, 9) as u32;
        pdf.object_level = self.document.object_compression;
        pdf.object_stream_numbers = self.document.object_stream_numbers.clone();
        let mut free = self
            .document
            .write_objects(&mut pdf, &self.resources, options.precision)?;
        free.extend(pdf.object_stream_numbers.iter().copied());
        let (navigation_catalog, navigation_names, navigation_free) =
            self.document.navigation.write(
                &mut pdf,
                self.resources.pages[0].page,
                (pages[0].width_sp, pages[0].height_sp),
                options.precision,
                pages[0].magnification,
                warn,
            )?;
        free.extend(navigation_free);
        for &number in self.resources.fonts.keys() {
            let font = &fonts.items[number];
            if let Some(error) = &font.materialization.resource_error {
                return Err(Error::value(error.clone()));
            }
            if (font.dynamic || font.tfm_widths.is_some())
                && !font.materialization.nonembedded
                && !font.materialization.embedding_available
            {
                return Err(Error::value(format!(
                    "Cannot render font {}: missing Type1 PFB outlines (TFM metrics loaded from {:?})",
                    font.name, font.tfm_file
                )));
            }
            if font.metric_mode() == "approximate" {
                return Err(Error::value(format!(
                    "Cannot render font {}: missing font metrics",
                    font.name
                )));
            }
        }
        let mut font_objects = BTreeMap::new();
        for group in &self.resources.font_groups {
            let id = group.font;
            let mut font = fonts.items[id].clone();
            if font.unicode_font.is_some() {
                font_objects.insert(id, embed_unicode_font(&mut pdf, &font, group)?);
                continue;
            }
            let mut included = BTreeSet::new();
            let mut attributes = font.materialization.font_attributes.clone();
            for &member in &group.members {
                let other = &fonts.items[member].materialization;
                included.extend(&other.included_codes);
                if attributes.is_empty() {
                    attributes = other.font_attributes.clone()
                } else if !other.font_attributes.is_empty() && attributes != other.font_attributes {
                    warn(r"\pdffontattr: fonts have conflicting attributes")
                }
            }
            font.materialization.included_codes = included.clone();
            font.materialization.font_attributes = attributes.clone();
            let mut codes = pages
                .iter()
                .flat_map(|p| &p.lines)
                .flat_map(|l| &l.emitted)
                .filter(|g| group.members.contains(&g.font))
                .filter_map(|g| g.code)
                .collect::<BTreeSet<_>>();
            for form in self.document.forms.values() {
                for member in &group.members {
                    if let Some(c) = form.codes.get(member) {
                        codes.extend(c);
                    }
                }
            }
            if let Some(FontResource::Bitmap(resource)) = font.materialization.resource.as_deref() {
                codes.extend(included);
                if codes.is_empty() {
                    codes.insert(0);
                }
                let options_bitmap = fonts
                    .pdf_options
                    .as_ref()
                    .ok_or_else(|| Error::value("Bitmap PDF options not frozen"))?;
                font_objects.insert(
                    id,
                    write_bitmap_font(
                        resource,
                        &mut pdf,
                        &font,
                        id,
                        &codes,
                        group.object_number,
                        options_bitmap,
                        Some(&self.unicode_maps),
                        options.unicode_enabled,
                        warn,
                    )?,
                );
                continue;
            }
            if (font.materialization.pfb_data.is_some() || font.materialization.nonembedded)
                && font.tfm_widths.is_some()
            {
                let n = create_embedded_font(
                    &mut pdf,
                    &font,
                    id,
                    Some(&self.unicode_maps),
                    Some(&codes),
                    Some(group.object_number),
                    options.omit_charset,
                    options.unicode_enabled,
                    warn,
                )?;
                if n != 0 {
                    font_objects.insert(id, n);
                }
            }
            if !font_objects.contains_key(&id) {
                if font.afm.is_none() {
                    return Err(Error::value(format!(
                        "Cannot serialize PDF font {}",
                        font.name
                    )));
                }
                font_objects.insert(
                    id,
                    pdf.add_text_object(
                        &format!(
                            "<< /Type /Font /Subtype /Type1 /BaseFont /{} {attributes} >>",
                            pdf_name(&font.materialization.pdf_name)?
                        ),
                        Some(group.object_number),
                        1,
                    )?,
                );
            }
        }
        let pages_num = if self.resources.page_groups.len() == 1 {
            self.resources.page_groups[0].0
        } else {
            pdf.reserve_object()
        };
        let suffix = self.document.resource_suffix.as_deref().unwrap_or("");
        let precision = options.precision;
        for (page, allocated) in pages.iter().zip(&self.resources.pages) {
            let content = page
                .lines
                .iter()
                .flat_map(|l| &l.operators)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            let mut content = latin1(&content)?;
            if page.magnification != 1000 {
                let scale = super::emission::format_number(page.magnification as f64 / 1000.0, 3);
                let mut prefix = format!("{scale} 0 0 {scale} 0 0 cm\n").into_bytes();
                prefix.extend(content);
                content = prefix
            }
            let font_dictionary = font_objects
                .iter()
                .map(|(id, n)| format!("/F{id}{suffix} {n} 0 R"))
                .collect::<Vec<_>>()
                .join(" ");
            let content_num = pdf.add_stream_object(
                &content,
                "",
                Some(allocated.content),
                Some(allocated.compress_level),
            )?;
            let objects = allocated
                .xobjects
                .iter()
                .map(|(name, n)| format!("/{name} {n} 0 R"))
                .collect::<Vec<_>>()
                .join(" ");
            let procset =
                if options.omit_procset < 0 || options.omit_procset == 0 && options.version.0 < 2 {
                    "/ProcSet [/PDF /Text]"
                } else {
                    ""
                };
            pdf.add_text_object(
                &format!(
                    "<< /Font << {font_dictionary} >> /XObject << {objects} >> {procset} {} >>",
                    allocated.extra_resources
                ),
                Some(allocated.resources),
                1,
            )?;
            let media = if allocated.attributes.contains("/MediaBox") {
                String::new()
            } else {
                format!(
                    "/MediaBox [0 0 {:.precision$} {:.precision$}]",
                    sp_to_bp(page.width_sp),
                    sp_to_bp(page.height_sp)
                )
            };
            let group = if allocated.group != 0 {
                format!("/Group {} 0 R", allocated.group)
            } else {
                String::new()
            };
            let annotations = references("Annots", &allocated.annotations);
            let beads = references("B", &allocated.beads);
            pdf.add_text_object(&format!("<<\n  /Type /Page\n  /Parent {} 0 R\n  {media}\n  /Contents {content_num} 0 R\n  /Resources {} 0 R\n  {}\n  {group}\n  {annotations}\n  {beads}\n>>",allocated.parent,allocated.resources,allocated.attributes),Some(allocated.page),1)?;
        }
        for (parent, children) in &self.resources.page_groups {
            let refs = children
                .iter()
                .map(|n| format!("{n} 0 R"))
                .collect::<Vec<_>>()
                .join(" ");
            let parent_ref = if *parent != pages_num {
                format!(" /Parent {pages_num} 0 R")
            } else {
                String::new()
            };
            pdf.add_text_object(
                &format!(
                    "<< /Type /Pages /Kids [{refs}] /Count {}{parent_ref} {} >>",
                    children.len(),
                    options.pages_attributes
                ),
                Some(*parent),
                1,
            )?;
        }
        if self.resources.page_groups.len() > 1 {
            let refs = self
                .resources
                .page_groups
                .iter()
                .map(|(n, _)| format!("{n} 0 R"))
                .collect::<Vec<_>>()
                .join(" ");
            pdf.add_text_object(
                &format!(
                    "<< /Type /Pages /Kids [{refs}] /Count {} {} >>",
                    pages.len(),
                    options.pages_attributes
                ),
                Some(pages_num),
                1,
            )?;
        }
        let name_entries = format!("{} {navigation_names}", self.document.names);
        let names = if name_entries.trim().is_empty() {
            String::new()
        } else {
            format!("/Names << {name_entries} >>")
        };
        let action = if self.document.open_action.is_empty() {
            String::new()
        } else {
            format!("/OpenAction {} 0 R", self.document.open_action_number)
        };
        let catalog=pdf.add_text_object(&format!("<< /Type /Catalog /Pages {pages_num} 0 R {names} {action} {navigation_catalog} {} >>",self.document.catalog),None,1)?;
        let mut trailer = self.document.trailer.clone();
        if !options.omit_info {
            let info = pdf.add_text_object(&format!("<< {} >>", options.information), None, 3)?;
            trailer.push_str(&format!(" /Info {info} 0 R"));
        }
        if !self.document.trailer_id.is_empty() {
            let seed = latin1(&self.document.trailer_id)?;
            let seed = seed.split(|&c| c == 0).next().unwrap_or(&[]);
            let digest = format!("{:X}", Md5::digest(seed));
            trailer.push_str(&format!(" /ID [<{digest}> <{digest}>]"));
        }
        let bytes = pdf.serialize(
            catalog,
            options.version,
            &trailer,
            &free.into_iter().collect::<Vec<_>>(),
        )?;
        Ok(Some(bytes))
    }
}
fn references(key: &str, values: &[usize]) -> String {
    if values.is_empty() {
        String::new()
    } else {
        format!(
            "/{key} [{}]",
            values
                .iter()
                .map(|n| format!("{n} 0 R"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }
}
