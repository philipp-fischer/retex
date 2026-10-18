//! Opt-in presentation only. Native ownership survives box copies; it is never
//! serialized into a format, consulted by TeX, or admitted as a reuse proof.
use crate::{
    Engine,
    core::{
        constants::Constants as C,
        enums::NodeKind,
        errors::{Error, Result},
    },
    engine::EngineView,
    pdf::{
        emission::PDFEmissionState,
        preview::{PreviewPage, RetainedPreview},
    },
    typesetting::{
        display::PageDisplayList,
        layout::{Layout, TypesettingContext},
    },
};
use serde::Serialize;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Region {
    pub occurrence: u64,
    pub key: String,
    pub file: String,
    pub first_line: usize,
    pub first_column: usize,
    pub last_line: usize,
    pub last_column: usize,
    pub lines: usize,
}
#[derive(Clone, Debug)]
pub(crate) struct Owner {
    pub region: Arc<Region>,
    pub line: usize,
    pub root: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct Span {
    pub owner: Owner,
    pub start: usize,
    pub end: usize,
    pub entry: PDFEmissionState,
    pub x: i64,
    pub baseline: i64,
    pub shift: i64,
}
pub(crate) struct Ready {
    pub region: Arc<Region>,
    pub roots: Vec<i64>,
    pub font_ids: Vec<usize>,
    pub layout_ms: f64,
    pub line_break_ms: f64,
}
#[derive(Default)]
pub(crate) struct State {
    pub pending: Option<Ready>,
    pub layout_ms: f64,
    pub line_break_ms: f64,
    pub reason: Option<String>,
}

impl EngineView<'_> {
    pub(crate) fn preview_paragraph(&mut self, roots: &[i64]) {
        if !self.core.config.paragraph_preview {
            return;
        }
        // Presentation-only validation: no TeX control flow consumes these reads.
        let _scope = self
            .builder
            .nodes
            .contract(crate::checked_material::Consumer::Validation);
        let result = (|| -> Result<()> {
            if roots.is_empty() || roots.len() > 128 {
                return Err(Error::value("empty_or_large_paragraph"));
            }
            let mut font_ids = std::collections::BTreeSet::new();
            let mut graph = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            let mut sources: BTreeMap<Arc<str>, ((usize, usize), (usize, usize))> = BTreeMap::new();
            for (line, &root) in roots.iter().enumerate() {
                if self.builder.node(root)?.node_type != NodeKind::Hlist {
                    return Err(Error::value("non_line_material"));
                }
                let mut todo = vec![(root, true, false)];
                while let Some((id, is_root, siblings)) = todo.pop() {
                    if id == C::NULL {
                        continue;
                    }
                    if !seen.insert(id) || seen.len() > 16384 {
                        return Err(Error::value("shared_or_large_material_graph"));
                    }
                    let node = self.builder.node(id)?;
                    if !crate::checked_material::pure(node) {
                        return Err(Error::value("effects_or_unsupported_material"));
                    }
                    if node.glyph() {
                        font_ids.insert(node.font);
                        for s in node.origins.iter().flatten().filter(|s| {
                            s.expansion.is_none()
                                && !s.call_site_truncated
                                && s.end_byte_column.is_some()
                        }) {
                            let first = (
                                s.line,
                                s.byte_column.map_or(s.column, std::num::NonZeroUsize::get),
                            );
                            let last = (s.line, s.end_byte_column.unwrap().get() - 1);
                            sources
                                .entry(s.file.clone())
                                .and_modify(|(a, b)| {
                                    *a = (*a).min(first);
                                    *b = (*b).max(last);
                                })
                                .or_insert((first, last));
                        }
                    }
                    graph.push((id, line, is_root));
                    if siblings {
                        todo.push((node.link, false, true));
                    }
                    todo.push((node.list_ptr, false, true));
                }
            }
            if sources.len() != 1 {
                return Err(Error::value("ambiguous_physical_source"));
            }
            let (file, (first, last)) = sources.into_iter().next().unwrap();
            let occurrence = self
                .core
                .input
                .source_occurrences
                .get(file.as_ref())
                .copied()
                .unwrap_or(0);
            let region = Arc::new(Region {
                occurrence,
                key: format!(
                    "{file}:{occurrence}:{}:{}:{}:{}",
                    first.0, first.1, last.0, last.1
                ),
                file: file.to_string(),
                first_line: first.0,
                first_column: first.1,
                last_line: last.0,
                last_column: last.1,
                lines: roots.len(),
            });
            for (id, line, root) in graph {
                self.builder.node_mut(id)?.preview_owner = Some(Owner {
                    region: region.clone(),
                    line,
                    root,
                });
            }
            self.core.paragraph_preview.pending = Some(Ready {
                region,
                roots: roots.to_vec(),
                font_ids: font_ids.into_iter().collect(),
                layout_ms: self.core.paragraph_preview.layout_ms,
                line_break_ms: self.core.paragraph_preview.line_break_ms,
            });
            Ok(())
        })();
        if let Err(e) = result {
            self.core.paragraph_preview.reason = Some(e.message);
        }
    }
}

#[derive(Clone, Serialize)]
pub struct RegionMap {
    pub id: String,
    pub file: String,
    pub source_lines: [usize; 2],
    pub ranges: Vec<[usize; 2]>,
    #[serde(skip)]
    pub(crate) region: Option<Arc<Region>>,
    #[serde(skip)]
    pub(crate) spans: Vec<Span>,
}
pub(crate) struct Base {
    pub document: String,
    pub revision: u64,
    pub pass: usize,
    pub sources: BTreeMap<String, Arc<[u8]>>,
    pub page: PreviewPage,
    pub regions: Vec<RegionMap>,
}
#[derive(Serialize)]
pub struct Fragment {
    pub source_bounds: [usize; 2],
    pub line_count: usize,
    pub base_line_count: usize,
    pub base_pass: usize,
    pub page: usize,
    pub pass: usize,
    pub base_revision: u64,
    pub base_hash: String,
    pub region: String,
    pub prefix: Vec<String>,
    pub close_text: bool,
    pub restore_count: usize,
    pub preview: RetainedPreview,
    pub timings: serde_json::Value,
}
fn fail(reason: &str) -> Error {
    Error::value(format!("paragraph-preview:{reason}"))
}
fn offset(source: &[u8], line: usize, column: usize) -> Option<usize> {
    let start = if line == 1 {
        0
    } else {
        source
            .iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .nth(line.checked_sub(2)?)
            .map(|(i, _)| i + 1)?
    };
    let end = source[start..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(source.len(), |n| start + n);
    let pos = start + column.saturating_sub(1);
    (pos <= end).then_some(pos)
}
fn bounds(source: &[u8], r: &Region) -> Option<(usize, usize)> {
    Some((
        offset(source, r.first_line, r.first_column)?,
        offset(source, r.last_line, r.last_column)?.saturating_add(1),
    ))
}
fn difference(a: &[u8], b: &[u8]) -> (usize, usize, usize) {
    let prefix = a.iter().zip(b).take_while(|(a, b)| a == b).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (prefix, a.len() - suffix, b.len() - suffix)
}
impl Base {
    pub fn capture(
        document: String,
        revision: u64,
        pass: usize,
        sources: BTreeMap<String, Arc<[u8]>>,
        prepared: &PageDisplayList,
        scene: &mut RetainedPreview,
    ) -> Result<Self> {
        let page = &mut scene.pages[0];
        page.operators = crate::pdf::preview::instruction_chunks(&page.operators)?;
        let mut groups: BTreeMap<String, Vec<(Span, Vec<String>)>> = BTreeMap::new();
        for line in &prepared.lines {
            for span in &line.preview_spans {
                if span.start >= span.end {
                    continue;
                }
                groups
                    .entry(span.owner.region.key.clone())
                    .or_default()
                    .push((
                        span.clone(),
                        crate::pdf::preview::instruction_chunks(
                            &line.operators[span.start..span.end],
                        )?,
                    ));
            }
        }
        // An output routine may insert material into an owned line or unpack
        // it. A resumed owned run without its root is not a complete interval.
        let mut interrupted = std::collections::BTreeSet::new();
        let mut ended = std::collections::BTreeSet::new();
        let mut active = None;
        for placement in prepared.lines.iter().flat_map(|l| &l.placements) {
            let owner = placement.preview_owner.as_ref();
            let next = owner.map(|o| (o.region.key.clone(), o.line));
            if next != active {
                if let Some(old) = active.take() {
                    ended.insert(old);
                }
                if let Some(key) = &next {
                    if ended.contains(key) {
                        interrupted.insert(key.0.clone());
                    }
                }
                active = next;
            }
        }
        let mut regions = Vec::new();
        for (id, group) in groups {
            let region = group[0].0.owner.region.clone();
            if interrupted.contains(&id)
                || group.len() != region.lines
                || group
                    .iter()
                    .enumerate()
                    .any(|(i, (s, _))| s.owner.line != i)
            {
                continue;
            }
            let mut ranges = Vec::new();
            let mut valid = true;
            for (_, ops) in &group {
                if ops.is_empty() {
                    valid = false;
                    break;
                }
                let matches: Vec<_> = page
                    .operators
                    .windows(ops.len())
                    .enumerate()
                    .filter(|(_, a)| a.iter().zip(ops).all(|(a, b)| a.trim() == b.trim()))
                    .map(|(i, _)| i)
                    .collect();
                if matches.len() != 1 {
                    valid = false;
                    break;
                }
                ranges.push([matches[0], matches[0] + ops.len()]);
            }
            if !valid || ranges.windows(2).any(|r| r[0][1] > r[1][0]) {
                continue;
            }
            // Split columns, external layout between lines and opaque graphics
            // gaps have no single prior paragraph placement for this prototype.
            if group.windows(2).any(|g| {
                g[0].0.baseline >= g[1].0.baseline
                    || g[0].0.x - g[0].0.shift != g[1].0.x - g[1].0.shift
            }) || ranges.windows(2).any(|r| {
                page.operators[r[0][1]..r[1][0]]
                    .iter()
                    .any(|op| op.trim() != "ET")
            }) {
                continue;
            }
            // Nested form/marked content or an unfinished path at the boundary
            // is outside this bounded fragment contract. Ordinary output remains.
            let prefix = &page.operators[..ranges[0][0]];
            if prefix.iter().any(|s| {
                matches!(
                    s.split_whitespace().last(),
                    Some("retexFormBegin" | "BMC" | "BDC")
                )
            }) {
                continue;
            }
            regions.push(RegionMap {
                id,
                file: region.file.clone(),
                source_lines: [region.first_line, region.last_line],
                ranges,
                region: Some(region),
                spans: group.into_iter().map(|(s, _)| s).collect(),
            });
        }
        page.provisional_regions = Some(regions.clone());
        Ok(Self {
            document,
            revision,
            pass,
            sources,
            page: page.clone(),
            regions,
        })
    }
    pub fn mapping_evidence(&self, new: &Ready, source: &[u8]) -> serde_json::Value {
        let previous = self.sources.get(&new.region.file);
        serde_json::json!({"base_revision":self.revision,"base_pass":self.pass,"new":new.region,"change":previous.map(|old|difference(old,source)),"new_bounds":bounds(source,&new.region)})
    }
    pub fn matching(&self, new: &Ready, source: &[u8], edit_anchor: usize) -> Option<&RegionMap> {
        let previous = self.sources.get(&new.region.file)?;
        let (mut start, mut old_end, mut new_end) = difference(previous, source);
        if start == previous.len() && start == source.len() {
            start = edit_anchor;
            old_end = start;
            new_end = start;
        }
        let (a, b) = bounds(source, &new.region)?;
        if start < a || new_end > b {
            return None;
        }
        let matches: Vec<_> = self
            .regions
            .iter()
            .filter(|m| {
                let r = m.region.as_ref().unwrap();
                r.file == new.region.file
                    && r.occurrence == new.region.occurrence
                    && bounds(previous, r).is_some_and(|(a, b)| start >= a && old_end <= b)
            })
            .collect();
        (matches.len() == 1).then(|| matches[0])
    }
    pub fn render(
        &self,
        m: &RegionMap,
        new: &Ready,
        source: &[u8],
        engine: &mut Engine,
        cache: &mut crate::fonts::outlines::OutlineCache,
        images: &mut crate::pdf::preview_images::ImageCache,
    ) -> Result<Fragment> {
        let timed = engine.core.config.paragraph_preview_timings;
        let now = || {
            if timed {
                crate::local_investigation::now()
            } else {
                0
            }
        };
        let started = now();
        if self.page.magnification != 1000 {
            return Err(fail("magnification"));
        }
        let _scope = engine
            .builder
            .nodes
            .contract(crate::checked_material::Consumer::Paint);
        let mut fonts = engine.fonts.detached_clone()?;
        let mut env = engine.env.clone();
        let mut backend = engine.core.backend.clone();
        env.reuse_stateful.set(false);
        env.reuse_paragraph_level.set(None);
        env.reuse_font_construction_allowed.set(false);
        env.reuse_physical_input.set(None);
        env.reuse_input_lookups.borrow_mut().clear();
        env.reuse_effects.borrow_mut().take();
        env.reuse_sequence.borrow_mut().take();
        env.reuse_expansions.set(None);
        let cloned = now();
        // Normal current font binding must precede placement: a logical font
        // may expand to physical virtual-font packets only when bound. Never
        // borrow an older page's incomplete subset or mutate the live catalog.
        let suppress =
            crate::typesetting::nodes::parameter(&env, "pdfsuppresswarningdupmap", 0) > 0;
        let mut font_warnings = Vec::new();
        for &id in &new.font_ids {
            fonts.bind(id, suppress, &mut |s| font_warnings.push(s.to_owned()))?;
            if let Some(vf) = fonts.items[id].virtual_font.clone() {
                for &physical in vf.fonts.values() {
                    fonts.materialize(physical, suppress, &mut |s| {
                        font_warnings.push(s.to_owned())
                    })?;
                }
            } else {
                fonts.materialize(id, suppress, &mut |s| font_warnings.push(s.to_owned()))?;
            }
        }
        if !font_warnings.is_empty() {
            return Err(fail("font_binding_diagnostic"));
        }
        let bound = now();
        let first = &m.spans[0];
        let mut layout = Layout::default();
        let mut lines = Vec::new();
        let mut baseline = first.baseline;
        let mut previous_depth = 0;
        let base_skip = crate::typesetting::nodes::glue_parameter(&env, "baselineskip").width;
        let line_skip = crate::typesetting::nodes::glue_parameter(&env, "lineskip").width;
        let limit = crate::typesetting::nodes::parameter(&env, "lineskiplimit", 0);
        let mut state = first.entry.clone();
        state.local_spans.clear();
        state.preview_spans.clear();
        state.local_text = None;
        state.historical_text = None;
        state.fragment = false;
        let height = crate::core::units::bp_to_sp(self.page.height_bp);
        for (i, &root) in new.roots.iter().enumerate() {
            let node = engine.builder.node(root)?;
            let (h, d, shift) = (node.height, node.depth, node.shift);
            if i > 0 {
                let gap = base_skip - previous_depth - h;
                baseline += previous_depth + h + if gap < limit { line_skip } else { gap };
            }
            previous_depth = d;
            let mut warnings = Vec::new();
            let mut line = layout.place_checked_line(
                &mut TypesettingContext {
                    builder: &mut engine.builder,
                    fonts: &mut fonts,
                    env: &mut env,
                    warn: &mut |s| warnings.push(s.to_owned()),
                },
                root,
                first.x - first.shift + shift,
                baseline,
            )?;
            let (operators, emitted) = backend.render_line(
                &line,
                height,
                &mut state,
                true,
                &mut fonts,
                &mut env,
                &mut |s| warnings.push(s.to_owned()),
            )?;
            if !warnings.is_empty() {
                return Err(fail("native_paint_diagnostic"));
            }
            line.source_geometry = std::mem::take(&mut state.source_geometry);
            line.operators = operators;
            line.emitted = emitted;
            lines.push(line);
        }
        let painted = now();
        backend.materialize_fonts(&mut fonts, &env, &mut |_| {})?;
        let page = PageDisplayList {
            page: self.page.page,
            width_sp: crate::core::units::bp_to_sp(self.page.width_bp),
            height_sp: height,
            lines,
            magnification: 1000,
        };
        let preview =
            crate::pdf::preview::capture_prepared(&backend, &fonts, &page, cache, images)?;
        let prepared = now();
        let prefix = self.page.operators[..m.ranges[0][0]].to_vec();
        let mut restore_count = 0usize;
        let mut close_text = false;
        for op in &prefix {
            match op.split_whitespace().last() {
                Some("q") => restore_count += 1,
                Some("Q") => restore_count = restore_count.saturating_sub(1),
                Some("BT") => close_text = true,
                Some("ET") => close_text = false,
                _ => {}
            }
        }
        let (start, end) = bounds(source, &new.region).ok_or_else(|| fail("source_bounds"))?;
        Ok(Fragment {
            source_bounds: [start, end],
            line_count: new.roots.len(),
            base_line_count: m.spans.len(),
            base_pass: self.pass,
            page: self.page.page,
            pass: 1,
            base_revision: self.revision,
            base_hash: self.page.content_sha256.clone(),
            region: m.id.clone(),
            prefix,
            close_text,
            restore_count,
            preview,
            timings: serde_json::json!({"paragraph_layout_and_cache_ms":new.layout_ms,"line_break_ms":new.line_break_ms,"observer_clone_ms":(cloned-started) as f64/1e6,"font_bind_ms":(bound-cloned) as f64/1e6,"native_placement_paint_ms":(painted-bound) as f64/1e6,"resource_outline_prepare_ms":(prepared-painted) as f64/1e6,"fragment_total_ms":(prepared-started) as f64/1e6}),
        })
    }
}
