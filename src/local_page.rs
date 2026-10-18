//! Portable preview-only recipe. The only executable operations here are pure
//! native placement, extraction and painting; no TeX continuation is replayed.
use crate::{
    Engine,
    checked_material::PaintSite,
    core::{
        constants::Constants as C,
        errors::{Error, Result},
    },
    engine::local::Ready,
    fonts::catalog::FontCatalog,
    language::state::Environment,
    pdf::{backend::PDFBackend, extraction::ExtractionServices, preview::RetainedPreview},
    typesetting::{
        display::PageDisplayList,
        layout::{Layout, TypesettingContext},
    },
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct Base {
    pub tag: crate::resources::effects::DocumentRevision,
    pub pass: usize,
    pub ready: Ready,
    pub epoch: u64,
    pub policy: u64,
    pub clock: (u64, i32),
    pub wall: Option<(std::time::SystemTime, i32)>,
    pub backend: PDFBackend,
    pub fonts: FontCatalog,
    pub env: Environment,
    pub page: PageDisplayList,
    pub scene: RetainedPreview,
    pub sites: Vec<PaintSite>,
    pub peak_nodes: usize,
    pub root_copies: usize,
    pub last_line_observed: bool,
    pub badness_observed: bool,
}
pub(crate) struct Early {
    pub scene: RetainedPreview,
    pub page: PageDisplayList,
    pub pass: usize,
    pub base_tag: crate::resources::effects::DocumentRevision,
    pub base_checkpoint: u64,
    pub validation_ms: f64,
    pub paint_ms: f64,
}
fn miss(reason: &str) -> Error {
    Error::value(format!("exact-local:{reason}"))
}
impl Ready {
    /// Read-only diagnostic projection, deliberately independent of admission.
    pub fn investigation(&self) -> serde_json::Value {
        serde_json::json!({"candidate":self.candidate,"origin":{"checkpoint":self.origin.id,"offset":self.origin.offset,"path":self.origin.path},
            "anchors":self.anchors.iter().map(|a|serde_json::json!({"id":a.id,"offset":a.offset,"fence":a.fence})).collect::<Vec<_>>(),
            "source_lines":self.source_lines,"end":self.end,"literal_end":self.literal_end,
            "shapes":self.shapes,"penalties":self.penalties,"font_runs":self.font_runs,
            "frontier_context":format!("{:?}",self.frontier_context),"tail_reads":format!("{:?}",self.tail_reads),
            "context":format!("{:?}",self.context),"badness":self.badness,"last_line":self.last_line,
            "warnings":self.warnings,"errors":self.errors})
    }
    pub fn compatible(&self, new: &Self) -> Result<()> {
        let old_source = &self.origin.source;
        let source = &new.origin.source;
        let anchor = self
            .anchors
            .iter()
            .find(|old| {
                new.anchors
                    .iter()
                    .any(|n| n.id == old.id && n.offset == old.offset && n.fence == old.fence)
            })
            .ok_or_else(|| miss("checkpoint_lineage"))?;
        if old_source.get(..anchor.offset) != source.get(..anchor.offset)
            || old_source.get(self.literal_end..) != source.get(new.literal_end..)
        {
            return Err(miss("outside_source"));
        }
        let prefix = old_source
            .iter()
            .zip(source.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = old_source[prefix..]
            .iter()
            .rev()
            .zip(source[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        if prefix == old_source.len() && prefix == source.len() {
            return Err(miss("unchanged_source"));
        }
        if prefix < anchor.offset || prefix >= self.literal_end || prefix >= new.literal_end {
            return Err(miss("edit_outside_parameter"));
        }
        let old_edit = &old_source[prefix..old_source.len() - suffix];
        let edit = &source[prefix..source.len() - suffix];
        if old_edit
            .iter()
            .chain(edit)
            .any(|b| !b.is_ascii_graphic() || *b == b'\\')
        {
            return Err(miss("nonliteral_edit_or_source_shift"));
        }
        let line = 1 + old_source[..prefix].iter().filter(|&&b| b == b'\n').count();
        if line < self.source_lines.0 || line > self.source_lines.1 {
            return Err(miss("edit_outside_parameter"));
        }
        if self.frontier_context != new.frontier_context || self.tail_reads != new.tail_reads {
            return Err(miss("paragraph_end_observations"));
        }
        if self.font_runs != new.font_runs {
            return Err(miss("font_use_sequence"));
        }
        if self.shapes != new.shapes || self.penalties != new.penalties {
            return Err(miss("vertical_observations"));
        }
        if self.context != new.context {
            return Err(miss("paragraph_context"));
        }
        if self.warnings != new.warnings || self.errors != new.errors {
            return Err(miss("paragraph_diagnostics"));
        }
        Ok(())
    }
}
struct Extract<'a> {
    fonts: &'a mut FontCatalog,
    precision: u32,
    dummy: Option<usize>,
}
impl ExtractionServices for Extract<'_> {
    fn fonts(&self) -> &FontCatalog {
        self.fonts
    }
    fn fonts_mut(&mut self) -> &mut FontCatalog {
        self.fonts
    }
    fn precision(&mut self) -> Result<u32> {
        Ok(self.precision)
    }
    fn dummy_space_font(&mut self) -> Result<usize> {
        self.dummy.ok_or_else(|| miss("new_extraction_resource"))
    }
}
impl Base {
    pub fn replace(
        &self,
        engine: &mut Engine,
        new: &Ready,
        now: &dyn Fn() -> u128,
    ) -> Result<Early> {
        let started = now();
        self.ready.compatible(new)?;
        if self.badness_observed && self.ready.badness != new.badness {
            return Err(miss("paragraph_badness_observation"));
        }
        if self.last_line_observed && self.ready.last_line != new.last_line {
            return Err(miss("paragraph_last_line_observation"));
        }
        if self.sites.iter().any(|site| {
            site.parameter.candidate == self.ready.candidate && site.page != self.page.page
        }) {
            return Err(miss("prior_paint_sink"));
        }
        if engine
            .core
            .local
            .evidence
            .borrow()
            .misses
            .contains_key(&new.candidate)
        {
            return Err(miss("recomputed_material_observation"));
        }
        if self.ready.current_font != engine.fonts.current
            || self.ready.fonts.len() != engine.fonts.items.len()
            || !self
                .ready
                .fonts
                .iter()
                .zip(&engine.fonts.items)
                .all(|(guard, font)| guard.matches(font))
        {
            return Err(miss("font_state"));
        }
        // Bound allocation failure under the changed number of pure records.
        // A generous worst case charges the full new paragraph at every copied
        // occurrence, including transient copies not eventually painted.
        let copy_count = self.root_copies + 1;
        if engine.builder.node_limit.is_some_and(|limit| {
            self.peak_nodes
                .saturating_add(new.nodes_count.saturating_mul(copy_count))
                >= limit
        }) {
            return Err(miss("node_capacity"));
        }
        if self.page.magnification != 1000 {
            return Err(miss("paint_mode"));
        }
        let validated = now();
        let mut fonts = self.fonts.detached_clone()?;
        let mut env = self.env.clone();
        env.reuse_effects.borrow_mut().take();
        env.reuse_sequence.borrow_mut().take();
        env.reuse_stateful.set(false);
        let mut backend = self.backend.clone();
        let mut page = self.page.clone();
        // Display lines are shipment batches, not paragraph lines. Merge their
        // already positioned immutable records for a page-wide operator splice.
        if page.lines.len() > 1 {
            let mut merged = page.lines[0].clone();
            for batch in &page.lines[1..] {
                let offset = merged.operators.len();
                merged.placements.extend(batch.placements.clone());
                merged.nodes.extend(batch.nodes.clone());
                merged.emitted.extend(batch.emitted.clone());
                merged.operators.extend(batch.operators.clone());
                for mut span in batch.local_spans.clone() {
                    span.start += offset;
                    span.end += offset;
                    merged.local_spans.push(span);
                }
            }
            page.lines = vec![merged];
        }
        let original_page = page.clone();
        let mut layout = Layout::default();
        let mut next_placement = page
            .lines
            .iter()
            .flat_map(|l| &l.placements)
            .map(|p| p.placement_id)
            .max()
            .unwrap_or(0)
            + 1;
        let mut next_node = page
            .lines
            .iter()
            .flat_map(|l| &l.nodes)
            .flat_map(|n| {
                [
                    n.node_id,
                    n.link,
                    n.child,
                    n.pre_break,
                    n.post_break,
                    n.leader_ptr,
                ]
            })
            .max()
            .unwrap_or(0)
            + 1;
        let mut replaced = 0;
        let mut scene_patches = Vec::new();
        for (line_index, line) in original_page.lines.iter().enumerate() {
            let spans: Vec<_> = line
                .local_spans
                .iter()
                .filter(|s| s.candidate == self.ready.candidate)
                .collect();
            if spans.is_empty() {
                continue;
            }
            if spans.len() % new.roots.len() != 0 {
                return Err(miss("partial_paragraph_on_page"));
            }
            let target = &mut page.lines[line_index];
            let mut op_replacements = Vec::new();
            let mut placement_replacements = Vec::new();
            let mut removed_nodes = BTreeSet::new();
            let mut added_nodes = Vec::new();
            let mut removed_placements = BTreeSet::new();
            let mut added_emitted = Vec::new();
            for group in spans.chunks(new.roots.len()) {
                if group.iter().enumerate().any(|(i, s)| s.line != i) {
                    return Err(miss("interleaved_or_split_paint"));
                }
                let sites: Vec<_> = group
                    .iter()
                    .map(|span| {
                        self.sites
                            .iter()
                            .find(|p| {
                                p.placement == span.placement
                                    && p.parameter.candidate == self.ready.candidate
                            })
                            .ok_or_else(|| miss("missing_extraction_boundary"))
                    })
                    .collect::<Result<_>>()?;
                let mut extraction = sites[0].entry.clone();
                let mut state = group[0].entry.clone();
                state.fragment = true;
                let mut operators = Vec::new();
                let mut run_start = group[0].start;
                for (i, span) in group.iter().enumerate() {
                    if i > 0 {
                        let previous = group[i - 1];
                        let gap = &line.operators[previous.end..span.start];
                        // Native neutral gaps may carry the parameterized
                        // cursor. Every other gap is opaque: it is retained
                        // only after both incoming states are proved identical.
                        let opaque = (!gap.is_empty() && gap != ["ET"])
                            || sites[i - 1].exit_epoch != sites[i].entry_epoch
                            || sites[i - 1].exit != sites[i].entry;
                        if opaque {
                            state.fragment = false;
                            if state != previous.exit || extraction != sites[i - 1].exit {
                                return Err(miss("opaque_paint_boundary"));
                            }
                            scene_patches.push((
                                line.operators[run_start..previous.end].to_vec(),
                                operators.clone(),
                            ));
                            op_replacements.push((
                                run_start,
                                previous.end,
                                std::mem::take(&mut operators),
                            ));
                            run_start = span.start;
                            state = span.entry.clone();
                            state.fragment = true;
                            extraction = sites[i].entry.clone();
                        } else if !gap.is_empty() {
                            state.fragment = false;
                            if state != previous.exit {
                                return Err(miss("batch_text_exit"));
                            }
                            let mut closed = state.clone();
                            closed.historical_text = None;
                            closed.local_text = Some((None, None, false));
                            if closed != span.entry {
                                return Err(miss("batch_text_transition"));
                            }
                            operators.push("ET".into());
                            state = closed;
                            state.fragment = true;
                        }
                    }
                    let start = line
                        .placements
                        .iter()
                        .position(|p| p.placement_id == span.placement)
                        .ok_or_else(|| miss("missing_placement"))?;
                    let old_root = &line.placements[start];
                    let end = start
                        + 1
                        + line.placements[start + 1..]
                            .iter()
                            .take_while(|p| {
                                p.parameter.is_some_and(|tag| {
                                    !tag.root
                                        && tag.candidate == self.ready.candidate
                                        && tag.line == i
                                })
                            })
                            .count();
                    if line.placements[start..end]
                        .iter()
                        .any(|p| p.shaped.is_some())
                    {
                        return Err(miss("virtual_or_shaped_paint"));
                    }
                    let mut warnings = Vec::new();
                    let mut fragment = layout.place_checked_line(
                        &mut TypesettingContext {
                            builder: &mut engine.builder,
                            fonts: &mut fonts,
                            env: &mut env,
                            warn: &mut |s| warnings.push(s.to_owned()),
                        },
                        new.roots[i],
                        old_root.x_sp,
                        old_root.baseline_y_sp,
                    )?;
                    if !warnings.is_empty() {
                        return Err(miss("placement_diagnostic"));
                    }
                    let mut extracted = Vec::new();
                    for p in std::mem::take(&mut fragment.placements) {
                        extracted.extend(extraction.capture(
                            &mut Extract {
                                fonts: &mut fonts,
                                precision: backend.document.decimal_digits.unwrap_or(9) as u32,
                                dummy: backend.document.space_font,
                            },
                            p,
                        )?);
                    }
                    let font_runs = |items: &[crate::typesetting::display::Placement]| {
                        let mut result = Vec::new();
                        for p in items {
                            if matches!(p.kind.as_str(), "glyph" | "extraction_space")
                                && result.last() != Some(&p.font)
                            {
                                result.push(p.font);
                            }
                        }
                        result
                    };
                    if font_runs(&extracted) != font_runs(&line.placements[start..end]) {
                        return Err(miss("paint_font_use_sequence"));
                    }
                    let mut placements = BTreeMap::new();
                    for p in &extracted {
                        placements.entry(p.placement_id).or_insert_with(|| {
                            let id = next_placement;
                            next_placement += 1;
                            id
                        });
                    }
                    // Synthetic extraction spaces have no children; give each an independent ID.
                    for p in &mut extracted {
                        let old = p.placement_id;
                        p.placement_id = if old == usize::MAX {
                            let id = next_placement;
                            next_placement += 1;
                            id
                        } else {
                            placements[&old]
                        };
                        p.parent_id = p.parent_id.and_then(|id| placements.get(&id).copied());
                    }
                    let mut node_ids = BTreeMap::new();
                    for n in &fragment.nodes {
                        let id = if n.node_id == new.roots[i] {
                            old_root.node_id
                        } else {
                            let id = next_node;
                            next_node += 1;
                            id
                        };
                        node_ids.insert(n.node_id, id);
                    }
                    for p in &mut extracted {
                        p.node_id = *node_ids.get(&p.node_id).unwrap_or(&C::NULL);
                        if p.kind == "hbox" && p.parent_id.is_none() {
                            p.parent_id = old_root.parent_id;
                        }
                        p.parameter = None;
                    }
                    for n in &mut fragment.nodes {
                        let old = n.node_id;
                        n.node_id = node_ids[&old];
                        n.child = *node_ids.get(&n.child).unwrap_or(&C::NULL);
                        n.link = if old == new.roots[i] {
                            line.nodes
                                .iter()
                                .find(|old| old.node_id == old_root.node_id)
                                .map_or(C::NULL, |old| old.link)
                        } else {
                            *node_ids.get(&n.link).unwrap_or(&C::NULL)
                        };
                    }
                    fragment.placements = extracted;
                    let (ops, emitted) = backend.render_line(
                        &fragment,
                        page.height_sp,
                        &mut state,
                        false,
                        &mut fonts,
                        &mut env,
                        &mut |s| warnings.push(s.to_owned()),
                    )?;
                    state.local_spans.clear();
                    if !warnings.is_empty() {
                        return Err(miss("paint_diagnostic"));
                    }
                    operators.extend(ops);
                    added_emitted.extend(emitted);
                    for p in &line.placements[start..end] {
                        removed_nodes.insert(p.node_id);
                        removed_placements.insert(p.placement_id);
                    }
                    added_nodes.extend(fragment.nodes);
                    placement_replacements.push((start, end, fragment.placements));
                }
                if extraction != sites.last().unwrap().exit {
                    return Err(miss("extraction_exit"));
                }
                state.fragment = false;
                if state != group.last().unwrap().exit {
                    return Err(miss("paint_exit"));
                }
                scene_patches.push((
                    line.operators[run_start..group.last().unwrap().end].to_vec(),
                    operators.clone(),
                ));
                op_replacements.push((run_start, group.last().unwrap().end, operators));
                replaced += 1;
            }
            for (start, end, ops) in op_replacements.into_iter().rev() {
                target.operators.splice(start..end, ops);
            }
            for (start, end, placements) in placement_replacements.into_iter().rev() {
                target.placements.splice(start..end, placements);
            }
            target
                .emitted
                .retain(|e| !removed_placements.contains(&e.placement_id));
            target.emitted.extend(added_emitted);
            let order: BTreeMap<_, _> = target
                .placements
                .iter()
                .enumerate()
                .map(|(i, p)| (p.placement_id, i))
                .collect();
            target
                .emitted
                .sort_by_key(|e| order.get(&e.placement_id).copied().unwrap_or(usize::MAX));
            target.nodes.retain(|n| !removed_nodes.contains(&n.node_id));
            target.nodes.extend(added_nodes);
            target.local_spans.clear();
        }
        if replaced == 0 {
            return Err(miss("parameter_not_painted"));
        }
        let mut scene = self.scene.clone();
        // A portable page never claims that this revision has settled.
        scene.complete = false;
        let mut operators = page
            .lines
            .iter()
            .flat_map(|l| l.operators.iter().cloned())
            .collect::<Vec<_>>();
        if !backend.resources.pages[self.page.page - 1]
            .xobjects
            .is_empty()
        {
            operators = scene.pages[0].operators.clone();
            for (old, new) in scene_patches {
                let old = crate::pdf::preview::instruction_chunks(&old)?;
                if old.is_empty() {
                    return Err(miss("empty_paint_mapping"));
                }
                let matches: Vec<_> = operators
                    .windows(old.len())
                    .enumerate()
                    .filter(|(_, run)| run.iter().zip(&old).all(|(a, b)| a.trim() == b.trim()))
                    .map(|(i, _)| i)
                    .collect();
                if matches.len() != 1 {
                    return Err(miss("ambiguous_expanded_paint_mapping"));
                }
                let start = matches[0];
                let mut new = crate::pdf::preview::instruction_chunks(&new)?;
                if let Some(first) = new.first_mut() {
                    let prefix: String = operators[start]
                        .chars()
                        .take_while(|c| c.is_ascii_whitespace())
                        .collect();
                    *first = format!("{prefix}{}", first.trim_start());
                }
                operators.splice(start..start + old.len(), new);
            }
        }
        scene.pages[0].content_sha256 =
            crate::pdf::preview::page_identity(&page, &operators, &scene.fonts)?;
        scene.pages[0].operators = operators;
        Ok(Early {
            scene,
            page,
            pass: self.pass,
            base_tag: self.tag.clone(),
            base_checkpoint: self.ready.origin.id,
            validation_ms: validated.saturating_sub(started) as f64 / 1_000_000.0,
            paint_ms: now().saturating_sub(validated) as f64 / 1_000_000.0,
        })
    }
}

/// Compare the portable early result with an ordinary observation without
/// treating revision-local graph/placement identities as content.
pub(crate) fn geometry(page: &PageDisplayList) -> Result<serde_json::Value> {
    let placements: Vec<_> = page.lines.iter().flat_map(|l| &l.placements).collect();
    let identities: BTreeMap<_, _> = placements
        .iter()
        .enumerate()
        .map(|(i, p)| (p.placement_id, i))
        .collect();
    let mut nodes = BTreeMap::new();
    for (i, p) in placements.iter().enumerate() {
        nodes.entry(p.node_id).or_insert(i);
    }
    let mut records = Vec::new();
    for p in placements {
        let mut record = serde_json::to_value(p).map_err(|e| miss(&e.to_string()))?;
        let map = record.as_object_mut().unwrap();
        map.remove("node_id");
        map.remove("placement_id");
        map.insert(
            "parent_id".into(),
            serde_json::json!(p.parent_id.and_then(|id| identities.get(&id).copied())),
        );
        records.push(record);
    }
    let mut packed = Vec::new();
    for n in page.lines.iter().flat_map(|l| &l.nodes) {
        let mut record = serde_json::to_value(n).map_err(|e| miss(&e.to_string()))?;
        let map = record.as_object_mut().unwrap();
        for (name, id) in [
            ("node_id", n.node_id),
            ("link", n.link),
            ("child", n.child),
            ("pre_break", n.pre_break),
            ("post_break", n.post_break),
            ("leader_ptr", n.leader_ptr),
        ] {
            map.insert(name.into(), serde_json::json!(nodes.get(&id)));
        }
        packed.push((nodes.get(&n.node_id).copied(), record));
    }
    packed.sort_by_key(|(id, _)| *id);
    let mut emitted = Vec::new();
    for e in page.lines.iter().flat_map(|l| &l.emitted) {
        let mut value = serde_json::to_value(e).map_err(|e| miss(&e.to_string()))?;
        value.as_object_mut().unwrap().insert(
            "placement_id".into(),
            serde_json::json!(identities.get(&e.placement_id)),
        );
        emitted.push(value);
    }
    Ok(
        serde_json::json!({"width":page.width_sp,"height":page.height_sp,"mag":page.magnification,"placements":records,"nodes":packed,"emitted":emitted}),
    )
}

pub(crate) fn difference(
    a: &serde_json::Value,
    b: &serde_json::Value,
    path: &str,
) -> Option<String> {
    if a == b {
        return None;
    }
    match (a, b) {
        (serde_json::Value::Array(a), serde_json::Value::Array(b)) if a.len() == b.len() => {
            for (i, (a, b)) in a.iter().zip(b).enumerate() {
                if let Some(d) = difference(a, b, &format!("{path}[{i}]")) {
                    return Some(d);
                }
            }
        }
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) if a.keys().eq(b.keys()) => {
            for (k, a) in a {
                if let Some(d) = difference(a, &b[k], &format!("{path}.{k}")) {
                    return Some(d);
                }
            }
        }
        _ => {}
    }
    Some(format!("{path}: ordinary={a} early={b}"))
}
