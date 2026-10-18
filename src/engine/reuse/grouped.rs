//! Conservative balanced horizontal construction groups. The ordinary command
//! dispatcher executes misses. Only an explicit operation whitelist can publish
//! an entry; table escape, unsupported effects or an unbalanced exit rejects it.
use super::*;
use crate::{
    language::{
        groups::RestorationKey,
        state::{ReuseEffectJournal, ReuseStateEffect},
    },
    resources::virtual_host::{ResourceCheckpoint, ResourceReadScope, VirtualHost},
};
use std::rc::Weak;
mod input;
mod pdf;
use input::Boundary;
const BUDGET: usize = 16 * 1024 * 1024;
const LIMIT: usize = 16;
#[derive(Default)]
struct GroupCache {
    entries: VecDeque<Arc<GroupEntry>>,
    bytes: usize,
    generation: u64,
    negative: VecDeque<(AdmissionKey, u8)>,
    negative_bytes: usize,
    paragraph_hints: VecDeque<(String, Vec<u8>)>,
}
#[derive(Clone)]
struct AdmissionKey {
    tokens: Arc<[Token]>,
    index: usize,
}
impl AdmissionKey {
    fn read(input: &InputProcessor) -> Option<Self> {
        let tokens = input.cursor.token_list.as_ref()?;
        if tokens.len() > 1024 {
            return None;
        }
        Some(Self {
            tokens: tokens.clone(),
            index: input.cursor.token_index,
        })
    }
    fn same(&self, other: &Self) -> bool {
        self.index == other.index && Arc::ptr_eq(&self.tokens, &other.tokens)
    }
    fn bytes(&self) -> usize {
        self.tokens.len() * std::mem::size_of::<Token>() + 128
    }
}
impl GroupCache {
    // Negative admission never advances input or reuses a result. An identity
    // match only executes normally without recording, then retries after eight
    // observations. Changed definitions get new token-array identities.
    fn reject_admission(&mut self, key: Option<AdmissionKey>) {
        let Some(key) = key else { return };
        if self.negative.iter().any(|(old, _)| old.same(&key)) {
            return;
        }
        while self.negative.len() >= 32 || self.negative_bytes + key.bytes() > 128 * 1024 {
            let Some((old, _)) = self.negative.pop_front() else {
                return;
            };
            self.negative_bytes -= old.bytes();
        }
        self.negative_bytes += key.bytes();
        self.negative.push_back((key, 8));
    }
    fn skip_recording(&mut self, key: &Option<AdmissionKey>) -> bool {
        let Some(key) = key else { return false };
        if self
            .entries
            .iter()
            .any(|e| e.admission.as_ref().is_some_and(|k| k.same(key)))
        {
            return false;
        }
        let Some(index) = self.negative.iter().position(|(k, _)| k.same(key)) else {
            return false;
        };
        let (old, remaining) = self.negative.remove(index).unwrap();
        if remaining == 0 {
            self.negative_bytes -= old.bytes();
            return false;
        }
        self.negative.push_back((old, remaining - 1));
        true
    }
}
#[derive(Default)]
pub(crate) struct GroupReuse {
    cache: Rc<RefCell<GroupCache>>,
    pub host: Weak<VirtualHost>,
    active: Option<GroupCapture>,
    generation_started: bool,
    pub hits: u64,
    pub misses: u64,
    pub admission_skips: u64,
    pub macros: u64,
    pub nodes: u64,
    pub effects: u64,
    pub tokens: u64,
    pub rejected: BTreeMap<String, u64>,
    #[cfg(feature = "latency-diagnostics")]
    pub measurements: VecDeque<RegionMeasurement>,
    #[cfg(feature = "latency-diagnostics")]
    pub measurement_dropped: u64,
}
#[cfg(feature = "latency-diagnostics")]
#[derive(Clone, Debug, serde::Serialize)]
pub struct RegionMeasurement {
    file: String,
    first_line: usize,
    paragraph: bool,
    outcome: String,
    active_ns: u64,
    calls: u64,
    max_ns: u64,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum RegionKind {
    Group,
    Paragraph,
}
impl GroupReuse {
    #[cfg(feature = "latency-diagnostics")]
    pub(super) fn retained_regions(
        &self,
    ) -> Vec<(String, usize, bool, usize, u64, usize, usize, usize)> {
        self.cache
            .borrow()
            .entries
            .iter()
            .map(|e| {
                (
                    e.guard.input.path.clone(),
                    e.guard.input.origin_line,
                    e.kind == RegionKind::Paragraph,
                    e.bytes,
                    e.macros,
                    e.retained_charges.0,
                    e.retained_charges.1,
                    e.retained_charges.2,
                )
            })
            .collect()
    }

    #[cfg(all(feature = "latency-diagnostics", target_arch = "wasm32"))]
    pub(super) fn clear_material(&mut self) {
        let mut cache = self.cache.borrow_mut();
        cache.entries.clear();
        cache.bytes = 0;
    }

    #[cfg(feature = "latency-diagnostics")]
    fn measure(&mut self, capture: &GroupCapture, outcome: &str) {
        let Some(start) = capture.started else {
            return;
        };
        let Some(end) = crate::latency_diagnostics::execution_clock() else {
            return;
        };
        let elapsed = end.saturating_sub(start);
        if let Some(row) = self.measurements.iter_mut().find(|r| {
            r.file == capture.guard.input.path
                && r.first_line == capture.guard.input.origin_line
                && r.paragraph == (capture.kind == RegionKind::Paragraph)
                && r.outcome == outcome
        }) {
            row.active_ns += elapsed;
            row.calls += 1;
            row.max_ns = row.max_ns.max(elapsed);
            return;
        }
        if self.measurements.len() == 64 {
            self.measurement_dropped += 1;
            return;
        }
        self.measurements.push_back(RegionMeasurement {
            file: capture.guard.input.path.clone(),
            first_line: capture.guard.input.origin_line,
            paragraph: capture.kind == RegionKind::Paragraph,
            outcome: outcome.into(),
            active_ns: elapsed,
            calls: 1,
            max_ns: elapsed,
        });
    }

    pub(super) fn note_paragraph_rejection(&self, path: &str, source: &[u8]) {
        let line = &source[..source
            .iter()
            .position(|&b| b == b'\n')
            .unwrap_or(source.len())];
        if path.len() > 1024 || line.len() > 1024 {
            return;
        }
        let mut cache = self.cache.borrow_mut();
        if cache
            .paragraph_hints
            .iter()
            .any(|(p, l)| p == path && l == line)
        {
            return;
        }
        if cache.paragraph_hints.len() == 32 {
            cache.paragraph_hints.pop_front();
        }
        cache
            .paragraph_hints
            .push_back((path.to_owned(), line.to_vec()));
    }
    pub(super) fn paragraph_hint(&self, path: &str, source: &[u8]) -> bool {
        let line = &source[..source
            .iter()
            .position(|&b| b == b'\n')
            .unwrap_or(source.len())];
        self.cache
            .borrow()
            .paragraph_hints
            .iter()
            .any(|(p, l)| p == path && l == line)
    }

    pub(super) fn trim(&self, budget: usize) {
        let mut cache = self.cache.borrow_mut();
        while cache.bytes > budget {
            let old = cache.entries.pop_front().unwrap();
            cache.bytes -= old.bytes;
        }
    }

    pub fn checkpoint_clone(&self) -> Self {
        Self {
            cache: self.cache.clone(),
            host: self.host.clone(),
            ..Default::default()
        }
    }
    pub fn retained_bytes(&self) -> usize {
        let cache = self.cache.borrow();
        cache.bytes
            + cache.negative_bytes
            + cache
                .paragraph_hints
                .iter()
                .map(|(p, l)| p.len() + l.len() + 128)
                .sum::<usize>()
    }
    pub fn recording(&self) -> bool {
        self.active.is_some()
    }
    pub fn cancel(&mut self, env: &Environment) {
        if self.active.take().is_some() {
            Self::clear_observers(env);
        }
    }
    pub fn entries(&self) -> usize {
        self.cache.borrow().entries.len()
    }
    fn reject(&mut self, env: &Environment, reason: &str) {
        if let Some(capture) = self.active.take() {
            #[cfg(feature = "latency-diagnostics")]
            self.measure(&capture, reason);
            self.cache
                .borrow_mut()
                .reject_admission(capture.admission.clone());
            if self.rejected.len() < 64 {
                #[cfg(feature = "latency-diagnostics")]
                let reason = if reason == "state-read" {
                    if let Some((file, line)) = env.reuse_rejection_site.get() {
                        format!("state-read:{file}:{line}")
                    } else {
                        reason.to_string()
                    }
                } else {
                    reason.to_string()
                };
                let reason = format!("{reason}@{}", capture.guard.input.origin_line);
                *self.rejected.entry(reason).or_default() += 1;
            }
        }
        Self::clear_observers(env);
    }
    fn clear_observers(env: &Environment) {
        env.reuse_stateful.set(false);
        env.reuse_paragraph_level.set(None);
        env.reuse_font_construction_allowed.set(false);
        env.reuse_physical_input.set(None);
        env.reuse_input_lookups.borrow_mut().clear();
        env.reuse_effects.borrow_mut().take();
        env.reuse_sequence.borrow_mut().take();
        env.reuse_font_names.borrow_mut().clear();
        env.reuse_font_writes.borrow_mut().clear();
        env.reuse_font_symbol_inputs.borrow_mut().clear();
        env.reuse_font_symbol_outputs.borrow_mut().clear();
        env.reuse_outer_prev.borrow_mut().clear();
        env.reuse_expansions.set(None);
        env.reuse_symbols.borrow_mut().clear();
        env.values.cancel_reads();
        env.meanings.cancel_reads();
        env.box_registers.cancel_reads();
    }
}
struct GroupCapture {
    #[cfg(feature = "latency-diagnostics")]
    started: Option<u64>,
    kind: RegionKind,
    initial_groups: Vec<crate::language::groups::GroupFrame>,
    admission: Option<AdmissionKey>,
    guard: GroupGuard,
    old_nodes: crate::typesetting::nodes::NodeTable,
    value_levels: crate::core::persistence::SharedTable<RestorationKey, usize>,
    incoming_conditions: Vec<Rc<RefCell<crate::language::conditionals::ConditionalFrame>>>,
    incoming_alignment: Option<Rc<RefCell<crate::typesetting::alignments::EntryHandler>>>,
    scope: Rc<ResourceReadScope>,
    font_scope: Option<Rc<crate::fonts::construction::Scope>>,
    bytes: Arc<[u8]>,
    token_start: usize,
    expansion_start: usize,
    warnings: usize,
    prefix: Option<(i64, i64, Node)>,
    prefix_read: bool,
    paragraphs: usize,
    packs: usize,
    diagnostics: (usize, usize, usize),
    pdf_guard: pdf::PdfGuard,
    pdf_events: Vec<(u64, pdf::Recipe)>,
}
// This witness describes only the suspended cell-body interceptor. Cell packing,
// template execution and the parent alignment machine remain outside the segment.
// Entries retain a value snapshot, never a prior execution's mutable handler.
#[derive(Clone, PartialEq, Eq)]
struct CellGuard {
    depth: usize,
    handler: crate::typesetting::alignments::EntryHandler,
}
fn cell_guard(view: &EngineView<'_>) -> Option<CellGuard> {
    use crate::typesetting::alignments::EntryPhase;
    if !view.core.config.alignment_material_reuse {
        return None;
    }
    // Most command boundaries have no active cell. Do not walk the grouping
    // stack in that common case (including ordinary output construction).
    let handler = view.core.input.alignment_handler.as_ref()?.borrow();
    if handler.phase != EntryPhase::Body
        || handler.boundary.is_some()
        || handler.suffix.len() > 8192
    {
        return None;
    }
    if !(view.env.cur_group == crate::core::enums::GroupKind::Alignment
        || view
            .env
            .groups
            .iter()
            .any(|g| g.previous_group == crate::core::enums::GroupKind::Alignment))
    {
        return None;
    }
    Some(CellGuard {
        depth: view.core.alignment_depth,
        handler: handler.clone(),
    })
}

fn same_handler(
    before: &Option<Rc<RefCell<crate::typesetting::alignments::EntryHandler>>>,
    after: &Option<Rc<RefCell<crate::typesetting::alignments::EntryHandler>>>,
) -> bool {
    match (before, after) {
        (None, None) => true,
        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
        _ => false,
    }
}
struct GroupGuard {
    cell: Option<CellGuard>,
    interaction: u8,
    eight_bit_output: bool,
    input: Boundary,
    context: ListContext,
    stack: Vec<ListContext>,
    level: usize,
    group: crate::core::enums::GroupKind,
    frames: Vec<(String, crate::core::enums::GroupKind, usize, usize)>,
    profile: crate::core::characters::LanguageProfile,
    catcodes: Vec<Cat>,
    par_loc: i64,
    par_token: Token,
    frozen: std::sync::Weak<BTreeMap<i64, i64>>,
    fonts: BTreeMap<usize, FontGuard>,
    current_font: usize,
    font_count: usize,
    hsize: i64,
    greedy: bool,
    exact: bool,
    hyphen: HyphenGuard,
    pdf: pdf::PdfGuard,
    next_symbol: i64,
    stream_eof: BTreeMap<i64, bool>,
    writers: Vec<u8>,
    pages: usize,
    conditions: Vec<(usize, bool, bool, bool)>,
}
pub(super) struct GroupEntry {
    kind: RegionKind,
    created_generation: u64,
    reused: std::cell::Cell<bool>,
    writes: crate::resources::reuse_writes::Journal,
    admission: Option<AdmissionKey>,
    guard: GroupGuard,
    source: Vec<u8>,
    end: Boundary,
    context: ListContext,
    graph: Graph,
    values: BTreeMap<ValueKey, Option<Quantity>>,
    meanings: BTreeMap<Arc<str>, Option<Arc<Equivalence>>>,
    levels: BTreeMap<RestorationKey, usize>,
    boxes: BTreeMap<i64, Option<Graph>>,
    symbols: BTreeMap<i64, String>,
    interned: Vec<(i64, String)>,
    symbol_keys: Vec<(i64, String)>,
    resources: ResourceCheckpoint,
    console: Vec<(bool, Vec<u8>)>,
    pdf: Vec<(u64, pdf::Recipe)>,
    input_bindings: Vec<std::path::PathBuf>,
    uses_input: bool,
    source_resources: Vec<(String, String)>,
    input_lookups: Vec<crate::language::state::ReuseInputLookup>,
    tokens: usize,
    expansions: usize,
    macros: u64,
    warnings: Vec<String>,
    hsize: i64,
    badness: i64,
    paragraphs: usize,
    packs: usize,
    last_line: Option<i64>,
    end_eof: BTreeMap<i64, bool>,
    bytes: usize,
    #[cfg(feature = "latency-diagnostics")]
    retained_charges: (usize, usize, usize),
    line_sensitive: bool,
    outer_prev: BTreeMap<usize, i64>,
    effects: Vec<(u64, ReuseStateEffect)>,
    console_order: Vec<u64>,
    font_names: BTreeMap<i64, Option<String>>,
    font_inputs: BTreeMap<i64, String>,
    constructed_fonts: crate::fonts::construction::Recipe,
    produced_font_symbols: BTreeMap<i64, usize>,
    prefix_tail: Option<(Node, Node)>,
}
fn rebased_node(node: &Node, path: &str, first: usize, last: usize, delta: isize) -> Node {
    let mut result = node.clone();
    if delta != 0 && !node.origins.is_empty() {
        result.origins = node
            .origins
            .iter()
            .map(|s| s.as_ref().map(|s| rebase(s, path, first, last, delta)))
            .collect();
    }
    result
}
fn same_contents(a: &Node, b: &Node) -> bool {
    if a.shares_contents_with(b) {
        return true;
    }
    let (mut a, mut b) = (a.clone(), b.clone());
    set_edges(&mut a, [C::NULL; 5]);
    set_edges(&mut b, [C::NULL; 5]);
    bincode::serialize(&a)
        .ok()
        .zip(bincode::serialize(&b).ok())
        .is_some_and(|(a, b)| a == b)
}
fn group_context(mut c: ListContext) -> ListContext {
    c.cur_list_head = C::NULL;
    c.cur_list_tail = C::NULL;
    c.cur_q = C::NULL;
    c
}
fn frames(env: &Environment) -> Vec<(String, crate::core::enums::GroupKind, usize, usize)> {
    env.groups
        .iter()
        .map(|f| {
            (
                f.kind.clone(),
                f.previous_group,
                f.entries.len(),
                f.after_tokens.len(),
            )
        })
        .collect()
}
fn conditions(view: &EngineView<'_>) -> Vec<(usize, bool, bool, bool)> {
    view.core
        .conditionals
        .stack
        .iter()
        .map(|f| {
            let f = f.borrow();
            (f.code, f.unless, f.evaluating, f.alternative)
        })
        .collect()
}
fn compatible_font(font: &crate::fonts::model::FontInfo) -> bool {
    font.unicode_font.is_none() && font.expansion_policy.is_none() && font.expansion_base.is_none()
}
impl GroupGuard {
    fn eligible(view: &EngineView<'_>) -> bool {
        use crate::core::enums::GroupKind as G;
        #[cfg(feature = "latency-diagnostics")]
        if crate::output_diagnostics::enabled() {
            return false;
        }
        let cell = cell_guard(view);
        if InputProcessor::parameter(view.env, "tracingparagraphs", 0) != 0 {
            return false;
        }
        if view.core.trace.enabled
            || view.core.trace.observer.is_some()
            || view.core.diagnostics.debug
            || view.core.diagnostics.debug_tokens
            || view.core.diagnostics.debug_nodes
            || view.core.diagnostics.debug_commands
            || view.core.config.expansion_limit.is_some()
            || view.core.expansion.expansion_depth_limit.is_some()
            || view.core.expansion.csname_depth != 0
            || view.builder.capacities != Default::default()
            || view.env.capacities != Default::default()
            || view.fonts.capacities != Default::default()
            || view.core.input.capacities != Default::default()
            || (view.core.config.input_execution_reuse
                && (view.core.input.last_input_error.is_some()
                    || view.core.input.last_missing_resource.is_some()))
            || view.core.input.profile != crate::core::characters::LanguageProfile::Pdftex
            || view.builder.context.mode != ListMode::Horizontal
            || view.builder.context.cur_q != C::NULL
            || !view.builder.context.unicode_run.is_empty()
            || view.builder.context.lig_stack.len() > 64
            || (view.core.alignment_depth != 0 && cell.is_none())
            || conditions(view).iter().any(|f| f.2)
            || !view.core.streams.readers.is_empty()
            || view.core.assignments.after_assignment.is_some()
            || view.core.box_after_assignment.is_some()
            || !view.core.setbox_allowed
            || view
                .fonts
                .items
                .get(view.fonts.current)
                .is_none_or(|f| !compatible_font(f))
            || [G::Output, G::MathShift, G::MathField].contains(&view.env.cur_group)
            || (view.env.cur_group == G::Alignment && cell.is_none())
            || view.env.groups.iter().any(|f| {
                [G::Output, G::MathField].contains(&f.previous_group)
                    || ([G::MathShift, G::Alignment].contains(&f.previous_group) && cell.is_none())
            })
            || (view.core.interaction != 0
                && crate::diagnostics::messages::TEX_TRACE_CONTROLS
                    .iter()
                    .any(|(_, name)| InputProcessor::parameter(view.env, name, 0) != 0))
        {
            return false;
        }
        true
    }
    fn new(view: &EngineView<'_>, input: Boundary) -> Option<Self> {
        if input.bytes()? > 1024 * 1024 {
            return None;
        }
        let c = &view.builder.context;
        if c.cur_list_head != C::NULL
            && (c.cur_list_tail == C::NULL
                || view.builder.node(c.cur_list_tail).ok()?.link != C::NULL)
        {
            return None;
        }
        Some(Self {
            cell: cell_guard(view),
            interaction: view.core.interaction,
            eight_bit_output: view.core.config.eight_bit_output,
            input,
            context: group_context(c.clone()),
            stack: view
                .builder
                .context_stack
                .iter()
                .cloned()
                .map(group_context)
                .collect(),
            level: view.env.cur_level,
            group: view.env.cur_group,
            frames: frames(view.env),
            profile: view.core.input.profile,
            catcodes: view.core.input.cat_code.clone(),
            par_loc: view.core.input.par_loc,
            par_token: view.core.input.par_token,
            frozen: view.env.frozen_primitives.weak_storage(),
            fonts: BTreeMap::from([(
                view.fonts.current,
                FontGuard::new(&view.fonts.items[view.fonts.current]),
            )]),
            current_font: view.fonts.current,
            font_count: view.fonts.items.len(),
            hsize: view.layout.hsize,
            greedy: view.layout.greedy_paragraphs,
            exact: view.layout.exact_repro,
            hyphen: HyphenGuard {
                ready: view.hyphenation.ready,
                tries: view.hyphenation.tries.weak_storage(),
                exceptions: view.hyphenation.exceptions.weak_storage(),
                codes: view.hyphenation.saved_codes.weak_storage(),
            },
            pdf: pdf::PdfGuard::new(
                &view.core.backend,
                view.core.config.material_region_admission,
            ),
            next_symbol: view.env.next_symbol,
            stream_eof: view.core.stream_eof.clone(),
            writers: view.core.streams.writers.keys().copied().collect(),
            pages: view.layout.pages.len(),
            conditions: conditions(view),
        })
    }
    fn common(&self, view: &EngineView<'_>, delta: isize) -> bool {
        #[cfg(feature = "latency-diagnostics")]
        let _guard = crate::latency_diagnostics::phase(
            "group_common_guard",
            "state/font/PDF witnesses",
            None,
        );
        let c = view.builder.context.clone();
        let old = rebase_context(
            self.context.clone(),
            &self.input.path,
            self.input.origin_line,
            self.input.line,
            delta,
        );
        self.cell == cell_guard(view)
            && self.interaction == view.core.interaction
            && self.eight_bit_output == view.core.config.eight_bit_output
            && group_context(c) == old
            && self.stack.len() == view.builder.context_stack.len()
            && self
                .stack
                .iter()
                .zip(&view.builder.context_stack)
                .all(|(a, b)| a.mode == b.mode && a.restricted == b.restricted)
            && self.level == view.env.cur_level
            && self.group == view.env.cur_group
            && self.frames.len() == view.env.groups.len()
            && self
                .frames
                .iter()
                .zip(&view.env.groups)
                .all(|(a, b)| a.0 == b.kind && a.1 == b.previous_group)
            && self.profile == view.core.input.profile
            && self.catcodes == view.core.input.cat_code
            && self.par_loc == view.core.input.par_loc
            && self.par_token == view.core.input.par_token
            && self.frozen.as_ptr() as usize == view.env.frozen_primitives.storage_address()
            && self.current_font == view.fonts.current
            && self.font_count == view.fonts.items.len()
            && self.fonts.iter().all(|(n, f)| {
                view.fonts
                    .items
                    .get(*n)
                    .is_some_and(|current| f.matches(current))
            })
            && self.hsize == view.layout.hsize
            && self.greedy == view.layout.greedy_paragraphs
            && self.exact == view.layout.exact_repro
            && self.hyphen.ready == view.hyphenation.ready
            && self.hyphen.tries.as_ptr() as usize == view.hyphenation.tries.storage_address()
            && self.hyphen.exceptions.as_ptr() as usize
                == view.hyphenation.exceptions.storage_address()
            && self.hyphen.codes.as_ptr() as usize == view.hyphenation.saved_codes.storage_address()
            && self.conditions == conditions(view)
            && view.core.streams.readers.is_empty()
            && self.stream_eof == view.core.stream_eof
            && self.writers
                == view
                    .core
                    .streams
                    .writers
                    .keys()
                    .copied()
                    .collect::<Vec<_>>()
            && self.pdf.matches(&view.core.backend)
            && view.core.assignments.after_assignment.is_none()
            && view.core.box_after_assignment.is_none()
            && view.core.setbox_allowed
    }
}
fn map_context(mut c: ListContext, mut map: impl FnMut(i64) -> Option<i64>) -> Option<ListContext> {
    c.cur_list_head = map(c.cur_list_head)?;
    c.cur_list_tail = map(c.cur_list_tail)?;
    c.cur_q = map(c.cur_q)?;
    Some(c)
}
fn equivalence_rebased(
    old: &Arc<Equivalence>,
    path: &str,
    start: usize,
    end: usize,
    delta: isize,
) -> Arc<Equivalence> {
    if delta == 0 {
        return old.clone();
    }
    let mut value = (**old).clone();
    if let CommandOperand::Macro(definition) = &mut value.value {
        let definition = Arc::make_mut(definition);
        definition.source = definition
            .source
            .as_ref()
            .map(|s| rebase(s, path, start, end, delta));
        definition.body_sources = definition.body_sources.as_ref().map(|list| {
            list.iter()
                .map(|s| s.as_ref().map(|s| rebase(s, path, start, end, delta)))
                .collect::<Vec<_>>()
                .into()
        });
    }
    Arc::new(value)
}
impl EngineView<'_> {
    pub(in crate::engine) fn try_grouped_reuse(&mut self) -> Result<bool> {
        self.try_region_reuse(RegionKind::Group)
    }
    pub(super) fn try_stateful_paragraph_reuse(&mut self) -> Result<bool> {
        self.try_region_reuse(RegionKind::Paragraph)
    }
    fn try_region_reuse(&mut self, kind: RegionKind) -> Result<bool> {
        if !self.core.config.execution_reuse
            || !self.core.config.grouped_execution_reuse
            || self.core.reuse.grouped.active.is_some()
            || (kind == RegionKind::Paragraph && self.core.input.alignment_handler.is_some())
        {
            return Ok(false);
        }
        let current = &self.core.input.current;
        if kind == RegionKind::Group
            && !current.is_category(Cat::LeftBrace)
            && !current.is_command(Command::BeginGroup)
        {
            return Ok(false);
        }
        #[cfg(feature = "latency-diagnostics")]
        let _boundary = crate::latency_diagnostics::phase(
            "group_reuse_boundary",
            "admission, validation, recording setup",
            None,
        );
        let Some(host) = self.core.reuse.grouped.host.upgrade() else {
            return Ok(false);
        };
        if !GroupGuard::eligible(self) || !host.owns_writers(&self.core.streams.writers) {
            return Ok(false);
        }
        let admission = (kind == RegionKind::Group)
            .then(|| AdmissionKey::read(&self.core.input))
            .flatten();
        if self
            .core
            .reuse
            .grouped
            .cache
            .borrow_mut()
            .skip_recording(&admission)
        {
            self.core.reuse.grouped.admission_skips += 1;
            return Ok(false);
        }
        let Some(input) = Boundary::read(self) else {
            return Ok(false);
        };
        if self.core.config.material_region_admission && !self.core.reuse.grouped.generation_started
        {
            let mut cache = self.core.reuse.grouped.cache.borrow_mut();
            cache.generation = cache.generation.saturating_add(1);
            let generation = cache.generation;
            cache.entries.retain(|entry| {
                entry.reused.get() || generation.saturating_sub(entry.created_generation) <= 1
            });
            cache.bytes = cache.entries.iter().map(|e| e.bytes).sum();
            drop(cache);
            self.core.reuse.grouped.generation_started = true;
        }
        self.core.reuse.reject(self.env);
        let bytes = self.core.input.input_file[0]
            .as_ref()
            .and_then(|s| s.catalog_bytes())
            .unwrap();
        let candidates = self
            .core
            .reuse
            .grouped
            .cache
            .borrow()
            .entries
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for entry in candidates {
            if entry.kind != kind {
                continue;
            }
            let Some(delta) = entry.guard.input.matches(&input) else {
                continue;
            };

            let dependencies_invalid = {
                #[cfg(feature = "latency-diagnostics")]
                let _guard = crate::latency_diagnostics::phase(
                    "group_dependency_validation",
                    "definitions, values and entry state",
                    None,
                );
                (!self.core.config.input_execution_reuse && entry.uses_input)
                    || (!self.core.config.stateful_execution_reuse
                        && !entry.writes.events.is_empty())
                    || !entry.guard.common(self, delta)
                    || entry.outer_prev.iter().any(|(index, value)| {
                        self.builder
                            .context_stack
                            .get(*index)
                            .is_none_or(|c| c.prev_graf != *value)
                    })
                    || {
                        let mut next = self.env.next_symbol;
                        !entry.interned.iter().all(|(id, name)| {
                            match self.env.symbol_ids.get(name) {
                                Some(actual) => actual == id,
                                None if *id == next => {
                                    next += 1;
                                    true
                                }
                                None => false,
                            }
                        })
                    }
                    || ((!entry.console.is_empty() || entry.line_sensitive) && delta != 0)
                    || bytes.get(input.position..input.position + entry.source.len())
                        != Some(entry.source.as_slice())
                    || !entry.constructed_fonts.matches(self.fonts)
                    || entry
                        .font_inputs
                        .iter()
                        .any(|(&id, key)| self.env.symbol_key(id, false) != *key)
                    || entry.produced_font_symbols.iter().any(|(&id, &number)| {
                        self.env
                            .font_symbols
                            .get(&number)
                            .is_some_and(|old| *old != id)
                    })
                    || entry.symbols.iter().any(|(&id, name)| {
                        if let Some(&number) = entry.produced_font_symbols.get(&id) {
                            name != &format!("font:{number}")
                        } else if let Some((_, spelling)) =
                            entry.interned.iter().find(|(cs, _)| *cs == id)
                        {
                            name != &format!("control-word:{spelling}")
                        } else {
                            self.env.symbol_key(id, false) != *name
                        }
                    })
                    || entry
                        .values
                        .iter()
                        .any(|(key, value)| self.env.values.get(key) != value.as_ref())
                    || entry.meanings.iter().any(|(key, value)| {
                        !meaning_matches(self.env.meanings.get(key.as_ref()), value.as_ref())
                    })
                    || entry
                        .font_names
                        .iter()
                        .any(|(&id, name)| self.env.font_identifier(id) != name.as_ref())
                    || entry.levels.iter().any(|(key, level)| {
                        self.env
                            .value_levels
                            .get(key)
                            .copied()
                            .unwrap_or(C::LEVEL_ONE)
                            != *level
                    })
                    || entry.prefix_tail.as_ref().is_some_and(|(before, _)| {
                        let old = rebased_node(
                            before,
                            &entry.guard.input.path,
                            entry.guard.input.origin_line,
                            entry.end.line,
                            delta,
                        );
                        self.builder
                            .node(self.builder.context.cur_list_tail)
                            .ok()
                            .is_none_or(|current| {
                                !same_contents(current, &old)
                                    || edges(current)[1..].iter().any(|&id| id != C::NULL)
                            })
                    })
                    || !host.can_replay_console(&entry.console)
                    || !host.can_replay_input_bindings(&entry.input_bindings)
            };
            if dependencies_invalid {
                continue;
            }
            let mut boxes_valid = true;
            for (&register, expected) in &entry.boxes {
                let pointer = self
                    .env
                    .box_registers
                    .get(&register)
                    .copied()
                    .unwrap_or(C::NULL);
                let actual = if pointer == C::NULL {
                    None
                } else {
                    Graph::capture_nodes(&self.builder.nodes, &[pointer], true)
                };
                if match (expected.as_ref(), actual.as_ref()) {
                    (None, None) => pointer != C::NULL,
                    (Some(a), Some(b)) => !a.same_input(b),
                    _ => true,
                } {
                    boxes_valid = false;
                    break;
                }
            }
            let Some(writer_keys) = entry.writes.bind(&self.core.streams.writers) else {
                continue;
            };
            if !boxes_valid
                || !host.can_replay_writes(&entry.writes)
                || !host.replay_resource_reads(&entry.resources)
            {
                continue;
            }
            // Re-run the bounded resolution computation with today's policy,
            // precedence and directory caches. Recorded byte reads alone cannot
            // prove that a previously unqueried higher-priority root is absent.
            if entry.input_lookups.iter().any(|lookup| {
                self.core
                    .input
                    .resources
                    .resolve_path(
                        &lookup.filename,
                        Some(&lookup.directory),
                        ".tex",
                        false,
                        false,
                    )
                    .ok()
                    .flatten()
                    .as_ref()
                    != Some(&lookup.resolved)
            }) {
                continue;
            }
            #[cfg(feature = "latency-diagnostics")]
            let _replay = crate::latency_diagnostics::phase(
                "group_reuse_import_replay",
                "balanced construction",
                None,
            );
            // All potentially failing validation and arena allocation precedes
            // namespace/state replay. Imported nodes have independent ownership.
            let imported = {
                #[cfg(feature = "latency-diagnostics")]
                let _material = crate::latency_diagnostics::phase(
                    "group_material_reconstruction",
                    "portable nodes and source rebasing",
                    None,
                );
                entry.graph.materialize_nodes(
                    self.builder,
                    &entry.guard.input.path,
                    entry.guard.input.origin_line,
                    entry.end.line,
                    delta,
                )?
            };
            let old_head = self.builder.context.cur_list_head;
            let old_tail = self.builder.context.cur_list_tail;
            let context = rebase_context(
                entry.context.clone(),
                &entry.guard.input.path,
                entry.guard.input.origin_line,
                entry.end.line,
                delta,
            );
            let context = map_context(context, |id| match id {
                C::NULL => Some(C::NULL),
                -2 => Some(old_head),
                -3 => Some(old_tail),
                n => imported.get(usize::try_from(n - 1).ok()?).copied(),
            })
            .ok_or_else(|| Error::runtime("Invalid grouped material context"))?;
            for (expected, name) in &entry.interned {
                let actual = self.env.intern(name, true);
                debug_assert_eq!(actual, *expected);
            }
            for (id, key) in &entry.symbol_keys {
                self.env.symbol_keys.insert(*id, key.clone());
            }
            host.replay_input_bindings(&entry.input_bindings)
                .map_err(Error::from)?;
            // These assignments are attribution metadata, not TeX-visible file
            // effects. Resource observations and current input bindings have
            // already been validated/replayed; no old stream handles are kept.
            self.core
                .input
                .source_resources
                .extend(entry.source_resources.iter().cloned());
            let mut image_map = self
                .core
                .backend
                .document
                .images
                .keys()
                .map(|&id| (id, id))
                .collect::<BTreeMap<_, _>>();
            let mut events = entry
                .effects
                .iter()
                .map(|(n, e)| (*n, Some(e), None, None, None, None))
                .chain(
                    entry
                        .console_order
                        .iter()
                        .copied()
                        .zip(&entry.console)
                        .map(|(n, c)| (n, None, Some(c), None, None, None)),
                )
                .collect::<Vec<_>>();
            events.extend(
                entry
                    .pdf
                    .iter()
                    .map(|(n, p)| (*n, None, None, Some(p), None, None)),
            );
            events.extend(
                entry
                    .constructed_fonts
                    .loads
                    .iter()
                    .map(|load| (load.order, None, None, None, Some(load), None)),
            );
            events.extend(entry.writes.events.iter().map(|(order, index, bytes)| {
                (*order, None, None, None, None, Some((*index, bytes)))
            }));
            events.sort_by_key(|(n, _, _, _, _, _)| *n);
            {
                #[cfg(feature = "latency-diagnostics")]
                let _effects = crate::latency_diagnostics::phase(
                    "group_effect_replay",
                    "ordered assignments, fonts, PDF and writes",
                    None,
                );
                for (_, effect, console, pdf, font_load, write) in events {
                    if let Some((index, bytes)) = write {
                        use std::io::Write;
                        let writer = self
                            .core
                            .streams
                            .writers
                            .get_mut(&writer_keys[index])
                            .ok_or_else(|| Error::runtime("Missing validated replay writer"))?;
                        writer.write_all(bytes)?;
                        writer.flush()?;
                    }
                    if let Some(load) = font_load {
                        entry.constructed_fonts.apply(load, self.fonts);
                    }
                    if let Some(pdf) = pdf {
                        image_map.extend(pdf.apply(&mut self.core.backend));
                    }
                    if let Some(effect) = effect {
                        match effect {
                            ReuseStateEffect::FontSymbol { number, id } => {
                                self.env.font_symbols.insert(*number, *id);
                                self.env.symbol_keys.insert(*id, format!("font:{number}"));
                            }
                            ReuseStateEffect::FontName { id, name } => {
                                self.env.font_identifiers.insert(*id, name.clone());
                            }
                            ReuseStateEffect::Value(key, value, global) => {
                                self.env.assign_value(key.clone(), value.clone(), *global)
                            }
                            ReuseStateEffect::Meaning(key, command, value, global) => {
                                let old = Arc::new(Equivalence {
                                    command: *command,
                                    value: value.clone(),
                                    level: C::LEVEL_ONE,
                                });
                                let value = equivalence_rebased(
                                    &old,
                                    &entry.guard.input.path,
                                    entry.guard.input.origin_line,
                                    entry.end.line,
                                    delta,
                                );
                                self.env.define_key(
                                    key.clone(),
                                    value.command,
                                    value.value.clone(),
                                    *global,
                                );
                            }
                        }
                    }
                    if let Some((error, text)) = console {
                        use crate::resources::host::Host;
                        host.console(*error, text).map_err(Error::from)?;
                    }
                }
            }
            for &id in &imported {
                if let NodePayload::Pdf(crate::typesetting::model::PdfEffect::RefImage(old)) =
                    &self.builder.node(id)?.payload
                {
                    let actual = *image_map
                        .get(old)
                        .ok_or_else(|| Error::runtime("Unowned reused PDF image"))?;
                    self.builder.node_mut(id)?.payload =
                        NodePayload::Pdf(crate::typesetting::model::PdfEffect::RefImage(actual));
                }
            }
            if let Some((_, after)) = &entry.prefix_tail {
                let mut after = rebased_node(
                    after,
                    &entry.guard.input.path,
                    entry.guard.input.origin_line,
                    entry.end.line,
                    delta,
                );
                set_edges(&mut after, edges(self.builder.node(old_tail)?));
                *self.builder.node_mut(old_tail)? = after;
            }
            if old_tail != C::NULL {
                self.builder.node_mut(old_tail)?.link = entry
                    .graph
                    .roots
                    .first()
                    .map_or(C::NULL, |&r| imported[r - 1]);
            }
            self.builder.context = context;
            if entry.packs != 0 {
                self.layout.set_last_badness(entry.badness);
                self.layout.pack_serial = self.layout.pack_serial.wrapping_add(entry.packs);
            }
            if entry.paragraphs != 0 {
                self.layout.hsize = entry.hsize;
                self.layout.paragraph_count += entry.paragraphs;
                self.math.set_last_line(entry.last_line);
            }
            self.core.stream_eof = entry.end_eof.clone();
            self.core
                .diagnostics
                .warnings
                .extend(entry.warnings.iter().cloned());
            entry.end.restore(
                self,
                entry.guard.input.origin_line,
                entry.end.line,
                delta,
                input.position + entry.source.len(),
            )?;
            self.core.input.token_count += entry.tokens;
            self.core.expansion.expansion_steps += entry.expansions;
            let reuse = &mut self.core.reuse;
            reuse.grouped.hits += 1;
            reuse.grouped.macros += entry.macros;
            reuse.grouped.nodes += imported.len() as u64;
            reuse.grouped.tokens += entry.tokens as u64;
            reuse.grouped.effects +=
                (entry.effects.len() + entry.console.len() + entry.writes.events.len()) as u64;
            reuse.stats.contains_reused_graphs = true;
            if reuse.stats.recent_regions.len() == 8 {
                reuse.stats.recent_regions.remove(0);
            }
            reuse.stats.recent_regions.push(ReusedRegion {
                file: entry.guard.input.path.clone(),
                first_line: entry
                    .guard
                    .input
                    .origin_line
                    .checked_add_signed(delta)
                    .unwrap(),
                last_line: entry.end.line.checked_add_signed(delta).unwrap(),
                macros: entry.macros,
                tokens: entry.tokens,
                nodes: entry.graph.nodes.len(),
                prepared: kind == RegionKind::Paragraph,
                grouped: true,
            });
            entry.reused.set(true);
            let mut cache = reuse.grouped.cache.borrow_mut();
            if let Some(index) = cache.entries.iter().position(|e| Arc::ptr_eq(e, &entry)) {
                let entry = cache.entries.remove(index).unwrap();
                cache.entries.push_back(entry);
            }
            drop(cache);
            if kind == RegionKind::Paragraph {
                self.core.reuse.layout_candidate =
                    Some(layout::PreparationKey::Stateful(entry.clone()));
            }
            return Ok(true);
        }
        if self
            .env
            .groups
            .iter()
            .map(|g| g.entries.len() + g.after_tokens.len())
            .sum::<usize>()
            > 1024
        {
            return Ok(false);
        }
        let Some(guard) = GroupGuard::new(self, input) else {
            return Ok(false);
        };
        let Some(scope) = host.begin_resource_reads() else {
            return Ok(false);
        };
        let context = &self.builder.context;
        let prefix = if context.cur_list_head == C::NULL {
            None
        } else {
            Some((
                context.cur_list_head,
                context.cur_list_tail,
                self.builder.node(context.cur_list_tail)?.clone(),
            ))
        };
        let font_scope =
            self.core.config.font_construction_reuse.then(|| {
                crate::fonts::construction::Scope::new(guard.font_count, scope.sequence())
            });
        self.fonts.construction = font_scope.as_ref().map(Rc::downgrade).unwrap_or_default();
        if self.core.config.stateful_execution_reuse {
            scope.allow_existing_writes();
            for (&key, writer) in &self.core.streams.writers {
                if !scope.register_writer(key, writer.as_ref()) {
                    return Ok(false);
                }
            }
        }
        let capture = GroupCapture {
            #[cfg(feature = "latency-diagnostics")]
            started: crate::latency_diagnostics::execution_clock(),
            kind,
            initial_groups: if kind == RegionKind::Paragraph {
                self.env.groups.clone()
            } else {
                Vec::new()
            },
            admission,
            font_scope,
            pdf_guard: guard.pdf.clone(),
            pdf_events: Vec::new(),
            diagnostics: (
                self.core.diagnostics.errors.len(),
                self.core.diagnostics.records.len(),
                self.core.diagnostics.help.len(),
            ),
            guard,
            old_nodes: self.builder.nodes.memo_snapshot(),
            value_levels: self.env.value_levels.clone(),
            incoming_conditions: self.core.conditionals.stack.clone(),
            incoming_alignment: self.core.input.alignment_handler.clone(),
            scope,
            bytes,
            token_start: self.core.input.token_count,
            expansion_start: self.core.expansion.expansion_steps,
            warnings: self.core.diagnostics.warnings.len(),
            prefix,
            prefix_read: false,
            paragraphs: self.layout.paragraph_count,
            packs: self.layout.pack_serial,
        };
        *self.env.reuse_effects.borrow_mut() =
            Some(Rc::new(RefCell::new(ReuseEffectJournal::default())));
        *self.env.reuse_sequence.borrow_mut() = Some(capture.scope.sequence());
        self.env.reuse_font_names.borrow_mut().clear();
        self.env.reuse_font_writes.borrow_mut().clear();
        self.env.reuse_font_symbol_inputs.borrow_mut().clear();
        self.env.reuse_font_symbol_outputs.borrow_mut().clear();
        self.env.reuse_outer_prev.borrow_mut().clear();
        self.env
            .reuse_base_context_depth
            .set(self.builder.context_stack.len());
        self.env.reuse_line_sensitive.set(false);
        self.env.reuse_rejected_by.set(None);
        #[cfg(feature = "latency-diagnostics")]
        self.env.reuse_rejection_site.set(None);
        self.env.values.begin_read_writes();
        self.env.meanings.begin_read_writes();
        self.env.box_registers.begin_read_writes();
        self.env.reuse_symbol_keys.borrow_mut().clear();
        self.env.reuse_symbols.borrow_mut().clear();
        self.env.reuse_expansions.set(Some((0, true)));
        self.env.reuse_input_lookups.borrow_mut().clear();
        self.env
            .reuse_physical_input
            .set(self.core.config.input_execution_reuse.then_some(false));
        self.env
            .reuse_font_construction_allowed
            .set(self.core.config.font_construction_reuse);
        self.env
            .reuse_paragraph_level
            .set((kind == RegionKind::Paragraph).then_some(self.env.cur_level));
        self.env.reuse_stateful.set(true);
        self.core.reuse.grouped.misses += 1;
        self.core.reuse.grouped.active = Some(capture);
        Ok(false)
    }
}
impl EvalView<'_> {
    // Tail quantities may inspect only material constructed in this segment or
    // its completely validated flat terminal input. Page-builder and math tails
    // are outside this boundary. No mutation or consumption is permitted here.
    pub(in crate::engine) fn grouped_tail_read(&mut self, code: i64) {
        let Some(capture) = self.core.reuse.grouped.active.as_mut() else {
            return;
        };
        let c = &self.builder.context;
        let valid = (|| -> Option<()> {
            if !matches!(c.mode, ListMode::Horizontal | ListMode::Vertical)
                || (c.mode == ListMode::Vertical && !c.restricted)
            {
                return None;
            }
            if c.cur_l.is_some() || !c.lig_stack.is_empty() {
                return Some(());
            }
            let tail = c.cur_list_tail;
            if tail == C::NULL {
                return Some(());
            }
            let node = self.builder.node(tail).ok()?;
            if capture.old_nodes.get(&tail).is_some_and(Option::is_some) {
                let (_, old_tail, before) = capture.prefix.as_ref()?;
                if *old_tail != tail
                    || edges(before)[1..].iter().any(|&n| n != C::NULL)
                    || edges(node)[1..].iter().any(|&n| n != C::NULL)
                {
                    return None;
                }
                capture.prefix_read = true;
            } else if node.node_type == crate::core::enums::NodeKind::Disc
                && node.list_ptr != C::NULL
            {
                for id in self.builder.siblings(node.list_ptr).ok()? {
                    if capture.old_nodes.get(&id).is_some_and(Option::is_some) {
                        return None;
                    }
                }
            }
            Some(())
        })()
        .is_some();
        if !valid || !matches!(code, 0..=2 | 5) {
            self.core
                .reuse
                .grouped
                .reject(self.env, "external-tail-read");
        }
    }
}
impl EngineView<'_> {
    pub(in crate::engine) fn grouped_command(&mut self) {
        if self.core.reuse.grouped.active.is_none() {
            return;
        }
        if self.env.reuse_expansions.get().is_none()
            || !self.env.values.recording()
            || !self.env.meanings.recording()
            || !self.env.box_registers.recording()
        {
            let reason = self.env.reuse_rejected_by.get().unwrap_or_else(|| {
                if !self.env.values.recording() {
                    "value-table-escape"
                } else if !self.env.meanings.recording() {
                    "meaning-table-escape"
                } else {
                    "box-table-escape"
                }
            });
            self.core.reuse.grouped.reject(self.env, reason);
            return;
        }
        if self
            .core
            .input
            .token_count
            .saturating_sub(self.core.reuse.grouped.active.as_ref().unwrap().token_start)
            > 131072
        {
            self.core.reuse.grouped.reject(self.env, "token-budget");
            return;
        }
        let current = &self.core.input.current;
        let command = current.operation();
        let tail = self.builder.context.cur_list_tail;
        let old_tail = tail != C::NULL
            && self
                .core
                .reuse
                .grouped
                .active
                .as_ref()
                .unwrap()
                .old_nodes
                .get(&tail)
                .is_some_and(Option::is_some);
        if old_tail
            && (command == Some(Command::ItalCorr)
                || ((current.is_category(Cat::Spacer) || command == Some(Command::ExSpace))
                    && InputProcessor::parameter(self.env, "pdfadjustinterwordglue", 0) > 0))
        {
            self.core
                .reuse
                .grouped
                .reject(self.env, "external-tail-read");
            return;
        }
        if command == Some(Command::AfterGroup)
            && self.env.reuse_paragraph_level.get() == Some(self.env.cur_level)
        {
            self.core
                .reuse
                .grouped
                .reject(self.env, "escaping-aftergroup");
            return;
        }
        if command == Some(Command::RemoveItem)
            && (old_tail || self.builder.context.mode != ListMode::Horizontal)
        {
            self.core
                .reuse
                .grouped
                .reject(self.env, "external-tail-removal");
            return;
        }
        let safe = matches!(
            current.category(),
            Some(Cat::Letter | Cat::OtherChar | Cat::Spacer | Cat::LeftBrace | Cat::RightBrace)
        ) || matches!(
            command,
            Some(
                Command::Relax
                    | Command::CharGiven
                    | Command::CharNum
                    | Command::Def
                    | Command::Let
                    | Command::BeginGroup
                    | Command::EndGroup
                    | Command::Global
                    | Command::LongPrefix
                    | Command::OuterPrefix
                    | Command::ProtectedPrefix
                    | Command::Parameter
                    | Command::Register
                    | Command::RegAlias
                    | Command::Shorthand
                    | Command::Arithmetic
                    | Command::CodeAssign
                    | Command::SpaceFactor
                    | Command::PrevDepth
                    | Command::DefFont
                    | Command::SetFont
                    | Command::SetBox
                    | Command::BoxDimen
                    | Command::Hskip
                    | Command::Vskip
                    | Command::Kern
                    | Command::Rule
                    | Command::ShiftBox
                    | Command::IgnoreSpaces
                    | Command::ExSpace
                    | Command::NoBoundary
                    | Command::ItalCorr
                    | Command::StartPar
                    | Command::Penalty
                    | Command::ParShape
                    | Command::Penalties
                    | Command::SetLanguage
                    | Command::AfterGroup
                    | Command::AfterAssignment
                    | Command::InStream
                    | Command::ReadStream
                    | Command::OutStream
                    | Command::Immediate
                    | Command::Message
                    | Command::Special
                    | Command::RemoveItem
            )
        ) || (command == Some(Command::UnHbox)
            && matches!(&current.value, CommandOperand::Unpack(op) if op.discard_source().is_none()))
            || (command == Some(Command::ParEnd)
                && self.builder.context_stack.len()
                    > self
                        .core
                        .reuse
                        .grouped
                        .active
                        .as_ref()
                        .unwrap()
                        .guard
                        .stack
                        .len())
            || (command == Some(Command::MakeBox)
                && matches!(
                    current.value,
                    CommandOperand::Box(
                        crate::core::enums::BoxOperation::Hbox
                            | crate::core::enums::BoxOperation::Vbox
                            | crate::core::enums::BoxOperation::Vtop
                            | crate::core::enums::BoxOperation::Box
                            | crate::core::enums::BoxOperation::Copy
                    )
                ))
            || (command == Some(Command::PdfExtension)
                && matches!(&current.value,CommandOperand::Text(name)
                if ["pdfximage","pdfrefximage","pdfliteral","pdfsave","pdfrestore","pdfsetmatrix","pdfcolorstack"].contains(&name.as_str())));
        let safe = (safe
            || (self.core.config.stateful_paragraph_reuse && command == Some(Command::Accent))
            || (self.core.config.font_construction_reuse
                && matches!(command, Some(Command::FontInt | Command::FontDimen))))
            && (command != Some(Command::DefFont)
                || (self.core.config.font_construction_reuse && current.character() == Some(0)));
        if !safe {
            let reason = if command == Some(Command::PdfExtension) {
                if let CommandOperand::Text(name) = &current.value {
                    name.as_str()
                } else {
                    "pdf_extension"
                }
            } else {
                command.map_or("category", Command::as_str)
            };
            self.core.reuse.grouped.reject(self.env, reason);
            return;
        }
        if command == Some(Command::SetFont) {
            let number = current.character().unwrap_or(-1) as usize;
            if let Some(font) = self.fonts.items.get(number).filter(|f| compatible_font(f)) {
                let capture = self.core.reuse.grouped.active.as_mut().unwrap();
                if number < capture.guard.font_count {
                    capture
                        .guard
                        .fonts
                        .entry(number)
                        .or_insert_with(|| FontGuard::new(font));
                }
            } else {
                self.core.reuse.grouped.reject(self.env, "font");
            }
        }
    }
    pub(in crate::engine) fn grouped_image_start(&mut self) -> Option<u64> {
        let capture = self.core.reuse.grouped.active.as_ref()?;
        if capture.pdf_events.len() >= 256 {
            self.core
                .reuse
                .grouped
                .reject(self.env, "image-event-budget");
            return None;
        }
        let sequence = capture.scope.sequence();
        let n = sequence.get();
        sequence.set(n + 1);
        Some(n)
    }
    pub(in crate::engine) fn grouped_image_end(&mut self, sequence: Option<u64>) {
        let Some(n) = sequence else { return };
        let Some(capture) = self.core.reuse.grouped.active.as_mut() else {
            return;
        };
        // Construction mutates PDF state across scanner resumptions. Until a
        // finer effect boundary is supported, interleaved observable effects
        // make this whole segment ineligible.
        if capture.scope.sequence().get() != n + 1 {
            self.core
                .reuse
                .grouped
                .reject(self.env, "interleaved image effects");
            return;
        }
        if capture.pdf_guard.matches(&self.core.backend) {
            return;
        }
        if let Some(recipe) = capture.pdf_guard.finish(&self.core.backend) {
            capture.pdf_events.push((n, recipe));
            capture.pdf_guard = pdf::PdfGuard::new(
                &self.core.backend,
                self.core.config.material_region_admission,
            );
        } else {
            self.core.reuse.grouped.reject(self.env, "image ownership");
        }
    }
    pub(in crate::engine) fn finish_grouped_reuse(&mut self, normal: bool) {
        if !normal {
            self.core.reuse.grouped.reject(self.env, "group-abort");
            return;
        }
        let Some(capture) = self.core.reuse.grouped.active.as_ref() else {
            return;
        };
        if capture.kind == RegionKind::Paragraph {
            if self.env.cur_level < capture.guard.level {
                self.core
                    .reuse
                    .grouped
                    .reject(self.env, "paragraph-scope-exit");
            }
            return;
        }
        if self.env.cur_level != capture.guard.level {
            return;
        }
        let start = &capture.guard.input.current;
        let end = &self.core.input.current;
        if !((start.is_category(Cat::LeftBrace) && end.is_category(Cat::RightBrace))
            || (start.is_command(Command::BeginGroup) && end.is_command(Command::EndGroup)))
        {
            self.core.reuse.grouped.reject(self.env, "group-exit-token");
            return;
        }
        self.finish_execution_region();
    }
    pub(in crate::engine) fn finish_stateful_paragraph(&mut self) {
        if !self.core.reuse.grouped.active.as_ref().is_some_and(|c| {
            c.kind == RegionKind::Paragraph
                && self.env.cur_level == c.guard.level
                && self.builder.context_stack.len() == c.guard.stack.len()
        }) {
            return;
        }
        if !self.core.input.current.is_command(Command::ParEnd)
            && !(self.core.input.current.control_sequence == self.core.input.par_loc
                && self.core.input.current.is_command(Command::Macro))
        {
            return;
        }
        self.finish_execution_region();
    }
    fn finish_execution_region(&mut self) {
        let capture = self.core.reuse.grouped.active.take().unwrap();
        #[cfg(feature = "latency-diagnostics")]
        self.core.reuse.grouped.measure(&capture, "closed");
        self.env.reuse_stateful.set(false);
        self.env.reuse_paragraph_level.set(None);
        self.env.reuse_font_construction_allowed.set(false);
        let uses_input = self.env.reuse_physical_input.replace(None) == Some(true);
        let input_lookups = std::mem::take(&mut *self.env.reuse_input_lookups.borrow_mut());
        let journal = self.env.reuse_effects.borrow_mut().take();
        self.env.reuse_sequence.borrow_mut().take();
        let font_names = std::mem::take(&mut *self.env.reuse_font_names.borrow_mut());
        let font_inputs = std::mem::take(&mut *self.env.reuse_font_symbol_inputs.borrow_mut());
        let values = self.env.values.finish_read_writes();
        let meanings = self.env.meanings.finish_read_writes();
        let boxes = self.env.box_registers.finish_read_writes();
        let expansions = self.env.reuse_expansions.take();
        let mut symbols = std::mem::take(&mut *self.env.reuse_symbols.borrow_mut());
        let recorded_keys = std::mem::take(&mut *self.env.reuse_symbol_keys.borrow_mut());
        let origin_line = capture.guard.input.origin_line;
        let admission = capture.admission.clone();
        let mut failure = self.env.reuse_rejected_by.get().unwrap_or("observation");
        let built = (|| -> Option<GroupEntry> {
            #[cfg(feature = "latency-diagnostics")]
            let _capture = crate::latency_diagnostics::phase(
                "group_reuse_capture",
                "balanced construction",
                None,
            );
            let (initial_values, value_writes, value_reads) = values?;
            let journal = journal?;
            let journal = std::mem::take(&mut *journal.borrow_mut());
            let (initial_meanings, meaning_writes, meaning_reads) = meanings?;
            let (initial_boxes, _box_writes, box_reads) = boxes?;
            let (macros, valid) = expansions?;
            if !valid {
                return None;
            }
            let guard = &capture.guard;
            failure = "state-boundary";
            if guard.cell != cell_guard(self)
                || !same_handler(&capture.incoming_alignment, &self.core.input.alignment_handler)
                || self.env.cur_level != guard.level || self.env.cur_group != guard.group
                || (capture.kind == RegionKind::Group && frames(self.env) != guard.frames)
                || self.fonts.current != guard.current_font
                || guard.conditions != conditions(self)
                // Incoming frames carry diagnostic source locations. Equal
                // branch flags cannot prove a popped/recreated frame unchanged.
                || capture.incoming_conditions.len() != self.core.conditionals.stack.len()
                || capture.incoming_conditions.iter().zip(&self.core.conditionals.stack)
                    .any(|(before, after)| !Rc::ptr_eq(before, after))
                || !self.core.streams.readers.is_empty()
                || self
                    .core
                    .streams
                    .writers
                    .keys()
                    .copied()
                    .collect::<Vec<_>>()
                    != guard.writers
                || self.core.assignments.after_assignment.is_some()
                || self.core.box_after_assignment.is_some()
                || self.builder.context.mode != ListMode::Horizontal
                || self.core.input.cat_code != guard.catcodes
                || self
                    .builder
                    .context_stack
                    .iter()
                    .cloned()
                    .map(group_context)
                    .collect::<Vec<_>>()
                    != guard.stack
                || self.layout.pages.len() != guard.pages
                || self.hyphenation.ready != guard.hyphen.ready
                || self.hyphenation.tries.storage_address() != guard.hyphen.tries.as_ptr() as usize
                || self.hyphenation.exceptions.storage_address()
                    != guard.hyphen.exceptions.as_ptr() as usize
                || self.hyphenation.saved_codes.storage_address()
                    != guard.hyphen.codes.as_ptr() as usize
            {
                return None;
            }
            if self.core.input.recovery.count > 0
                || capture.diagnostics
                    != (
                        self.core.diagnostics.errors.len(),
                        self.core.diagnostics.records.len(),
                        self.core.diagnostics.help.len(),
                    )
                || self.builder.events.iter().any(|e| {
                    matches!(
                        e,
                        crate::typesetting::nodes::BuilderEvent::MissingCharacter { .. }
                    )
                })
                || !capture.pdf_guard.matches(&self.core.backend)
            {
                return None;
            }
            failure = "input-boundary";
            let end = Boundary::read(self)?;
            if uses_input
                && (self.core.input.last_input_error.is_some()
                    || self.core.input.last_missing_resource.is_some())
            {
                return None;
            }
            if end.path != guard.input.path
                || end.position < guard.input.position
                || end.position - guard.input.position > SOURCE
            {
                return None;
            }
            if capture.kind == RegionKind::Paragraph {
                let trimmed = guard.input.position.saturating_sub(usize::from(
                    capture.bytes.get(guard.input.position.wrapping_sub(1)) == Some(&b'\n'),
                ));
                let start = capture.bytes[..trimmed]
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .map_or(0, |n| n + 1);
                if self
                    .core
                    .reuse
                    .edited_offset
                    .is_some_and(|n| n >= start && n < end.position)
                {
                    failure = "edited-paragraph";
                    return None;
                }
            }
            let mut context = self.builder.context.clone();
            let mut prefix_tail = None;
            let head = if let Some((head, tail, node)) = &capture.prefix {
                let Ok(current) = self.builder.node(*tail) else {
                    failure = "prefix-reclaimed";
                    return None;
                };
                failure = "prefix-child-links";
                if edges(current)[1..] != edges(node)[1..] {
                    return None;
                }
                failure = "prefix-head";
                if context.cur_list_head != *head {
                    return None;
                }
                failure = "prefix-content";
                if capture.prefix_read || !current.shares_contents_with(node) {
                    // Finishing a pending ligature can modify the old terminal
                    // node. Support only a flat, fully validated material input;
                    // nested/opaque payloads remain an ordinary-execution path.
                    if edges(node)[1..].iter().any(|&id| id != C::NULL)
                        || !matches!(
                            node.payload,
                            NodePayload::None | NodePayload::CharacterPolicy { .. }
                        )
                        || !matches!(
                            current.payload,
                            NodePayload::None | NodePayload::CharacterPolicy { .. }
                        )
                        || bincode::serialized_size(node).ok()? > 64 * 1024
                        || bincode::serialized_size(current).ok()? > 64 * 1024
                    {
                        return None;
                    }
                    prefix_tail = Some((node.clone(), current.clone()));
                }
                current.link
            } else {
                context.cur_list_head
            };
            let roots = if head == C::NULL { vec![] } else { vec![head] };
            failure = "material-payload";
            let graph = Graph::capture_nodes(&self.builder.nodes, &roots, true)?;
            // Material may not retain revision-local ownership from an input
            // graph. Copies and freshly constructed/unpacked local lists have
            // new graph IDs; consuming preexisting material falls back.
            if graph
                .source_ids
                .iter()
                .any(|id| capture.old_nodes.get(id).is_some_and(Option::is_some))
            {
                failure = "external-material-ownership";
                return None;
            }
            failure = "material-context";
            context = map_context(context, |id| {
                if id == C::NULL {
                    return Some(C::NULL);
                }
                if let Some((head, tail, _)) = &capture.prefix {
                    if id == *head {
                        return Some(-2);
                    }
                    if id == *tail {
                        return Some(-3);
                    }
                }
                graph
                    .source_ids
                    .iter()
                    .position(|&n| n == id)
                    .map(|n| n as i64 + 1)
            })?;
            failure = "assignment-contract";
            let mut levels = BTreeMap::new();
            if !value_writes.is_subset(&journal.values)
                || !meaning_writes.is_subset(&journal.meanings)
            {
                return None;
            }
            let mut produced_font_symbols = BTreeMap::new();
            let mut final_font_names = BTreeMap::new();
            // A small independent projection uses the ordinary assignment/save
            // implementation, never a cloned whole meaning table. It proves the
            // escaping journal accounts for the final state and save stack.
            let mut projected = Environment::default();
            projected.cur_level = guard.level;
            projected.cur_group = guard.group;
            projected.groups = capture.initial_groups.clone();
            for (key, value) in &initial_values {
                let restoration = RestorationKey::Value(key.clone());
                let level = capture
                    .value_levels
                    .get(&restoration)
                    .copied()
                    .unwrap_or(C::LEVEL_ONE);
                projected.value_levels.insert(restoration.clone(), level);
                if capture.kind == RegionKind::Paragraph {
                    levels.insert(restoration, level);
                }
                if let Some(value) = value {
                    projected.values.insert(key.clone(), value.clone());
                }
            }
            for (key, value) in &initial_meanings {
                if let Some(value) = value {
                    projected.meanings.insert(key.clone(), value.clone());
                }
            }
            for (_, effect) in &journal.effects {
                match effect {
                    ReuseStateEffect::FontSymbol { number, id } => {
                        produced_font_symbols.insert(*id, *number);
                    }
                    ReuseStateEffect::FontName { id, name } => {
                        final_font_names.insert(*id, name.clone());
                    }
                    ReuseStateEffect::Value(key, value, global) => {
                        projected.assign_value(key.clone(), value.clone(), *global);
                    }
                    ReuseStateEffect::Meaning(key, command, value, global) => {
                        projected.define_key(key.clone(), *command, value.clone(), *global);
                    }
                }
            }
            if produced_font_symbols
                .iter()
                .any(|(id, n)| self.env.font_symbols.get(n) != Some(id))
                || final_font_names
                    .iter()
                    .any(|(id, name)| self.env.font_identifiers.get(id) != Some(name))
            {
                return None;
            }
            for key in &value_writes {
                let restoration = RestorationKey::Value(key.clone());
                let final_level = self
                    .env
                    .value_levels
                    .get(&restoration)
                    .copied()
                    .unwrap_or(C::LEVEL_ONE);
                if self.env.values.get(key) != projected.values.get(key)
                    || final_level
                        != projected
                            .value_levels
                            .get(&restoration)
                            .copied()
                            .unwrap_or(C::LEVEL_ONE)
                {
                    return None;
                }
            }
            for key in &meaning_writes {
                if !meaning_matches(
                    self.env.meanings.get(key.as_ref()),
                    projected.meanings.get(key.as_ref()),
                ) {
                    return None;
                }
            }
            if capture.kind == RegionKind::Paragraph
                && bincode::serialize(&projected.groups).ok()?
                    != bincode::serialize(&self.env.groups).ok()?
            {
                return None;
            }
            // Replaying an ending macro's former meaning would be unsafe.
            if capture.kind == RegionKind::Paragraph
                && meaning_writes.contains(&Arc::from(
                    self.env.symbol_key(self.core.input.par_loc, false),
                ))
            {
                return None;
            }
            let values = initial_values
                .into_iter()
                .filter(|(key, _)| value_reads.contains(key))
                .collect();
            let meanings = initial_meanings
                .into_iter()
                .filter(|(key, _)| meaning_reads.contains(key))
                .collect::<BTreeMap<_, _>>();
            let mut boxes = BTreeMap::new();
            for (register, old) in initial_boxes {
                let old = old.unwrap_or(C::NULL);
                let current = self
                    .env
                    .box_registers
                    .get(&register)
                    .copied()
                    .unwrap_or(C::NULL);
                // A local replacement hides the previous independently owned graph
                // until restoration. Any actual read before replacement needs a
                // complete graph witness. Existing graph mutation may not escape.
                if current != old {
                    return None;
                }
                if box_reads.contains(&register) {
                    let before = if old == C::NULL {
                        None
                    } else {
                        Some(Graph::capture_nodes(&capture.old_nodes, &[old], true)?)
                    };
                    let after = if current == C::NULL {
                        None
                    } else {
                        Some(Graph::capture_nodes(&self.builder.nodes, &[current], true)?)
                    };
                    if match (&before, &after) {
                        (None, None) => false,
                        (Some(a), Some(b)) => !a.same_input(b),
                        _ => true,
                    } {
                        return None;
                    }
                    boxes.insert(register, before);
                }
            }
            let interned = self
                .env
                .symbols
                .range(guard.next_symbol..)
                .map(|(&id, name)| (id, name.clone()))
                .collect::<Vec<_>>();
            if interned.len() > 1024
                || self.env.next_symbol.checked_sub(guard.next_symbol)? as usize != interned.len()
            {
                return None;
            }
            let symbol_keys = recorded_keys.into_iter().collect::<Vec<_>>();
            for token in guard
                .input
                .tokens()
                .chain(end.tokens())
                .chain(std::iter::once(end.current.token))
            {
                if let Some(cs) = token.control_sequence_id() {
                    symbols
                        .entry(cs)
                        .or_insert_with(|| self.env.symbol_key(cs, false));
                }
            }
            for value in meanings
                .values()
                .flatten()
                .chain(projected.meanings.raw().values())
            {
                if let CommandOperand::Macro(m) = &value.value {
                    for token in m
                        .body
                        .iter()
                        .copied()
                        .chain(m.parameters.iter().flat_map(|p| {
                            p.prefix
                                .iter()
                                .chain(p.delimiters.iter().flatten())
                                .copied()
                                .chain(p.hash_brace)
                        }))
                    {
                        if let Some(cs) = token.control_sequence_id() {
                            symbols
                                .entry(cs)
                                .or_insert_with(|| self.env.symbol_key(cs, false));
                        }
                    }
                }
            }
            for node in &graph.nodes {
                let tokens = match &node.payload {
                    NodePayload::Special(s) => Some(&s.tokens),
                    NodePayload::Io(crate::typesetting::model::IoEffect::Write {
                        tokens, ..
                    }) => Some(tokens),
                    _ => None,
                };
                if let Some(tokens) = tokens {
                    for token in tokens.iter() {
                        if let Some(cs) = token.control_sequence_id() {
                            symbols
                                .entry(cs)
                                .or_insert_with(|| self.env.symbol_key(cs, false));
                        }
                    }
                }
            }
            failure = "symbols";
            if symbols.len() > 2048 {
                return None;
            }
            failure = "small-region";
            if macros < 16
                && graph.nodes.len() < 128
                && journal.effects.len() < 16
                && !graph.nodes.iter().any(|n| {
                    matches!(
                        n.payload,
                        NodePayload::Pdf(crate::typesetting::model::PdfEffect::RefImage(_))
                    )
                })
            {
                return None;
            }
            failure = capture.scope.rejection().unwrap_or("resource-budget");
            let (resources, console) = capture.scope.finish()?;
            failure = "write-contract";
            let writes = capture.scope.write_journal(Path::new(&guard.input.path))?;
            failure = "font-construction";
            let constructed_fonts = if let Some(scope) = &capture.font_scope {
                scope.finish(self.fonts)?
            } else {
                if self.fonts.items.len() != guard.font_count {
                    return None;
                }
                crate::fonts::construction::Recipe::empty()
            };
            if self.core.config.material_region_admission
                && capture.kind == RegionKind::Group
                && graph.nodes.len() < 128
                && macros < 256
                && self
                    .core
                    .input
                    .token_count
                    .saturating_sub(capture.token_start)
                    < 4096
                && !graph.nodes.iter().any(|n| {
                    matches!(
                        n.payload,
                        NodePayload::Pdf(crate::typesetting::model::PdfEffect::RefImage(_))
                    )
                })
                && constructed_fonts.outputs.is_empty()
                && capture.pdf_events.is_empty()
            {
                failure = "small-material-admission";
                return None;
            }
            let pdf = capture.pdf_events;
            // Publish a closed immutable result; temporary boxes/state are gone.
            let paragraphs = self
                .layout
                .paragraph_count
                .checked_sub(capture.paragraphs)?;
            let packs = self.layout.pack_serial.wrapping_sub(capture.packs);
            let source = capture.bytes[guard.input.position..end.position].to_vec();
            let mut guard = capture.guard;
            failure = "material-font";
            for node in prefix_tail
                .iter()
                .flat_map(|(a, b)| [a, b])
                .chain(graph.nodes.iter())
                .chain(boxes.values().flatten().flat_map(|g| g.nodes.iter()))
            {
                if node.glyph() {
                    let font = self.fonts.items.get(node.font)?;
                    if !compatible_font(font) {
                        return None;
                    }
                    if node.font < guard.font_count {
                        guard
                            .fonts
                            .entry(node.font)
                            .or_insert_with(|| FontGuard::new(font));
                    } else if !constructed_fonts.outputs.contains_key(&node.font) {
                        return None;
                    }
                }
            }
            if let Some(scope) = &capture.font_scope {
                for (&number, font) in scope.reads.borrow().iter() {
                    guard
                        .fonts
                        .entry(number)
                        .or_insert_with(|| FontGuard::new(font));
                }
            }
            failure = "input-source-receipt";
            let input_bindings = capture.scope.input_bindings();
            if uses_input
                && input_bindings
                    .iter()
                    .any(|p| !input_lookups.iter().any(|l| &l.resolved == p))
            {
                return None;
            }
            let source_resources = if uses_input {
                input_bindings
                    .iter()
                    .map(|p| {
                        let path = crate::resources::paths::resolve_path(p)
                            .to_string_lossy()
                            .into_owned();
                        Some((
                            path.clone(),
                            self.core.input.source_resources.get(&path)?.clone(),
                        ))
                    })
                    .collect::<Option<Vec<_>>>()?
            } else {
                Vec::new()
            };
            let mut entry = GroupEntry {
                kind: capture.kind,
                created_generation: self.core.reuse.grouped.cache.borrow().generation,
                reused: std::cell::Cell::new(false),
                writes,
                admission: capture.admission.clone(),
                guard,
                source,
                end,
                context,
                graph,
                values,
                meanings,
                levels,
                boxes,
                symbols,
                interned,
                symbol_keys,
                resources,
                console,
                pdf,
                input_bindings,
                uses_input,
                source_resources,
                input_lookups,
                tokens: self.core.input.token_count - capture.token_start,
                expansions: self.core.expansion.expansion_steps - capture.expansion_start,
                macros,
                warnings: self.core.diagnostics.warnings[capture.warnings..].to_vec(),
                hsize: self.layout.hsize,
                badness: self.layout.last_badness(),
                paragraphs,
                packs,
                last_line: self.math.last_line(),
                end_eof: self.core.stream_eof.clone(),
                bytes: 0,
                #[cfg(feature = "latency-diagnostics")]
                retained_charges: (0, 0, 0),
                line_sensitive: self.env.reuse_line_sensitive.get(),
                effects: journal.effects,
                console_order: capture.scope.console_sequence(),
                font_names,
                font_inputs,
                constructed_fonts,
                produced_font_symbols,
                prefix_tail,
                outer_prev: std::mem::take(&mut *self.env.reuse_outer_prev.borrow_mut()),
            };
            failure = "retention-budget";
            let serialized = bincode::serialized_size(&(
                &entry.values,
                &entry.symbols,
                &entry.console,
                &entry.effects,
            ))
            .ok()? as usize;
            let meanings_bytes = if self.core.config.material_region_admission {
                input::retained_meanings(&entry.meanings)?
            } else {
                (bincode::serialized_size(&entry.meanings).ok()? as usize).saturating_mul(4)
            };
            let mut bytes = serialized.saturating_mul(4)
                + meanings_bytes
                + entry.guard.cell.as_ref().map_or(0, |c| {
                    c.handler.suffix.len() * std::mem::size_of::<Token>() + 256
                })
                + entry.guard.input.bytes()?
                + entry.end.bytes()?
                + entry.source.len()
                + entry.graph.bytes()
                + entry.resources.retained_bytes()
                + entry.writes.retained_bytes()
                + entry.constructed_fonts.bytes()?
                + entry
                    .prefix_tail
                    .as_ref()
                    .map(|p| bincode::serialized_size(p).unwrap_or(u64::MAX / 4) as usize * 4)
                    .unwrap_or(0)
                + entry.pdf.iter().map(|(_, p)| p.bytes()).sum::<usize>()
                + entry
                    .input_bindings
                    .iter()
                    .map(|p| p.as_os_str().len() + 64)
                    .sum::<usize>()
                + entry
                    .input_lookups
                    .iter()
                    .map(|l| {
                        l.filename.as_os_str().len()
                            + l.directory.as_os_str().len()
                            + l.resolved.as_os_str().len()
                            + 192
                    })
                    .sum::<usize>()
                + entry
                    .source_resources
                    .iter()
                    .map(|(path, hash)| path.len() + hash.len() + 128)
                    .sum::<usize>()
                + entry.guard.pdf.bytes()
                + 4096;
            for node in &entry.graph.nodes {
                bytes = bytes.saturating_add(bincode::serialized_size(node).ok()? as usize * 4);
            }
            for box_graph in entry.boxes.values().flatten() {
                bytes = bytes.saturating_add(box_graph.bytes());
                for n in &box_graph.nodes {
                    bytes = bytes.saturating_add(bincode::serialized_size(n).ok()? as usize * 4);
                }
            }
            for font in entry.guard.fonts.values() {
                bytes =
                    bytes.saturating_add(bincode::serialized_size(&font.font).ok()? as usize * 4);
            }
            #[cfg(feature = "latency-diagnostics")]
            if crate::latency_diagnostics::enabled() {
                entry.retained_charges = (
                    bincode::serialized_size(&entry.meanings).ok()? as usize * 4,
                    entry
                        .guard
                        .fonts
                        .values()
                        .map(|f| bincode::serialized_size(&f.font).unwrap_or(0) as usize * 4)
                        .sum(),
                    entry.guard.input.bytes()? + entry.end.bytes()?,
                );
            }
            entry.bytes = bytes;
            if bytes > BUDGET {
                return None;
            }
            Some(entry)
        })();
        if let Some(entry) = built {
            let budget = self
                .core
                .reuse
                .group_budget(self.core.config.stateful_execution_reuse);
            let mut cache = self.core.reuse.grouped.cache.borrow_mut();
            while cache.entries.len() >= LIMIT || cache.bytes + entry.bytes > budget {
                let old = cache.entries.pop_front().unwrap();
                cache.bytes -= old.bytes;
                #[cfg(feature = "latency-diagnostics")]
                if crate::latency_diagnostics::enabled() {
                    *self
                        .core
                        .reuse
                        .grouped
                        .rejected
                        .entry(format!("evicted-entry@{}", old.guard.input.origin_line))
                        .or_default() += 1;
                }
            }
            cache.bytes += entry.bytes;
            cache.entries.push_back(Arc::new(entry));
        } else {
            // Admission records contain immutable token identities only. Their
            // separate 128 KiB budget is included in retained-byte statistics.
            self.core
                .reuse
                .grouped
                .cache
                .borrow_mut()
                .reject_admission(admission);
            if self.core.reuse.grouped.rejected.len() >= 64 {
                return;
            }
            *self
                .core
                .reuse
                .grouped
                .rejected
                .entry(format!("{failure}@{origin_line}"))
                .or_default() += 1;
        }
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[test]
    fn negative_admission_retries_and_has_a_byte_budget() {
        let mut cache = GroupCache::default();
        let key = AdmissionKey {
            tokens: vec![Token::Packed(65); 1024].into(),
            index: 1,
        };
        cache.reject_admission(Some(key.clone()));
        let different = AdmissionKey {
            tokens: vec![Token::Packed(65); 1024].into(),
            index: 1,
        };
        assert!(!cache.skip_recording(&Some(different)));
        for _ in 0..8 {
            assert!(cache.skip_recording(&Some(key.clone())));
        }
        assert!(!cache.skip_recording(&Some(key)));
        assert_eq!(cache.negative_bytes, 0);
        for i in 0..80 {
            cache.reject_admission(Some(AdmissionKey {
                tokens: vec![Token::Packed(i); 1024].into(),
                index: 1,
            }));
            assert!(cache.negative_bytes <= 128 * 1024);
            assert!(cache.negative.len() <= 32);
        }
    }
}
