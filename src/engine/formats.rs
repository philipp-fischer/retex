//! Initialization snapshots and fresh-job restoration over native components.
use super::*;
use crate::serialization::formats::{self, FileResource, FormatEngine, FormatSnapshot};
use std::rc::Rc;
pub(super) fn capture(v: &mut EngineView<'_>) -> Result<FormatSnapshot> {
    if !v.env.groups.is_empty()
        || !v.core.conditionals.stack.is_empty()
        || !v.builder.context_stack.is_empty()
        || v.builder.context.cur_list_head != C::NULL
        || !v.pages.roots().is_empty()
        || !v.vertical.roots().is_empty()
    {
        return Err(Error::value(
            "Cannot dump a format with a group, conditional, nested list or unfinished typesetting material",
        ));
    }
    let directory = v.fonts.directory.clone();
    let fonts = v
        .fonts
        .dump_state(|name| formats::file_resource(name, Some(&directory)))?;
    let shared_micro_codes = v
        .fonts
        .items
        .iter()
        .enumerate()
        .map(|(i, font)| {
            v.fonts.items[..i]
                .iter()
                .position(|other| Rc::ptr_eq(&font.micro_codes, &other.micro_codes))
                .unwrap_or(i)
        })
        .collect();
    let mut pdf = v.core.backend.document.clone();
    pdf.reset_job();
    Ok(FormatSnapshot {
        version: formats::FORMAT_VERSION,
        engine: "pdfTeX-1.40.25-rust".into(),
        font_id_base: C::FONT_ID_BASE,
        language_profile: v.core.config.language_profile,
        primitive_signature: formats::primitive_signature(v.env)?,
        interaction: v.core.interaction,
        environment: v.env.dump_state(),
        input: v.core.input.dump_state(),
        expansion: v.core.expansion.dump_state(),
        hyphenation: v.hyphenation.dump_state(),
        nodes: v.builder.dump_state(),
        unicode_maps: v.core.backend.unicode_maps.entries.clone(),
        pdf_document: pdf,
        pdf_resources: v.core.backend.resources.clone(),
        fonts,
        shared_micro_codes,
    })
}
impl FormatEngine for Engine {
    fn format_is_fresh(&self) -> bool {
        self.core.ready_already == 0 && self.core.input.in_open == 0
    }
    fn format_is_quiescent(&self) -> bool {
        self.env.groups.is_empty()
            && self.core.conditionals.stack.is_empty()
            && self.builder.context_stack.is_empty()
            && self.builder.context.cur_list_head == C::NULL
            && self.page_builder.roots().is_empty()
            && self.vertical.roots().is_empty()
    }
    fn capture_format(&mut self) -> Result<FormatSnapshot> {
        capture(&mut self.view())
    }
    fn initialize_format_primitives(&mut self) -> Result<()> {
        self.init_prim()
    }
    fn primitive_signature(&self) -> Result<String> {
        formats::primitive_signature(&self.env)
    }
    fn language_profile(&self) -> LanguageProfile {
        self.core.config.language_profile
    }
    fn restore_format(&mut self, mut state: FormatSnapshot, directory: &Path) -> Result<()> {
        let mut resolved = BTreeMap::new();
        for font in &state.fonts.fonts {
            for (record, kind) in [
                (&font.tfm, "tfm"),
                (&font.pfb, "type1 fonts"),
                (&font.enc, "enc files"),
                (&font.unicode, "truetype fonts"),
            ] {
                if let Some(record) = record {
                    let path = formats::checked_resource(
                        Some(record.clone()),
                        Some(&mut self.fonts),
                        kind,
                        directory,
                    )?
                    .unwrap();
                    resolved.insert(record.path.clone(), path);
                }
            }
        }
        for (i, &shared) in state.shared_micro_codes.iter().enumerate() {
            if shared > i || i >= state.fonts.fonts.len() {
                return Err(Error::value("Invalid shared font state in format"));
            }
            if shared != i {
                state.fonts.fonts[i].micro_codes = state.fonts.fonts[shared].micro_codes.clone()
            }
        }
        self.fonts
            .restore_state(state.fonts, |r: Option<FileResource>, _| {
                r.map(|r| {
                    resolved
                        .get(&r.path)
                        .cloned()
                        .ok_or_else(|| Error::value("Unresolved format font resource"))
                })
                .transpose()
            })?;
        self.env.restore_state(state.environment)?;
        self.core.input.restore_state(state.input)?;
        self.core.expansion.restore_state(state.expansion)?;
        self.hyphenation.restore_state(state.hyphenation)?;
        self.builder.restore_state(state.nodes)?;
        self.core.backend.unicode_maps.entries = state.unicode_maps;
        self.core.backend.document = state.pdf_document;
        self.core.backend.document.reset_job();
        self.core.backend.resources = state.pdf_resources;
        let date = startup_date();
        for (name, value) in [
            ("time", date.hour * 60 + date.minute),
            ("day", date.day),
            ("month", date.month),
            ("year", date.year),
        ] {
            self.env
                .values
                .insert(ValueKey::parameter(name), Quantity::Integer(value));
        }
        self.core.interaction = self
            .core
            .config
            .interaction_mode
            .unwrap_or(state.interaction);
        self.core.input.ini = true;
        self.fonts.ini = true;
        self.core.ready_already = 314159;
        self.view().refresh_roots();
        Ok(())
    }
}
