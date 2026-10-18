//! Read-only paragraph execution reuse. Unsupported commands/expansions reject
//! the segment before an entry can be published. Page assembly is never cached.
use super::command_execution::ParagraphMaterial;
use super::*;
use crate::language::services::ListServices;
use crate::typesetting::box_commands::BoxServices;
use crate::{
    core::{
        enums::{CharacterCategory as Cat, Command, InputStateKind, ListMode},
        source::SourceSpan,
    },
    language::{
        input::{CurrentToken, ScannerStatus},
        records::{CommandOperand, Equivalence, InputState},
    },
    typesetting::{
        model::{Node, NodePayload},
        nodes::ListContext,
        paragraph_operations::ParagraphOperations as Par,
    },
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    rc::Rc,
};
mod grouped;
mod layout;
const BYTES: usize = 32 * 1024 * 1024;
const ENTRIES: usize = 256;
const NODES: usize = 8192;
const SOURCE: usize = 32 * 1024;

#[derive(Clone, Debug, serde::Serialize)]
pub struct ReusedRegion {
    pub file: String,
    pub first_line: usize,
    pub last_line: usize,
    pub macros: u64,
    pub tokens: usize,
    pub nodes: usize,
    pub prepared: bool,
    pub grouped: bool,
}
#[derive(Default, Clone, Debug, serde::Serialize)]
pub struct ReuseStats {
    pub contains_reused_graphs: bool,
    pub entry_rejections: BTreeMap<String, u64>,
    pub recent_regions: Vec<ReusedRegion>,
    pub layout_hits: u64,
    pub avoided_layouts: u64,
    pub attempts: u64,
    pub hits: u64,
    pub misses: u64,
    pub rejected: u64,
    pub avoided_macros: u64,
    pub avoided_tokens: u64,
    pub reused_nodes: u64,
    pub group_hits: u64,
    pub group_recording: bool,
    pub group_admission_skips: u64,
    pub group_entries: usize,
    pub group_effects_replayed: u64,
    pub group_rejections: BTreeMap<String, u64>,
    #[cfg(feature = "latency-diagnostics")]
    pub region_measurements: Vec<grouped::RegionMeasurement>,
    #[cfg(feature = "latency-diagnostics")]
    pub region_measurements_dropped: u64,
    #[cfg(feature = "latency-diagnostics")]
    pub material_partition_bytes: (usize, usize, usize),
    #[cfg(feature = "latency-diagnostics")]
    pub group_retained_regions: Vec<(String, usize, bool, usize, u64, usize, usize, usize)>,
    #[cfg(feature = "latency-diagnostics")]
    pub expansion_packets: Vec<crate::latency_diagnostics::ExpansionPacketMeasurement>,
    #[cfg(feature = "latency-diagnostics")]
    pub expansion_packets_dropped: u64,
    pub entries: usize,
    pub layout_entries: usize,
    pub retained_bytes: usize,
}
#[derive(Default)]
struct Cache {
    entries: VecDeque<Arc<Entry>>,
    bytes: usize,
    layouts: VecDeque<Arc<layout::LayoutMemo>>,
    layout_bytes: usize,
}
#[derive(Default)]
pub(crate) struct ExecutionReuse {
    pub(crate) grouped: grouped::GroupReuse,
    cache: Rc<RefCell<Cache>>,
    active: Option<Capture>,
    layout_candidate: Option<layout::PreparationKey>,
    layout_capture: Option<layout::LayoutCapture>,
    pub(crate) edited_offset: Option<usize>,
    pub attempted: bool,
    pub stats: ReuseStats,
}
impl ExecutionReuse {
    #[cfg(all(feature = "latency-diagnostics", target_arch = "wasm32"))]
    pub(crate) fn clear_material(&mut self) {
        let mut cache = self.cache.borrow_mut();
        cache.entries.clear();
        cache.layouts.clear();
        cache.bytes = 0;
        cache.layout_bytes = 0;
        drop(cache);
        self.layout_candidate = None;
        self.layout_capture = None;
        self.grouped.clear_material();
    }

    // Extended regions may use idle paragraph/layout capacity. Existing caches
    // retain their 32 MiB limits; their combined charged retention never exceeds
    // 80 MiB, plus the separately bounded negative-admission metadata.
    fn group_budget(&self, extended: bool) -> usize {
        if !extended {
            return 16 * 1024 * 1024;
        }
        let cache = self.cache.borrow();
        (80usize * 1024 * 1024)
            .saturating_sub(cache.bytes + cache.layout_bytes)
            .min(32 * 1024 * 1024)
    }
    fn rebalance(&self, extended: bool) {
        self.grouped.trim(self.group_budget(extended));
    }
    pub fn checkpoint_clone(&self) -> Self {
        Self {
            grouped: self.grouped.checkpoint_clone(),
            cache: self.cache.clone(),
            active: None,
            layout_candidate: None,
            layout_capture: None,
            edited_offset: self.edited_offset,
            attempted: self.attempted,
            stats: ReuseStats {
                contains_reused_graphs: self.stats.contains_reused_graphs,
                ..Default::default()
            },
        }
    }
    pub fn cancel(&mut self, env: &Environment) {
        self.grouped.cancel(env);
        self.reject(env);
    }
    pub fn reject(&mut self, env: &Environment) {
        if let Some(capture) = self.active.take() {
            self.grouped
                .note_paragraph_rejection(&capture.guard.path, &capture.bytes[capture.start..]);
            self.stats.rejected += 1;
        }
        self.layout_candidate = None;
        self.layout_capture = None;
        if !env.reuse_stateful.get() {
            env.values.cancel_reads();
            env.meanings.cancel_reads();
        }
        env.reuse_boundary.set(false);
        if !env.reuse_stateful.get() {
            env.reuse_expansions.set(None);
        }
    }
    pub fn command(&mut self, env: &Environment, current: &CurrentToken) {
        if self.active.is_none() {
            return;
        }
        let safe = matches!(
            current.category(),
            Some(Cat::Letter | Cat::OtherChar | Cat::Spacer)
        ) || matches!(
            current.operation(),
            Some(Command::Relax | Command::ParEnd | Command::CharGiven)
        );
        if !safe {
            self.reject(env);
        }
    }
}
struct Capture {
    guard: Guard,
    bytes: Arc<[u8]>,
    start: usize,
    cursor: InputState,
    tokens: usize,
    expansions: usize,
    warnings: usize,
}
struct Entry {
    guard: Guard,
    source: Vec<u8>,
    end: InputState,
    end_stack: Vec<InputState>,
    end_line: usize,
    prepared: Option<ListContext>,
    current: CurrentToken,
    end_align: i64,
    tokens: usize,
    expansions: usize,
    macros: u64,
    symbols: BTreeMap<i64, String>,
    values: BTreeMap<ValueKey, Option<Quantity>>,
    meanings: BTreeMap<Arc<str>, Option<Arc<Equivalence>>>,
    graph: Graph,
    penalties: Vec<i64>,
    hsize: i64,
    badness: i64,
    hyphen_ready: bool,
    warnings: Vec<String>,
    bytes: usize,
}
struct Guard {
    path: String,
    line: usize,
    cursor: InputState,
    base_depth: usize,
    context: ListContext,
    first_origins: Vec<Arc<SourceSpan>>,
    indent: Option<Vec<u8>>,
    font_number: usize,
    font: FontGuard,
    profile: crate::core::characters::LanguageProfile,
    catcodes: Vec<Cat>,
    align: i64,
    par_loc: i64,
    par_token: Token,
    cur_level: usize,
    cur_group: crate::core::enums::GroupKind,
    previous: i64,
    hsize: i64,
    greedy: bool,
    exact: bool,
    frozen: std::sync::Weak<BTreeMap<i64, i64>>,
    hyphenation: HyphenGuard,
}
struct HyphenGuard {
    ready: bool,
    tries: std::sync::Weak<BTreeMap<i64, crate::typesetting::hyphenation::Trie>>,
    exceptions: std::sync::Weak<BTreeMap<(i64, Vec<u32>), Vec<usize>>>,
    codes: std::sync::Weak<BTreeMap<i64, crate::typesetting::hyphenation::SavedCodes>>,
}
#[derive(Clone)]
pub(crate) struct FontGuard {
    font: crate::fonts::model::FontInfo,
    micro: crate::fonts::model::MicroCodes,
}
fn arc_same<T: PartialEq + ?Sized>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
        (None, None) => true,
        _ => false,
    }
}
fn table_same<K: Ord, V: PartialEq>(
    a: &crate::core::persistence::SharedTable<K, V>,
    b: &crate::core::persistence::SharedTable<K, V>,
) -> bool {
    a.shares_storage_with(b) || a == b
}
fn optional_table_same<K: Ord, V: PartialEq>(
    a: &Option<crate::core::persistence::SharedTable<K, V>>,
    b: &Option<crate::core::persistence::SharedTable<K, V>>,
) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => table_same(a, b),
        (None, None) => true,
        _ => false,
    }
}
impl FontGuard {
    pub(crate) fn new(font: &crate::fonts::model::FontInfo) -> Self {
        let micro = font.micro_codes.borrow().clone();
        let mut font = font.clone();
        // Keep the dependency snapshot independent of later mutable resource
        // materialization and microtype settings; neither may grow this entry.
        let resource_error = font.materialization.resource_error.clone();
        font.materialization = Default::default();
        font.materialization.resource_error = resource_error;
        // Virtual expansion occurs during ordinary shipment, not construction.
        font.virtual_font = None;
        font.micro_codes = Rc::new(RefCell::new(micro.clone()));
        Self { font, micro }
    }
    pub(crate) fn matches(&self, b: &crate::fonts::model::FontInfo) -> bool {
        let a = &self.font;
        a.name == b.name
            && a.size == b.size
            && optional_table_same(&a.tfm_widths, &b.tfm_widths)
            && optional_table_same(&a.tfm_heights, &b.tfm_heights)
            && optional_table_same(&a.tfm_depths, &b.tfm_depths)
            && arc_same(&a.tfm_lig_kern, &b.tfm_lig_kern)
            && optional_table_same(&a.tfm_lig_start, &b.tfm_lig_start)
            && a.boundary_char == b.boundary_char
            && a.boundary_label == b.boundary_label
            && a.false_boundary_char == b.false_boundary_char
            && a.tfm_kerns == b.tfm_kerns
            && a.tfm_design_size == b.tfm_design_size
            && arc_same(&a.tfm_data, &b.tfm_data)
            && arc_same(&a.afm, &b.afm)
            && a.char_width == b.char_width
            && a.char_height == b.char_height
            && a.char_depth == b.char_depth
            && a.space_width == b.space_width
            && table_same(&a.parameter_overrides, &b.parameter_overrides)
            && a.parameter_count == b.parameter_count
            && a.hyphenchar == b.hyphenchar
            && a.skewchar == b.skewchar
            && a.dynamic == b.dynamic
            && a.null_font == b.null_font
            && a.ligatures_disabled == b.ligatures_disabled
            && table_same(&a.width_overrides, &b.width_overrides)
            && a.letterspacing == b.letterspacing
            && table_same(&a.italic_overrides, &b.italic_overrides)
            && table_same(&self.micro, &b.micro_codes.borrow())
            && a.expansion_policy == b.expansion_policy
            && a.expansion_base == b.expansion_base
            && a.expansion_ratio == b.expansion_ratio
            && a.materialization.resource_error == b.materialization.resource_error
    }
}
fn meaning_matches(a: Option<&Arc<Equivalence>>, b: Option<&Arc<Equivalence>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) if Arc::ptr_eq(a, b) => true,
        (Some(a), Some(b)) if a.command == b.command && a.level == b.level => {
            match (&a.value, &b.value) {
                (CommandOperand::Macro(a), CommandOperand::Macro(b)) => {
                    Arc::ptr_eq(a, b)
                        || (a.body == b.body
                            && a.parameters == b.parameters
                            && a.protected == b.protected
                            && a.source == b.source
                            && a.long == b.long
                            && a.outer == b.outer
                            && a.body_sources == b.body_sources)
                }
                (a, b) => a == b,
            }
        }
        _ => false,
    }
}
fn context_key(mut c: ListContext) -> ListContext {
    c.cur_list_head = C::NULL;
    c.cur_list_tail = C::NULL;
    c.cur_q = C::NULL;
    c.cur_l_origins.clear();
    c.lig_origins.clear();
    if let Some(item) = &mut c.cur_item {
        item.origins.clear();
    }
    for item in &mut c.lig_stack {
        item.origins.clear();
    }
    c
}
fn physical_entry(input: &InputProcessor) -> Option<&InputState> {
    if input.cursor.token_list.is_none() {
        return Some(&input.cursor);
    }
    let parent = input.input_stack.last()?;
    if input.cursor.token_list.as_ref()?.len() == 1
        && input.cursor.token_index == 1
        && input.cursor.token_sources.is_none()
        && parent.token_list.is_none()
        && parent.source == input.cursor.source
        && input
            .cursor
            .source
            .as_ref()
            .is_some_and(|s| s.expansion.is_none() && s.call_site.is_none())
    {
        Some(parent)
    } else {
        None
    }
}
fn entry_reason(view: &mut EngineView<'_>, reason: &str) -> Option<Guard> {
    *view
        .core
        .reuse
        .stats
        .entry_rejections
        .entry(reason.into())
        .or_default() += 1;
    None
}
impl Guard {
    fn new(view: &mut EngineView<'_>) -> Option<Self> {
        let input = &view.core.input;
        let Some(cursor) = physical_entry(input) else {
            return entry_reason(view, "input-frame");
        };
        let context = &view.builder.context;
        let font = &view.fonts.items[view.fonts.current];
        if !view.core.config.execution_reuse
            || view.core.trace.enabled
            || view.core.trace.observer.is_some()
            || view.core.diagnostics.debug_tokens
            || view.core.diagnostics.debug_nodes
            || view.core.config.expansion_limit.is_some()
            || view.builder.capacities != crate::core::capacities::Capacities::default()
            || input.capacities != Default::default()
            || view.env.capacities != Default::default()
            || view.fonts.capacities != Default::default()
            || input.alignment_handler.is_some()
            || input.log_file.is_some()
            || view.core.expansion.expansion_depth_limit.is_some()
            || input.scanner_status != ScannerStatus::Normal
            || input.read_line_only
            || input.scanning_filename
            || input.end_input_requested
            || input.in_open != 1
            || input.cursor.index != 0
            || cursor.state == InputStateKind::TokenList
            || context.mode != ListMode::Horizontal
            || context.restricted
            || !context.vertical_paragraph
            || context.cur_l.is_none()
            || !context.lig_stack.is_empty()
            || context.cur_q != C::NULL
            || !context.unicode_run.is_empty()
            || font.unicode_font.is_some()
            || font.expansion_policy.is_some()
            || font.expansion_base.is_some()
        {
            let reason = if font.unicode_font.is_some()
                || font.expansion_policy.is_some()
                || font.expansion_base.is_some()
            {
                "font"
            } else if view.builder.capacities != crate::core::capacities::Capacities::default()
                || view.core.config.expansion_limit.is_some()
            {
                "capacity"
            } else if context.restricted
                || !context.lig_stack.is_empty()
                || context.cur_q != C::NULL
            {
                "context"
            } else if input.in_open != 1
                || input.read_line_only
                || input.scanner_status != ScannerStatus::Normal
            {
                "scanner"
            } else {
                "trace-or-mode"
            };
            return entry_reason(view, reason);
        }
        if view.core.diagnostics.debug
            || view.core.diagnostics.debug_commands
            || (view.core.interaction != 0
                && crate::diagnostics::messages::TEX_TRACE_CONTROLS
                    .iter()
                    .any(|(_, name)| InputProcessor::parameter(view.env, name, 0) != 0))
        {
            return entry_reason(view, "trace");
        }
        let indent = if context.cur_list_head == C::NULL {
            None
        } else {
            let n = view.builder.node(context.cur_list_head).ok()?;
            if n.link != C::NULL
                || n.list_ptr != C::NULL
                || n.pre_break != C::NULL
                || n.post_break != C::NULL
                || n.leader_ptr != C::NULL
                || n.node_type != crate::core::enums::NodeKind::Hlist
                || n.height != 0
                || n.depth != 0
            {
                return entry_reason(view, "prefix-material");
            }
            Some(bincode::serialize(n).ok()?)
        };
        let previous = view
            .builder
            .context_stack
            .iter()
            .rev()
            .find(|c| c.mode == ListMode::Vertical)?
            .prev_graf;
        Some(Self {
            path: cursor.name.clone()?,
            line: cursor.line,
            cursor: cursor.clone(),
            base_depth: input.input_stack.len() - usize::from(input.cursor.token_list.is_some()),
            context: context_key(context.clone()),
            first_origins: context.cur_l_origins.clone(),
            indent,
            font_number: view.fonts.current,
            font: FontGuard::new(font),
            profile: input.profile,
            catcodes: input.cat_code.clone(),
            align: input.align_state,
            par_loc: input.par_loc,
            par_token: input.par_token,
            cur_level: view.env.cur_level,
            cur_group: view.env.cur_group,
            previous,
            hsize: view.layout.hsize,
            greedy: view.layout.greedy_paragraphs,
            exact: view.layout.exact_repro,
            frozen: view.env.frozen_primitives.weak_storage(),
            hyphenation: HyphenGuard {
                ready: view.hyphenation.ready,
                tries: view.hyphenation.tries.weak_storage(),
                exceptions: view.hyphenation.exceptions.weak_storage(),
                codes: view.hyphenation.saved_codes.weak_storage(),
            },
        })
    }
    fn matches(&self, view: &EngineView<'_>) -> bool {
        let input = &view.core.input;
        let Some(cursor) = physical_entry(input) else {
            return false;
        };
        self.path == cursor.name.as_deref().unwrap_or("")
            && self.base_depth
                == input.input_stack.len() - usize::from(input.cursor.token_list.is_some())
            && self.cursor.loc == cursor.loc
            && self.cursor.state == cursor.state
            && self.cursor.buffer == cursor.buffer
            && self.cursor.columns == cursor.columns
            && self.cursor.byte_columns == cursor.byte_columns
            && self.cursor.buffer_base == cursor.buffer_base
            && self.cursor.limit == cursor.limit
            && self.context == context_key(view.builder.context.clone())
            && self.first_origins.len() == view.builder.context.cur_l_origins.len()
            && self
                .first_origins
                .iter()
                .zip(&view.builder.context.cur_l_origins)
                .all(|(old, current)| {
                    rebase(
                        old,
                        &self.path,
                        self.line,
                        self.line,
                        cursor.line as isize - self.line as isize,
                    ) == *current
                })
            && self.indent
                == if view.builder.context.cur_list_head == C::NULL {
                    None
                } else {
                    view.builder
                        .node(view.builder.context.cur_list_head)
                        .ok()
                        .and_then(|n| bincode::serialize(n).ok())
                }
            && self.font_number == view.fonts.current
            && self.font.matches(&view.fonts.items[view.fonts.current])
            && self.profile == input.profile
            && self.catcodes == input.cat_code
            && self.align == input.align_state
            && self.par_loc == input.par_loc
            && self.par_token == input.par_token
            && self.cur_level == view.env.cur_level
            && self.cur_group == view.env.cur_group
            && view
                .builder
                .context_stack
                .iter()
                .rev()
                .find(|c| c.mode == ListMode::Vertical)
                .is_some_and(|c| c.prev_graf == self.previous)
            && self.hsize == view.layout.hsize
            && self.greedy == view.layout.greedy_paragraphs
            && self.exact == view.layout.exact_repro
            && self.frozen.as_ptr() as usize == view.env.frozen_primitives.storage_address()
            && self.hyphenation.ready == view.hyphenation.ready
            && self.hyphenation.tries.as_ptr() as usize == view.hyphenation.tries.storage_address()
            && self.hyphenation.exceptions.as_ptr() as usize
                == view.hyphenation.exceptions.storage_address()
            && self.hyphenation.codes.as_ptr() as usize
                == view.hyphenation.saved_codes.storage_address()
    }
}
pub(super) enum ReuseHit {
    Prepared,
    LaidOut(ParagraphMaterial),
}
struct Graph {
    source_ids: Vec<i64>,
    nodes: Vec<Node>,
    roots: Vec<usize>,
}
fn edges(n: &Node) -> [i64; 5] {
    [n.link, n.list_ptr, n.pre_break, n.post_break, n.leader_ptr]
}
fn set_edges(n: &mut Node, e: [i64; 5]) {
    [n.link, n.list_ptr, n.pre_break, n.post_break, n.leader_ptr] = e;
}
impl Graph {
    fn capture(builder: &NodeBuilder, roots: &[i64]) -> Option<Self> {
        Self::capture_nodes(&builder.nodes, roots, false)
    }
    fn capture_nodes(
        table: &crate::typesetting::nodes::NodeTable,
        roots: &[i64],
        effects: bool,
    ) -> Option<Self> {
        let mut map = BTreeMap::new();
        let mut pending: VecDeque<i64> = roots.iter().copied().collect();
        let mut nodes = Vec::new();
        let mut source_ids = Vec::new();
        while let Some(id) = pending.pop_front() {
            if id == C::NULL {
                continue;
            }
            if map.contains_key(&id) || nodes.len() == NODES {
                return None;
            }
            let n = table.get(&id)?.as_ref()?.clone();
            if !matches!(
                n.payload,
                NodePayload::None | NodePayload::CharacterPolicy { .. }
            ) && !(effects
                && matches!(
                    n.payload,
                    NodePayload::Io(_)
                        | NodePayload::Special(_)
                        | NodePayload::Pdf(
                            crate::typesetting::model::PdfEffect::RefImage(_)
                                | crate::typesetting::model::PdfEffect::Save
                                | crate::typesetting::model::PdfEffect::Restore
                                | crate::typesetting::model::PdfEffect::SetMatrix(_)
                                | crate::typesetting::model::PdfEffect::ColorStack { .. }
                        )
                ))
                || (!n.whatsit.is_empty()
                    && !(effects
                        && matches!(
                            (n.whatsit.as_str(), &n.payload),
                            ("pdf", NodePayload::Pdf(_))
                                | ("special", NodePayload::Special(_))
                                | ("io", NodePayload::Io(_))
                        )))
                || n.shaped.is_some()
                || n.components.len() > NODES
                || n.origins.len() > NODES
            {
                return None;
            }
            map.insert(id, nodes.len() + 1);
            pending.extend(edges(&n).into_iter().filter(|&n| n != C::NULL));
            nodes.push(n);
            source_ids.push(id);
        }
        let roots = roots
            .iter()
            .map(|r| map.get(r).copied())
            .collect::<Option<Vec<_>>>()?;
        for n in &mut nodes {
            let mapped = edges(n).map(|e| {
                if e == C::NULL {
                    C::NULL
                } else {
                    map[&e] as i64
                }
            });
            set_edges(n, mapped);
        }
        Some(Self {
            nodes,
            roots,
            source_ids,
        })
    }
    fn materialize_nodes(
        &self,
        builder: &mut NodeBuilder,
        path: &str,
        start: usize,
        end: usize,
        delta: isize,
    ) -> Result<Vec<i64>> {
        #[cfg(feature = "latency-diagnostics")]
        let _materialize =
            crate::latency_diagnostics::phase("execution_reuse_materialize", "paragraph", None);
        let mut allocated = Vec::new();
        for original in &self.nodes {
            let mut node = original.clone();
            set_edges(&mut node, [C::NULL; 5]);
            if delta != 0 && !node.origins.is_empty() {
                let origins = original
                    .origins
                    .iter()
                    .map(|s| s.as_ref().map(|s| rebase(s, path, start, end, delta)))
                    .collect();
                // Only the already validated source rebase preserves this lineage.
                // Any later semantic mutation clears it through DerefMut.
                node.rebase_map_origins(origins);
            }
            match builder.allocate(node) {
                Ok(id) => allocated.push(id),
                Err(e) => {
                    for &id in &allocated {
                        builder.release(id, false)?;
                    }
                    return Err(e);
                }
            }
        }
        for (i, original) in self.nodes.iter().enumerate() {
            let e = edges(original).map(|e| {
                if e == C::NULL {
                    C::NULL
                } else {
                    allocated[e as usize - 1]
                }
            });
            set_edges(builder.node_mut(allocated[i])?, e);
        }
        Ok(allocated)
    }
    #[cfg(test)]
    fn materialize(
        &self,
        builder: &mut NodeBuilder,
        path: &str,
        start: usize,
        end: usize,
        delta: isize,
    ) -> Result<Vec<i64>> {
        let nodes = self.materialize_nodes(builder, path, start, end, delta)?;
        Ok(self.roots.iter().map(|&r| nodes[r - 1]).collect())
    }
    fn portable_context(&self, mut context: ListContext) -> Option<ListContext> {
        let index = |id| {
            if id == C::NULL {
                Some(C::NULL)
            } else {
                self.source_ids
                    .iter()
                    .position(|&n| n == id)
                    .map(|n| n as i64 + 1)
            }
        };
        context.cur_list_head = index(context.cur_list_head)?;
        context.cur_list_tail = index(context.cur_list_tail)?;
        context.cur_q = index(context.cur_q)?;
        Some(context)
    }
    fn bytes(&self) -> usize {
        self.nodes
            .iter()
            .map(|n| {
                std::mem::size_of::<Node>()
                    + std::mem::size_of::<crate::typesetting::model::NodeData>()
                    + n.components.len() * 4
                    + n.origins.len() * 128
            })
            .sum()
    }
}
fn rebase(
    s: &Arc<SourceSpan>,
    path: &str,
    start: usize,
    end: usize,
    delta: isize,
) -> Arc<SourceSpan> {
    let call = s
        .call_site
        .as_ref()
        .map(|c| rebase(c, path, start, end, delta));
    let changes = s.file.as_ref() == path && s.line >= start && s.line <= end;
    if !changes
        && match (&s.call_site, &call) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        }
    {
        return s.clone();
    }
    let mut s = (**s).clone();
    if changes {
        s.line = s
            .line
            .checked_add_signed(delta)
            .expect("validated source rebase");
    }
    s.call_site = call;
    Arc::new(s)
}
fn rebase_frame(
    mut frame: InputState,
    path: &str,
    start: usize,
    end: usize,
    delta: isize,
) -> InputState {
    if delta == 0 {
        return frame;
    }
    frame.line = frame
        .line
        .checked_add_signed(delta)
        .expect("validated source cursor rebase");
    frame.source = frame
        .source
        .as_ref()
        .map(|s| rebase(s, path, start, end, delta));
    frame.token_sources = frame.token_sources.as_ref().map(|sources| {
        sources
            .iter()
            .map(|s| s.as_ref().map(|s| rebase(s, path, start, end, delta)))
            .collect::<Vec<_>>()
            .into()
    });
    frame
}
fn rebase_context(
    mut context: ListContext,
    path: &str,
    start: usize,
    end: usize,
    delta: isize,
) -> ListContext {
    if delta == 0 {
        return context;
    }
    context.cur_l_origins = context
        .cur_l_origins
        .iter()
        .map(|s| rebase(s, path, start, end, delta))
        .collect();
    context.lig_origins = context
        .lig_origins
        .iter()
        .map(|origins| {
            origins
                .iter()
                .map(|s| rebase(s, path, start, end, delta))
                .collect()
        })
        .collect();
    if let Some(item) = &mut context.cur_item {
        item.origins = item
            .origins
            .iter()
            .map(|s| rebase(s, path, start, end, delta))
            .collect();
    }
    for item in &mut context.lig_stack {
        item.origins = item
            .origins
            .iter()
            .map(|s| rebase(s, path, start, end, delta))
            .collect();
    }
    context
}
impl Engine {
    pub fn execution_reuse_stats(&self) -> ReuseStats {
        let mut stats = self.core.reuse.stats.clone();
        let cache = self.core.reuse.cache.borrow();
        stats.entries = cache.entries.len();
        stats.layout_entries = cache.layouts.len();
        stats.retained_bytes =
            cache.bytes + cache.layout_bytes + self.core.reuse.grouped.retained_bytes();
        stats.group_hits = self.core.reuse.grouped.hits;
        stats.group_recording = self.core.reuse.grouped.recording();
        stats.group_admission_skips = self.core.reuse.grouped.admission_skips;
        stats.group_entries = self.core.reuse.grouped.entries();
        stats.group_effects_replayed = self.core.reuse.grouped.effects;
        stats.group_rejections = self.core.reuse.grouped.rejected.clone();
        #[cfg(feature = "latency-diagnostics")]
        {
            stats.region_measurements = self
                .core
                .reuse
                .grouped
                .measurements
                .iter()
                .cloned()
                .collect();
            stats.region_measurements_dropped = self.core.reuse.grouped.measurement_dropped;
            stats.material_partition_bytes = (
                cache.bytes,
                cache.layout_bytes,
                self.core.reuse.grouped.retained_bytes(),
            );
            stats.group_retained_regions = self.core.reuse.grouped.retained_regions();
        }
        stats.hits += self.core.reuse.grouped.hits;
        stats.misses += self.core.reuse.grouped.misses;
        stats.attempts += self.core.reuse.grouped.hits + self.core.reuse.grouped.misses;
        stats.avoided_macros += self.core.reuse.grouped.macros;
        stats.reused_nodes += self.core.reuse.grouped.nodes;
        stats.avoided_tokens += self.core.reuse.grouped.tokens;
        stats
    }
}
impl EngineView<'_> {
    pub(super) fn try_paragraph_reuse(&mut self) -> Result<Option<ReuseHit>> {
        if self.env.reuse_stateful.get()
            || !self.core.config.execution_reuse
            || self.core.reuse.attempted
            || self.core.reuse.active.is_some()
            || self.builder.context.mode != ListMode::Horizontal
            || !self.builder.context.vertical_paragraph
            || self.builder.context.cur_l.is_none()
        {
            return Ok(None);
        }
        // Evaluate eligibility once at the first completed character. Retrying
        // a rejected paragraph at every subsequent glyph costs more than reuse.
        self.core.reuse.attempted = true;
        #[cfg(feature = "latency-diagnostics")]
        let _validation = crate::latency_diagnostics::phase(
            "execution_reuse_validate",
            "paragraph",
            Some(crate::latency_diagnostics::region(
                self.core.input.source().as_ref(),
            )),
        );
        let Some(guard) = Guard::new(self) else {
            return Ok(None);
        };
        self.core.reuse.stats.attempts += 1;
        let input = &self.core.input;
        let Some(bytes) = input.input_file[0].as_ref().and_then(|s| s.catalog_bytes()) else {
            return Ok(None);
        };
        let end = input.input_file[0]
            .as_ref()
            .unwrap()
            .byte_position()
            .unwrap();
        let trimmed = if bytes.get(end.wrapping_sub(1)) == Some(&b'\n') {
            end - 1
        } else {
            end
        };
        let start = bytes[..trimmed]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |p| p + 1);
        let candidates: Vec<_> = self
            .core
            .reuse
            .cache
            .borrow()
            .entries
            .iter()
            .filter(|e| bytes.get(start..start + e.source.len()) == Some(e.source.as_slice()))
            .cloned()
            .collect();
        for entry in candidates {
            if !entry.guard.matches(self)
                || entry
                    .symbols
                    .iter()
                    .any(|(&cs, key)| self.env.symbol_key(cs, false) != *key)
                || entry
                    .values
                    .iter()
                    .any(|(k, v)| self.env.values.get(k) != v.as_ref())
                || entry
                    .meanings
                    .iter()
                    .any(|(k, v)| !meaning_matches(self.env.meanings.get(k.as_ref()), v.as_ref()))
            {
                continue;
            }
            let delta = self.core.input.cursor.line as isize - entry.guard.line as isize;
            let imported = entry.graph.materialize_nodes(
                &mut self.builder,
                &entry.guard.path,
                entry.guard.line,
                entry.end_line,
                delta,
            )?;
            let boxes: Vec<i64> = entry.graph.roots.iter().map(|&r| imported[r - 1]).collect();
            if self.core.config.exact_local_trace {
                self.core.local.trace(serde_json::json!({"event":"paragraph_execution_reused","file":entry.guard.path,"first_line":entry.guard.line,"last_line":entry.end_line,"prepared":entry.prepared.is_some(),"imported_roots":boxes.len()}));
            }
            let head = self.builder.context.cur_list_head;
            if head != C::NULL {
                self.builder.release(head, true)?;
            }
            if let Some(context) = &entry.prepared {
                self.builder.context = rebase_context(
                    context.clone(),
                    &entry.guard.path,
                    entry.guard.line,
                    entry.end_line,
                    delta,
                );
                let actual = |id| {
                    if id == C::NULL {
                        C::NULL
                    } else {
                        imported[id as usize - 1]
                    }
                };
                self.builder.context.cur_list_head = actual(self.builder.context.cur_list_head);
                self.builder.context.cur_list_tail = actual(self.builder.context.cur_list_tail);
                self.builder.context.cur_q = actual(self.builder.context.cur_q);
            } else {
                self.builder.context = ListContext::default();
                self.builder.pop_context()?;
                self.builder.context.prev_graf += boxes.len() as i64;
                self.layout.hsize = entry.hsize;
                self.layout.set_last_badness(entry.badness);
                self.hyphenation.ready = entry.hyphen_ready;
                if !boxes.is_empty() {
                    self.layout.paragraph_count += 1;
                    let p = *boxes.last().unwrap();
                    let extent = self.with_layout(|_, ctx| Par::line_extent(ctx, p))?;
                    self.math.set_last_line(Some(extent));
                }
            }
            let end_cursor = rebase_frame(
                entry.end.clone(),
                &entry.guard.path,
                entry.guard.line,
                entry.end_line,
                delta,
            );
            self.core.input.input_stack.truncate(entry.guard.base_depth);
            self.core
                .input
                .input_stack
                .extend(entry.end_stack.iter().cloned().map(|frame| {
                    rebase_frame(
                        frame,
                        &entry.guard.path,
                        entry.guard.line,
                        entry.end_line,
                        delta,
                    )
                }));
            self.core.input.cursor = end_cursor;
            self.core.input.current = entry.current.clone();
            self.builder.source = self.core.input.cursor.source.clone();
            for warning in &entry.warnings {
                self.warning(warning);
            }
            self.core.input.align_state = entry.end_align;
            self.core.input.input_file[0]
                .as_mut()
                .unwrap()
                .seek_catalog(start + entry.source.len())?;
            self.core.input.token_count += entry.tokens;
            self.core.expansion.expansion_steps += entry.expansions;
            if entry.prepared.is_none() {
                self.with_layout(|_, ctx| {
                    Par::normal_paragraph(ctx);
                    Ok(())
                })?;
            }
            {
                let mut cache = self.core.reuse.cache.borrow_mut();
                if let Some(index) = cache.entries.iter().position(|e| Arc::ptr_eq(e, &entry)) {
                    let hot = cache.entries.remove(index).unwrap();
                    cache.entries.push_back(hot);
                }
            }
            self.core.reuse.stats.contains_reused_graphs = true;
            if self.core.reuse.stats.recent_regions.len() == 8 {
                self.core.reuse.stats.recent_regions.remove(0);
            }
            self.core.reuse.stats.recent_regions.push(ReusedRegion {
                file: entry.guard.path.clone(),
                first_line: entry.guard.line.checked_add_signed(delta).unwrap(),
                last_line: entry.end_line.checked_add_signed(delta).unwrap(),
                macros: entry.macros,
                tokens: entry.tokens,
                nodes: entry.graph.nodes.len(),
                prepared: entry.prepared.is_some(),
                grouped: false,
            });
            self.core.reuse.stats.hits += 1;
            self.core.reuse.stats.avoided_macros += entry.macros;
            self.core.reuse.stats.avoided_tokens += entry.tokens as u64;
            self.core.reuse.stats.reused_nodes += entry.graph.nodes.len() as u64;
            if entry.prepared.is_some() {
                self.core.reuse.layout_candidate =
                    Some(layout::PreparationKey::ReadOnly(entry.clone()));
                return Ok(Some(ReuseHit::Prepared));
            }
            let hold = self.builder.ownership.hold(boxes.iter().copied());
            return Ok(Some(ReuseHit::LaidOut(ParagraphMaterial {
                boxes,
                penalties: entry.penalties.clone(),
                hold,
            })));
        }
        if self.core.config.stateful_paragraph_reuse
            && self
                .core
                .reuse
                .grouped
                .paragraph_hint(&guard.path, &bytes[start..])
        {
            let hit = self.try_stateful_paragraph_reuse()?;
            if hit {
                return Ok(Some(ReuseHit::Prepared));
            }
            if self.env.reuse_stateful.get() {
                return Ok(None);
            }
        }
        self.core.reuse.stats.misses += 1;
        self.env.values.begin_reads();
        self.env.meanings.begin_reads();
        self.env.reuse_expansions.set(Some((0, true)));
        self.env.reuse_symbols.borrow_mut().clear();
        self.core.reuse.active = Some(Capture {
            guard,
            bytes,
            start,
            cursor: physical_entry(&self.core.input).unwrap().clone(),
            tokens: self.core.input.token_count,
            expansions: self.core.expansion.expansion_steps,
            warnings: self.core.diagnostics.warnings.len(),
        });
        Ok(None)
    }
}
impl EngineView<'_> {
    pub(super) fn cache_paragraph(&mut self, boxes: &[i64], penalties: &[i64], display: bool) {
        self.cache_material(boxes, penalties, display, None);
    }
    pub(super) fn cache_prepared_paragraph(&mut self) {
        if self.env.reuse_stateful.get() {
            self.finish_stateful_paragraph();
            return;
        }
        let bounded = self.core.reuse.active.as_ref().is_some_and(|capture| {
            self.core
                .input
                .input_file
                .first()
                .and_then(Option::as_ref)
                .and_then(|s| s.byte_position())
                .is_some_and(|end| end >= capture.start && end - capture.start <= SOURCE)
        });
        let context = &self.builder.context;
        if !bounded
            || context.cur_l_origins.len() > NODES
            || context.lig_origins.iter().map(Vec::len).sum::<usize>() > NODES
            || context
                .cur_item
                .as_ref()
                .is_some_and(|item| item.components.len() > NODES || item.origins.len() > NODES)
        {
            self.core.reuse.reject(self.env);
            return;
        }
        if self.builder.events.iter().any(|e| {
            matches!(
                e,
                crate::typesetting::nodes::BuilderEvent::MissingCharacter { .. }
            )
        }) {
            self.core.reuse.reject(self.env);
            return;
        }
        let context = self.builder.context.clone();
        let roots = if context.cur_list_head == C::NULL {
            vec![]
        } else {
            vec![context.cur_list_head]
        };
        self.cache_material(&roots, &[], false, Some(context));
    }
    fn cache_material(
        &mut self,
        boxes: &[i64],
        penalties: &[i64],
        display: bool,
        prepared: Option<ListContext>,
    ) {
        let Some(capture) = self.core.reuse.active.take() else {
            return;
        };
        #[cfg(feature = "latency-diagnostics")]
        let _capture = crate::latency_diagnostics::phase(
            "execution_reuse_capture",
            "paragraph",
            Some(crate::latency_diagnostics::region(
                self.core.input.source().as_ref(),
            )),
        );
        let values = self.env.values.finish_reads();
        let meanings = self.env.meanings.finish_reads();
        let expansions = self.env.reuse_expansions.take();
        let input = &self.core.input;
        let valid = input.recovery.count == 0
            && expansions.is_some_and(|(_, valid)| valid)
            && (prepared.is_some()
                || InputProcessor::parameter(self.env, "tracingparagraphs", 0) == 0)
            && !display
            && input.input_stack.len() >= capture.guard.base_depth
            && input.input_stack[capture.guard.base_depth..]
                .iter()
                .all(|frame| {
                    frame.index == 0 && frame.name.as_deref() == Some(capture.guard.path.as_str())
                })
            && (input.cursor.token_list.is_none()
                || input
                    .input_stack
                    .get(capture.guard.base_depth)
                    .is_some_and(|frame| frame.token_list.is_none()))
            && input.in_open == 1
            && input.cursor.name == capture.cursor.name
            && input.cursor.index == 0
            && input.scanner_status == ScannerStatus::Normal
            && (prepared.is_some() || input.current.is_command(Command::ParEnd))
            && self.env.cur_level == capture.guard.cur_level
            && self.env.cur_group == capture.guard.cur_group
            && self.fonts.current == capture.guard.font_number;
        let end = input
            .input_file
            .first()
            .and_then(Option::as_ref)
            .and_then(|s| s.byte_position())
            .unwrap_or(0);
        // Admission policy only: avoid retaining a newly edited computation
        // whose source may change again on the next keystroke. Existing entries
        // still undergo normal dependency validation and may hit after undo.
        if self
            .core
            .reuse
            .edited_offset
            .is_some_and(|offset| offset >= capture.start && offset < end)
        {
            return;
        }
        let Some((values, meanings, graph)) = valid
            .then(|| Some((values?, meanings?, Graph::capture(self.builder, boxes)?)))
            .flatten()
        else {
            self.core.reuse.stats.rejected += 1;
            return;
        };
        if end <= capture.start || end - capture.start > SOURCE {
            self.core.reuse.stats.rejected += 1;
            return;
        }
        let end_line = if input.cursor.token_list.is_none() {
            input.cursor.line
        } else {
            input.input_stack[capture.guard.base_depth].line
        };
        let end_stack = input.input_stack[capture.guard.base_depth..].to_vec();
        let mut symbols = std::mem::take(&mut *self.env.reuse_symbols.borrow_mut());
        // Definition equality must include the spelling of every token identity,
        // including pending macro-body tokens that have not yet been read.
        for meaning in meanings.values().flatten() {
            if let CommandOperand::Macro(definition) = &meaning.value {
                for token in definition.body.iter().chain(
                    definition
                        .parameters
                        .iter()
                        .flat_map(|p| p.prefix.iter().chain(p.delimiters.iter().flatten())),
                ) {
                    if let Some(cs) = token.control_sequence_id() {
                        symbols
                            .entry(cs)
                            .or_insert_with(|| self.env.symbol_key(cs, false));
                    }
                }
            }
        }
        if symbols.len() > 1024 {
            return;
        }
        let prepared = match prepared {
            Some(context) => match graph.portable_context(context) {
                Some(c) => Some(c),
                None => return,
            },
            None => None,
        };
        let mut entry = Entry {
            guard: capture.guard,
            source: capture.bytes[capture.start..end].to_vec(),
            end: input.cursor.clone(),
            end_stack,
            end_line,
            prepared,
            current: input.current.clone(),
            end_align: input.align_state,
            tokens: input.token_count - capture.tokens,
            expansions: self.core.expansion.expansion_steps - capture.expansions,
            macros: expansions.unwrap().0,
            symbols,
            values,
            meanings,
            graph,
            penalties: penalties.to_vec(),
            hsize: self.layout.hsize,
            badness: self.layout.last_badness(),
            hyphen_ready: self.hyphenation.ready,
            warnings: self.core.diagnostics.warnings[capture.warnings..].to_vec(),
            bytes: 0,
        };
        let serialized = bincode::serialized_size(&(entry.end.clone(), &entry.end_stack))
            .ok()
            .and_then(|s| bincode::serialized_size(&entry.values).ok().map(|v| s + v))
            .and_then(|v| {
                bincode::serialized_size(&entry.meanings)
                    .ok()
                    .map(|m| v + m)
            })
            .and_then(|v| {
                bincode::serialized_size(&entry.guard.font.font)
                    .ok()
                    .map(|f| v + f)
            })
            .and_then(|v| {
                entry.graph.nodes.iter().try_fold(v, |sum, n| {
                    bincode::serialized_size(n).ok().map(|size| sum + size)
                })
            });
        let Some(serialized) = serialized.and_then(|s| usize::try_from(s).ok()) else {
            return;
        };
        entry.bytes = serialized.saturating_mul(4)
            + entry.graph.bytes()
            + entry.source.len()
            + entry.symbols.len() * 256
            + 4096;
        let mut cache = self.core.reuse.cache.borrow_mut();
        if entry.bytes > BYTES {
            return;
        }
        while cache.entries.len() >= ENTRIES || cache.bytes + entry.bytes > BYTES {
            let old = cache.entries.pop_front().unwrap();
            cache.bytes -= old.bytes;
        }
        cache.bytes += entry.bytes;
        cache.entries.push_back(Arc::new(entry));
        self.core.reuse.stats.entries = cache.entries.len();
        self.core.reuse.stats.retained_bytes = cache.bytes;
        drop(cache);
        self.core
            .reuse
            .rebalance(self.core.config.stateful_execution_reuse);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typesetting::model::NodeFields;
    #[test]
    fn portable_graph_owns_every_edge_and_rebases_only_physical_call_sites() {
        let mut source = NodeBuilder::new(Default::default());
        let root = source.new_null_box().unwrap();
        let definition = Arc::new(SourceSpan::new("document.tex", 2, 8));
        let mut origin = SourceSpan::new("document.tex", 10, 5);
        origin.expansion = Some("macro:word".into());
        origin.call_site = Some(definition.clone());
        let child = source
            .allocate(Node::from(NodeFields {
                node_type: crate::core::enums::NodeKind::Char,
                char: 65,
                origins: vec![Some(Arc::new(origin))],
                ..Default::default()
            }))
            .unwrap();
        source.node_mut(root).unwrap().list_ptr = child;
        let graph = Graph::capture(&source, &[root]).unwrap();
        source.release(root, false).unwrap();
        assert_eq!(source.nodes.len(), 0);
        let mut target = NodeBuilder::new(Default::default());
        let unrelated = target.new_null_box().unwrap();
        let a = graph
            .materialize(&mut target, "document.tex", 10, 12, 3)
            .unwrap()[0];
        let b = graph
            .materialize(&mut target, "document.tex", 10, 12, 0)
            .unwrap()[0];
        assert_ne!(a, b);
        assert_ne!(a, unrelated);
        let ac = target.node(a).unwrap().list_ptr;
        let bc = target.node(b).unwrap().list_ptr;
        assert_ne!(ac, bc);
        assert_eq!(
            target.node(ac).unwrap().origins[0].as_ref().unwrap().line,
            13
        );
        assert_eq!(
            target.node(ac).unwrap().origins[0]
                .as_ref()
                .unwrap()
                .call_site
                .as_ref()
                .unwrap()
                .line,
            2
        );
        target.node_mut(ac).unwrap().char = 66;
        assert_eq!(target.node(bc).unwrap().char, 65);
        target.release(a, false).unwrap();
        assert!(target.node(bc).is_ok());
        target.release(b, false).unwrap();
        target.release(unrelated, false).unwrap();
        assert_eq!(target.nodes.len(), 0);
    }
    #[test]
    fn graph_effects_and_failed_import_cannot_leak_or_alias_nodes() {
        let mut builder = NodeBuilder::new(Default::default());
        let root = builder.new_null_box().unwrap();
        builder.node_mut(root).unwrap().payload = NodePayload::Mark {
            number: 0,
            tokens: Arc::from([]),
        };
        assert!(Graph::capture(&builder, &[root]).is_none());
        builder.node_mut(root).unwrap().payload = NodePayload::None;
        let child = builder.new_null_box().unwrap();
        builder.node_mut(root).unwrap().list_ptr = child;
        let graph = Graph::capture(&builder, &[root]).unwrap();
        let mut target = NodeBuilder::new(crate::core::capacities::Capacities {
            main_memory: Some(1),
            ..Default::default()
        });
        assert!(
            graph
                .materialize(&mut target, "document.tex", 0, 0, 0)
                .is_err()
        );
        assert_eq!(target.nodes.len(), 0);
        builder.node_mut(child).unwrap().link = root;
        assert!(Graph::capture(&builder, &[root]).is_none());
    }
    #[test]
    fn font_dependencies_include_microtype_and_dimension_changes() {
        let mut font = crate::fonts::model::FontInfo::default();
        let guard = FontGuard::new(&font);
        assert!(guard.matches(&font));
        font.parameter_overrides.insert(2, 100);
        assert!(!guard.matches(&font));
        let guard = FontGuard::new(&font);
        font.width_overrides.insert(65, 123);
        assert!(!guard.matches(&font));
        let guard = FontGuard::new(&font);
        font.micro_codes
            .borrow_mut()
            .entry(crate::fonts::microtypography::FontCode::Left)
            .or_default()
            .insert(65, 20);
        assert!(!guard.matches(&font));
    }
}
