//! Complete classic paragraph packing, attached to a validated preparation hit.
//! No arbitrary macro or output-routine state is cached here.
use super::*;
const LAYOUT_BYTES: usize = 32 * 1024 * 1024;
const LAYOUT_ENTRIES: usize = 64;

// Strong ownership is held only between a preparation hit and layout entry.
// Stored memos retain weak handles, so eviction still releases preparation.
pub(super) enum PreparationKey {
    ReadOnly(Arc<Entry>),
    Stateful(Arc<grouped::GroupEntry>),
    ReadOnlyWeak(std::sync::Weak<Entry>),
    StatefulWeak(std::sync::Weak<grouped::GroupEntry>),
}
impl PreparationKey {
    fn weak(&self) -> Self {
        match self {
            Self::ReadOnly(e) => Self::ReadOnlyWeak(Arc::downgrade(e)),
            Self::Stateful(e) => Self::StatefulWeak(Arc::downgrade(e)),
            _ => unreachable!(),
        }
    }
    fn matches(&self, candidate: &Self) -> bool {
        match (self, candidate) {
            (Self::ReadOnlyWeak(a), Self::ReadOnly(b)) => std::ptr::eq(a.as_ptr(), Arc::as_ptr(b)),
            (Self::StatefulWeak(a), Self::Stateful(b)) => std::ptr::eq(a.as_ptr(), Arc::as_ptr(b)),
            _ => false,
        }
    }
    fn stateful(&self) -> bool {
        matches!(self, Self::Stateful(_) | Self::StatefulWeak(_))
    }
}
fn material(builder: &NodeBuilder, roots: &[i64], stateful: bool) -> Option<Graph> {
    let graph = Graph::capture_nodes(&builder.nodes, roots, stateful)?;
    // Layout carries deferred payloads to ordinary shipment; it does not execute
    // them. Image IDs need an explicit mapping contract and remain unsupported.
    if graph.nodes.iter().any(|n| {
        matches!(
            n.payload,
            NodePayload::Pdf(crate::typesetting::model::PdfEffect::RefImage(_))
        )
    }) {
        return None;
    }
    Some(graph)
}
pub(super) struct LayoutCapture {
    key: PreparationKey,
    input: Graph,
    guard: LayoutGuard,
    warnings: usize,
}
pub(super) struct LayoutMemo {
    key: PreparationKey,
    input: Graph,
    guard: LayoutGuard,
    values: BTreeMap<ValueKey, Option<Quantity>>,
    output: Graph,
    penalties: Vec<i64>,
    hsize: i64,
    badness: i64,
    ready: bool,
    warnings: Vec<String>,
    bytes: usize,
}
struct LayoutGuard {
    profile: crate::core::characters::LanguageProfile,
    current_font: usize,
    fonts: Vec<(usize, FontGuard)>,
    initial: (i64, i64, i64),
    language: i64,
    previous: i64,
    display: bool,
    hsize: i64,
    greedy: bool,
    exact: bool,
    hyphen: HyphenGuard,
}
impl LayoutGuard {
    fn new(view: &EngineView<'_>, input: &Graph, previous: i64, display: bool) -> Option<Self> {
        if view.core.trace.enabled
            || view.core.trace.observer.is_some()
            || view.core.diagnostics.debug
            || view.core.diagnostics.debug_nodes
            || InputProcessor::parameter(view.env, "tracingparagraphs", 0) != 0
            || InputProcessor::parameter(view.env, "pdfadjustspacing", 0) != 0
        {
            return None;
        }
        let mut numbers = std::collections::BTreeSet::from([view.fonts.current]);
        for node in &input.nodes {
            if node.glyph() || node.node_type == crate::core::enums::NodeKind::Glue {
                numbers.insert(node.font);
            }
        }
        let mut fonts = Vec::new();
        for number in numbers {
            let font = view.fonts.items.get(number)?;
            if font.expansion_policy.is_some()
                || font.expansion_base.is_some()
                || font.unicode_font.is_some()
            {
                return None;
            }
            fonts.push((number, FontGuard::new(font)));
        }
        Some(Self {
            profile: view.core.config.language_profile,
            current_font: view.fonts.current,
            fonts,
            initial: view.builder.context.initial_hyphenation,
            language: view.builder.context.current_language,
            previous,
            display,
            hsize: view.layout.hsize,
            greedy: view.layout.greedy_paragraphs,
            exact: view.layout.exact_repro,
            hyphen: HyphenGuard {
                ready: view.hyphenation.ready,
                tries: view.hyphenation.tries.weak_storage(),
                exceptions: view.hyphenation.exceptions.weak_storage(),
                codes: view.hyphenation.saved_codes.weak_storage(),
            },
        })
    }
    fn matches(&self, view: &EngineView<'_>, previous: i64, display: bool) -> bool {
        self.profile == view.core.config.language_profile
            && self.current_font == view.fonts.current
            && self.previous == previous
            && self.display == display
            && self.hsize == view.layout.hsize
            && self.greedy == view.layout.greedy_paragraphs
            && self.exact == view.layout.exact_repro
            && self.initial == view.builder.context.initial_hyphenation
            && self.language == view.builder.context.current_language
            && self.fonts.iter().all(|(n, f)| {
                view.fonts
                    .items
                    .get(*n)
                    .is_some_and(|current| f.matches(current))
            })
            && self.hyphen.ready == view.hyphenation.ready
            && self.hyphen.tries.as_ptr() as usize == view.hyphenation.tries.storage_address()
            && self.hyphen.exceptions.as_ptr() as usize
                == view.hyphenation.exceptions.storage_address()
            && self.hyphen.codes.as_ptr() as usize == view.hyphenation.saved_codes.storage_address()
    }
}
impl Graph {
    pub(super) fn same_input(&self, other: &Self) -> bool {
        self.roots == other.roots
            && self.nodes.len() == other.nodes.len()
            && self.nodes.iter().zip(&other.nodes).all(|(a, b)| {
                edges(a) == edges(b)
                    && (a.shares_contents_with(b)
                        || match (bincode::serialize(a), bincode::serialize(b)) {
                            (Ok(a), Ok(b)) => a == b,
                            _ => false,
                        })
            })
    }
}
impl EngineView<'_> {
    pub(in crate::engine) fn begin_layout_reuse(
        &mut self,
        head: i64,
        previous: i64,
        display: bool,
    ) -> Result<Option<(Vec<i64>, Vec<i64>)>> {
        let Some(candidate) = self.core.reuse.layout_candidate.take() else {
            return Ok(None);
        };
        if self.core.reuse.active.is_some() || head == C::NULL {
            return Ok(None);
        }
        let Some(input) = material(self.builder, &[head], candidate.stateful()) else {
            return Ok(None);
        };
        let Some(guard) = LayoutGuard::new(self, &input, previous, display) else {
            return Ok(None);
        };
        #[cfg(feature = "latency-diagnostics")]
        let _validate =
            crate::latency_diagnostics::phase("layout_reuse_validate", "paragraph", None);
        let records: Vec<_> = self
            .core
            .reuse
            .cache
            .borrow()
            .layouts
            .iter()
            .filter(|entry| entry.key.matches(&candidate))
            .cloned()
            .collect();
        for memo in records {
            if !memo.guard.matches(self, previous, display)
                || !memo.input.same_input(&input)
                || memo
                    .values
                    .iter()
                    .any(|(key, value)| self.env.values.get(key) != value.as_ref())
            {
                continue;
            }
            let imported = memo.output.materialize_nodes(self.builder, "", 0, 0, 0)?;
            let boxes = memo.output.roots.iter().map(|&r| imported[r - 1]).collect();
            self.builder.release(head, true)?;
            self.layout.hsize = memo.hsize;
            self.layout.set_last_badness(memo.badness);
            self.hyphenation.ready = memo.ready;
            for warning in &memo.warnings {
                self.warning(warning);
            }
            self.core.reuse.stats.layout_hits += 1;
            self.core.reuse.stats.avoided_layouts += 1;
            let mut cache = self.core.reuse.cache.borrow_mut();
            if let Some(index) = cache
                .layouts
                .iter()
                .position(|entry| Arc::ptr_eq(entry, &memo))
            {
                let entry = cache.layouts.remove(index).unwrap();
                cache.layouts.push_back(entry);
            }
            return Ok(Some((boxes, memo.penalties.clone())));
        }
        // Capture layout only after preparation itself has hit. A new edited
        // paragraph should not pay to serialize a layout that may never recur.
        self.env.values.begin_reads();
        self.core.reuse.layout_capture = Some(LayoutCapture {
            key: candidate.weak(),
            input,
            guard,
            warnings: self.core.diagnostics.warnings.len(),
        });
        Ok(None)
    }
    pub(in crate::engine) fn finish_layout_reuse(&mut self, boxes: &[i64], penalties: &[i64]) {
        let Some(capture) = self.core.reuse.layout_capture.take() else {
            return;
        };
        let Some(values) = self.env.values.finish_reads() else {
            return;
        };
        let Some(output) = material(self.builder, boxes, capture.key.stateful()) else {
            return;
        };
        #[cfg(feature = "latency-diagnostics")]
        let _record = crate::latency_diagnostics::phase("layout_reuse_capture", "paragraph", None);
        let mut bytes = bincode::serialized_size(&values).unwrap_or(u64::MAX);
        for node in capture.input.nodes.iter().chain(&output.nodes) {
            bytes = bytes.saturating_add(bincode::serialized_size(node).unwrap_or(u64::MAX));
        }
        for (_, font) in &capture.guard.fonts {
            bytes = bytes.saturating_add(bincode::serialized_size(&font.font).unwrap_or(u64::MAX));
        }
        let bytes = usize::try_from(bytes)
            .unwrap_or(usize::MAX)
            .saturating_mul(4)
            .saturating_add(capture.input.bytes())
            .saturating_add(output.bytes())
            .saturating_add(4096);
        if bytes > LAYOUT_BYTES {
            return;
        }
        let memo = LayoutMemo {
            key: capture.key,
            input: capture.input,
            guard: capture.guard,
            values,
            output,
            penalties: penalties.to_vec(),
            hsize: self.layout.hsize,
            badness: self.layout.last_badness(),
            ready: self.hyphenation.ready,
            warnings: self.core.diagnostics.warnings[capture.warnings..].to_vec(),
            bytes,
        };
        let mut cache = self.core.reuse.cache.borrow_mut();
        while cache.layouts.len() >= LAYOUT_ENTRIES || cache.layout_bytes + bytes > LAYOUT_BYTES {
            let old = cache.layouts.pop_front().unwrap();
            cache.layout_bytes -= old.bytes;
        }
        cache.layout_bytes += bytes;
        cache.layouts.push_back(Arc::new(memo));
        drop(cache);
        self.core
            .reuse
            .rebalance(self.core.config.stateful_execution_reuse);
    }
}
