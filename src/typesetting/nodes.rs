//! Node ownership and TeX ligature list construction (`typesetting/nodes.py`).
use super::ligature_cycles::PumpingGuard;
use super::model::{Node, NodePayload};
use crate::core::capacities::{Capacities, reached};
use crate::core::constants::Constants as C;
use crate::core::enums::{ListMode, NodeKind};
use crate::core::errors::{Error, Result};
use crate::core::persistence::PersistentComponent;
use crate::core::runtime::OwnershipRoots;
use crate::core::source::SourceSpan;
use crate::core::values::{GlueSpec, trunc_div};
use crate::fonts::catalog::FontCatalog;
use crate::fonts::ligatures::{LigatureAction, LigatureMatch};
use crate::fonts::microtypography::{FontCode, auto_kern, em_amount};
use crate::fonts::model::FontInfo;
use crate::fonts::unicode::ShapedGlyph;
use crate::language::records::{Quantity, ValueIndex, ValueKey};
use crate::language::state::{Environment, StateServices};
use crate::typesetting::model::NodeFields;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::sync::Arc;

// Node identities are normally a compact monotonic range. A bitmap avoids
// ordered-set allocation/search at every edge during the frequent box sweeps.
// Sparse or unusual restored identities keep bounded storage via a hash set.
enum ReachableNodes {
    Dense { first: i64, bits: Vec<u64> },
    Sparse(HashSet<i64>),
}
impl ReachableNodes {
    fn new(first: Option<i64>, last: Option<i64>) -> Self {
        if let (Some(first), Some(last)) = (first, last) {
            if let Some(span) = last.checked_sub(first).and_then(|n| n.checked_add(1)) {
                if span <= 8 * 1024 * 1024 {
                    return Self::Dense {
                        first,
                        bits: vec![0; (span as usize).div_ceil(64)],
                    };
                }
            }
        }
        Self::Sparse(HashSet::new())
    }
    fn insert(&mut self, pointer: i64) -> bool {
        match self {
            Self::Dense { first, bits } => {
                let Some(index) = pointer
                    .checked_sub(*first)
                    .and_then(|n| usize::try_from(n).ok())
                else {
                    return true;
                };
                // Outside the known range: the caller's existing node lookup
                // reports the unknown ownership root, without indexing memory.
                let Some(word) = bits.get_mut(index / 64) else {
                    return true;
                };
                let mask = 1 << (index % 64);
                let fresh = *word & mask == 0;
                *word |= mask;
                fresh
            }
            Self::Sparse(set) => set.insert(pointer),
        }
    }
    fn contains(&self, pointer: &i64) -> bool {
        match self {
            Self::Dense { first, bits } => pointer
                .checked_sub(*first)
                .and_then(|n| usize::try_from(n).ok())
                .is_some_and(|index| {
                    bits.get(index / 64)
                        .is_some_and(|word| word & (1 << (index % 64)) != 0)
                }),
            Self::Sparse(set) => set.contains(pointer),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LigatureItem {
    pub code: u32,
    pub components: Vec<u32>,
    pub origins: Vec<Arc<SourceSpan>>,
    pub ligature: bool,
    pub left_hit: bool,
    pub right_hit: bool,
    pub boundary: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListContext {
    pub mode: ListMode,
    pub restricted: bool,
    pub cur_list_head: i64,
    pub cur_list_tail: i64,
    pub cur_l: Option<u32>,
    pub cur_r: Option<u32>,
    pub cur_q: i64,
    pub lig_stack: VecDeque<LigatureItem>,
    pub cur_item: Option<LigatureItem>,
    pub cancel_boundary: bool,
    pub ligature_present: bool,
    pub lft_hit: bool,
    pub rt_hit: bool,
    pub cur_l_origins: Vec<Arc<SourceSpan>>,
    pub lig_origins: Vec<Vec<Arc<SourceSpan>>>,
    pub vertical_paragraph: bool,
    pub prev_depth: Option<i64>,
    pub prev_graf: i64,
    pub space_factor: i64,
    pub initial_hyphenation: (i64, i64, i64),
    pub current_language: i64,
    pub unicode_run: Vec<(u32, Option<Arc<SourceSpan>>)>,
}
impl Default for ListContext {
    fn default() -> Self {
        Self {
            mode: ListMode::Vertical,
            restricted: false,
            cur_list_head: C::NULL,
            cur_list_tail: C::NULL,
            cur_l: None,
            cur_r: None,
            cur_q: C::NULL,
            lig_stack: VecDeque::new(),
            cur_item: None,
            cancel_boundary: false,
            ligature_present: false,
            lft_hit: false,
            rt_hit: false,
            cur_l_origins: Vec::new(),
            lig_origins: Vec::new(),
            vertical_paragraph: false,
            prev_depth: None,
            prev_graf: 0,
            space_factor: 1000,
            initial_hyphenation: (0, 1, 1),
            current_language: 0,
            unicode_run: Vec::new(),
        }
    }
}
#[derive(Clone, Debug)]
pub enum BuilderEvent {
    MissingCharacter {
        font: usize,
        code: u32,
        source: Option<Arc<SourceSpan>>,
    },
    Ligature {
        left: u32,
        right: u32,
        result: u32,
        operation: Option<u8>,
        origins: Vec<Arc<SourceSpan>>,
    },
    ModePush {
        mode: ListMode,
        restricted: bool,
        depth: usize,
    },
    ModePop {
        mode: ListMode,
        restricted: bool,
        depth: usize,
    },
    DebugNode {
        pointer: i64,
        kind: NodeKind,
        character: Option<u32>,
        width: i64,
    },
}
const NODE_PAGE_SIZE: usize = 64;
#[derive(Clone)]
struct NodePage {
    slots: [Option<(i64, Option<Node>)>; NODE_PAGE_SIZE],
}
impl Default for NodePage {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
        }
    }
}
/// Bounded pages detach independently on mutation. Node identities, ordered
/// observations and the initialized map representation remain unchanged.
#[derive(Default)]
pub struct NodeTable {
    pub(crate) observer: Option<crate::checked_material::Observer>,
    pages: crate::core::persistence::SharedTable<i64, Arc<NodePage>>,
    len: usize,
}
impl Clone for NodeTable {
    fn clone(&self) -> Self {
        if let Some(o) = &self.observer {
            o.borrow_mut().unknown_iteration();
        }
        Self {
            pages: self.pages.clone(),
            len: self.len,
            observer: None,
        }
    }
}
struct NodeIterator<I> {
    inner: I,
    remaining: usize,
}
impl<I: Iterator> Iterator for NodeIterator<I> {
    type Item = I::Item;
    fn next(&mut self) -> Option<Self::Item> {
        let item = self.inner.next();
        if item.is_some() {
            self.remaining -= 1;
        }
        item
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl<I: DoubleEndedIterator> DoubleEndedIterator for NodeIterator<I> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let item = self.inner.next_back();
        if item.is_some() {
            self.remaining -= 1;
        }
        item
    }
}
impl<I: Iterator> ExactSizeIterator for NodeIterator<I> {}
impl std::fmt::Debug for NodeTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}
impl Serialize for NodeTable {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.len))?;
        for (key, node) in self.iter() {
            map.serialize_entry(key, node)?;
        }
        map.end()
    }
}
impl<'de> Deserialize<'de> for NodeTable {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let map = std::collections::BTreeMap::<i64, Option<Node>>::deserialize(deserializer)?;
        let mut table = Self::default();
        for (key, node) in map {
            table.insert(key, node);
        }
        Ok(table)
    }
}
impl NodeTable {
    /// Storage-only snapshot used by the existing memo validator. It does not
    /// certify the contents as unchanged. All later reads use a private fixed
    /// tag map and report unsupported observations to the live recipe, even
    /// after the live arena releases a node. No TeX continuation uses this arena.
    pub(crate) fn memo_snapshot(&self) -> Self {
        let observer = self.observer.as_ref().map(|parent| {
            let tags = parent.borrow().tags.clone();
            let mut evidence = crate::checked_material::Evidence::default();
            evidence.tags = tags;
            evidence.snapshot_parent = Some(parent.clone());
            std::rc::Rc::new(std::cell::RefCell::new(evidence))
        });
        Self {
            pages: self.pages.clone(),
            len: self.len,
            observer,
        }
    }
    fn address(key: i64) -> (i64, usize) {
        (
            key.div_euclid(NODE_PAGE_SIZE as i64),
            key.rem_euclid(NODE_PAGE_SIZE as i64) as usize,
        )
    }
    pub fn len(&self) -> usize {
        if let Some(o) = &self.observer {
            o.borrow_mut().unknown_iteration();
        }
        self.len
    }
    pub fn is_empty(&self) -> bool {
        if let Some(o) = &self.observer {
            o.borrow_mut().unknown_iteration();
        }
        self.len == 0
    }
    pub fn contains_key(&self, key: &i64) -> bool {
        self.get(key).is_some()
    }
    pub(crate) fn contract(
        &self,
        c: crate::checked_material::Consumer,
    ) -> crate::checked_material::Grant {
        crate::checked_material::Evidence::grant(&self.observer, c)
    }
    #[track_caller]
    fn observe(&self, id: i64, access: crate::checked_material::Consumer) {
        if let Some(o) = &self.observer {
            o.borrow_mut().access(id, access);
        }
    }
    fn get_inner(&self, key: &i64) -> Option<&Option<Node>> {
        let (page, slot) = Self::address(*key);
        self.pages.get(&page)?.slots[slot].as_ref().map(|(_, n)| n)
    }
    /// Native paint traversal only. Returning an owned node closes the grant
    /// before navigation, PDF or write callbacks can execute.
    pub(crate) fn paint_node(&self, id: i64) -> Result<Node> {
        self.observe(id, crate::checked_material::Consumer::Paint);
        self.get_inner(&id)
            .and_then(Option::as_ref)
            .cloned()
            .ok_or_else(|| Error::value("Missing paint node"))
    }
    /// Preserve the engine's first-matching effect identity lookup. Pure
    /// parameter material cannot match a PDF payload, regardless of glyphs or
    /// origins. All other observations keep the ordinary rejection boundary.
    pub(crate) fn effect_source(&self, target: &Node) -> Option<i64> {
        for page in self.pages.values() {
            for slot in &page.slots {
                let Some((id, Some(node))) = slot else {
                    continue;
                };
                if matches!(target.payload, NodePayload::Pdf(_))
                    && matches!(node.payload, NodePayload::None)
                {
                    self.observe(*id, crate::checked_material::Consumer::EffectIdentity);
                    continue;
                }
                self.observe(*id, crate::checked_material::Consumer::Raw);
                if node.whatsit == target.whatsit
                    && node.origins == target.origins
                    && std::mem::discriminant(&node.payload)
                        == std::mem::discriminant(&target.payload)
                {
                    return Some(*id);
                }
            }
        }
        None
    }
    pub(crate) fn tail_summary(&self, id: i64) -> Result<crate::checked_material::TailSummary> {
        let summary = crate::checked_material::TailSummary::new(
            self.get_inner(&id)
                .and_then(Option::as_ref)
                .ok_or_else(|| Error::value("Missing tail node"))?,
        );
        if let Some(o) = &self.observer {
            let mut e = o.borrow_mut();
            if let Some(tag) = e.tags.get(&id).copied() {
                if e.tail_open.contains(&tag.candidate) {
                    let reads = e.tail_reads.entry(tag.candidate).or_default();
                    if reads.len() < 64 {
                        reads.push(crate::checked_material::TailObservation::Summary(
                            summary.clone(),
                        ));
                    } else {
                        e.misses.insert(tag.candidate, "tail_observation_bound");
                    }
                }
            }
        }
        self.observe(id, crate::checked_material::Consumer::TailKind);
        Ok(summary)
    }
    pub(crate) fn tail_kind(&self, id: i64) -> Result<NodeKind> {
        let kind = self
            .get_inner(&id)
            .and_then(Option::as_ref)
            .ok_or_else(|| Error::value("Missing tail node"))?
            .node_type;
        if let Some(o) = &self.observer {
            let mut e = o.borrow_mut();
            if let Some(tag) = e.tags.get(&id).copied() {
                if e.tail_open.contains(&tag.candidate) {
                    let reads = e.tail_reads.entry(tag.candidate).or_default();
                    if reads.len() < 64 {
                        reads.push(crate::checked_material::TailObservation::Kind(kind));
                    } else {
                        e.misses.insert(tag.candidate, "tail_observation_bound");
                    }
                }
            }
        }
        self.observe(id, crate::checked_material::Consumer::TailKind);
        Ok(kind)
    }
    pub(crate) fn tail_link(&self, id: i64) -> Result<i64> {
        self.observe(id, crate::checked_material::Consumer::TailLinks);
        Ok(self
            .get_inner(&id)
            .and_then(Option::as_ref)
            .ok_or_else(|| Error::value("Missing tail link"))?
            .link)
    }
    pub(crate) fn set_tail_link(&mut self, id: i64, link: i64) -> Result<()> {
        self.observe(id, crate::checked_material::Consumer::TailLinks);
        self.get_mut_inner(&id)
            .and_then(Option::as_mut)
            .ok_or_else(|| Error::value("Missing tail link"))?
            .link = link;
        Ok(())
    }
    pub(crate) fn shape(&self, id: i64) -> Result<crate::checked_material::Shape> {
        self.observe(id, crate::checked_material::Consumer::Shape);
        let n = self
            .get_inner(&id)
            .and_then(Option::as_ref)
            .ok_or_else(|| Error::value("Missing shape node"))?;
        Ok(crate::checked_material::Shape::new(n))
    }
    pub(crate) fn link(&self, id: i64) -> Result<i64> {
        self.observe(id, crate::checked_material::Consumer::Link);
        Ok(self
            .get_inner(&id)
            .and_then(Option::as_ref)
            .ok_or_else(|| Error::value("Missing linked node"))?
            .link)
    }
    pub(crate) fn set_link(&mut self, id: i64, link: i64) -> Result<()> {
        self.observe(id, crate::checked_material::Consumer::Link);
        self.get_mut_inner(&id)
            .and_then(Option::as_mut)
            .ok_or_else(|| Error::value("Missing linked node"))?
            .link = link;
        Ok(())
    }
    #[track_caller]
    pub fn get(&self, key: &i64) -> Option<&Option<Node>> {
        self.observe(*key, crate::checked_material::Consumer::Raw);
        let (page, slot) = Self::address(*key);
        self.pages.get(&page)?.slots[slot]
            .as_ref()
            .map(|(_, node)| node)
    }
    #[track_caller]
    pub fn get_mut(&mut self, key: &i64) -> Option<&mut Option<Node>> {
        self.observe(*key, crate::checked_material::Consumer::Raw);
        self.get_mut_inner(key)
    }
    fn get_mut_inner(&mut self, key: &i64) -> Option<&mut Option<Node>> {
        let (page, slot) = Self::address(*key);
        #[cfg(feature = "latency-diagnostics")]
        if let Some(page) = self.pages.get(&page) {
            if Arc::strong_count(page) > 1 || Arc::weak_count(page) > 0 {
                crate::latency_diagnostics::count("cow_node_pages", 1);
                crate::latency_diagnostics::count(
                    "cow_node_page_record_bytes",
                    std::mem::size_of::<NodePage>(),
                );
            }
        }
        Arc::make_mut(self.pages.get_mut(&page)?).slots[slot]
            .as_mut()
            .map(|(_, node)| node)
    }
    pub fn insert(&mut self, key: i64, node: Option<Node>) -> Option<Option<Node>> {
        self.observe(key, crate::checked_material::Consumer::Raw);
        let (page, slot) = Self::address(key);
        let page = Arc::make_mut(
            self.pages
                .entry(page)
                .or_insert_with(|| Arc::new(NodePage::default())),
        );
        let old = page.slots[slot].replace((key, node));
        if old.is_none() {
            self.len += 1;
        }
        old.map(|(_, node)| node)
    }
    pub fn remove(&mut self, key: &i64) -> Option<Option<Node>> {
        self.observe(*key, crate::checked_material::Consumer::Raw);
        if let Some(o) = &self.observer {
            o.borrow_mut().tags.remove(key);
        }
        #[cfg(feature = "latency-diagnostics")]
        crate::local_update_audit::retired(*key);
        let (id, slot) = Self::address(*key);
        let page = Arc::make_mut(self.pages.get_mut(&id)?);
        let old = page.slots[slot].take();
        if old.is_some() {
            self.len -= 1;
        }
        if page.slots.iter().all(Option::is_none) {
            self.pages.remove(&id);
        }
        old.map(|(_, node)| node)
    }
    fn discard(&mut self, key: &i64) {
        self.remove(key);
    }
    pub fn iter(
        &self,
    ) -> impl DoubleEndedIterator<Item = (&i64, &Option<Node>)> + ExactSizeIterator {
        if let Some(o) = &self.observer {
            o.borrow_mut().unknown_iteration();
        }
        NodeIterator {
            remaining: self.len,
            inner: self.pages.values().flat_map(|page| {
                page.slots
                    .iter()
                    .filter_map(|slot| slot.as_ref().map(|(key, node)| (key, node)))
            }),
        }
    }
    pub fn values(&self) -> impl DoubleEndedIterator<Item = &Option<Node>> + ExactSizeIterator {
        self.iter().map(|(_, node)| node)
    }
    pub fn first_key_value(&self) -> Option<(&i64, &Option<Node>)> {
        self.iter().next()
    }
    pub fn last_key_value(&self) -> Option<(&i64, &Option<Node>)> {
        self.iter().next_back()
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&i64, &Option<Node>) -> bool) {
        if let Some(o) = &self.observer {
            o.borrow_mut().unknown_iteration();
        }
        let ids: Vec<_> = self.pages.keys().copied().collect();
        for id in ids {
            let page = &self.pages[&id];
            let mut remove = [false; NODE_PAGE_SIZE];
            let mut count = 0;
            for (slot, value) in page.slots.iter().enumerate() {
                if let Some((key, node)) = value {
                    if !keep(key, node) {
                        remove[slot] = true;
                        count += 1;
                    }
                }
            }
            if count == 0 {
                continue;
            }
            let page = Arc::make_mut(self.pages.get_mut(&id).unwrap());
            for (slot, remove) in remove.into_iter().enumerate() {
                if remove {
                    if let (Some(o), Some((key, _))) = (&self.observer, &page.slots[slot]) {
                        o.borrow_mut().tags.remove(key);
                    }
                    page.slots[slot] = None;
                }
            }
            self.len -= count;
            if page.slots.iter().all(Option::is_none) {
                self.pages.remove(&id);
            }
        }
    }
}
impl std::ops::Index<&i64> for NodeTable {
    type Output = Option<Node>;
    fn index(&self, key: &i64) -> &Self::Output {
        self.get(key).expect("Missing node identity")
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeSnapshot {
    pub nodes: NodeTable,
    pub node_ptr: i64,
}
pub struct NodeBuilder {
    pub nodes: NodeTable,
    pub node_limit: Option<usize>,
    pub peak_nodes: usize,
    pub reclaimed_nodes: usize,
    pub node_ptr: i64,
    pub context: ListContext,
    pub context_stack: Vec<ListContext>,
    pub source: Option<Arc<SourceSpan>>,
    pub ownership: OwnershipRoots,
    pub capacities: Capacities,
    pub events: Vec<BuilderEvent>,
    pub tracing: bool,
    pub recovery_glyphs: bool,
    pub recovery_finite_shrinkage: bool,
}
impl NodeBuilder {
    pub(crate) fn checkpoint_clone(&self) -> Self {
        let _read = self
            .nodes
            .contract(crate::checked_material::Consumer::Ownership);
        Self {
            nodes: self.nodes.clone(),
            node_limit: self.node_limit,
            peak_nodes: self.peak_nodes,
            reclaimed_nodes: self.reclaimed_nodes,
            node_ptr: self.node_ptr,
            context: self.context.clone(),
            context_stack: self.context_stack.clone(),
            source: self.source.clone(),
            ownership: OwnershipRoots::default(),
            capacities: self.capacities.clone(),
            events: self.events.clone(),
            tracing: self.tracing,
            recovery_glyphs: self.recovery_glyphs,
            recovery_finite_shrinkage: self.recovery_finite_shrinkage,
        }
    }

    pub fn new(capacities: Capacities) -> Self {
        Self {
            nodes: Default::default(),
            node_limit: capacities.main_memory,
            peak_nodes: 0,
            reclaimed_nodes: 0,
            node_ptr: 0,
            context: ListContext::default(),
            context_stack: Vec::new(),
            source: None,
            ownership: OwnershipRoots::default(),
            capacities,
            events: Vec::new(),
            tracing: false,
            recovery_glyphs: false,
            recovery_finite_shrinkage: false,
        }
    }
    #[track_caller]
    pub fn node(&self, pointer: i64) -> Result<&Node> {
        #[cfg(feature = "latency-diagnostics")]
        crate::local_update_audit::access(pointer, false);
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::copied_access(pointer, false);
        self.nodes
            .get(&pointer)
            .ok_or_else(|| Error::value(format!("Unknown node: {pointer}")))?
            .as_ref()
            .ok_or_else(|| Error::value("Uninitialized node"))
    }
    #[track_caller]
    pub fn node_mut(&mut self, pointer: i64) -> Result<&mut Node> {
        #[cfg(feature = "latency-diagnostics")]
        crate::local_update_audit::access(pointer, true);
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::copied_access(pointer, true);
        self.nodes
            .get_mut(&pointer)
            .ok_or_else(|| Error::value(format!("Unknown node: {pointer}")))?
            .as_mut()
            .ok_or_else(|| Error::value("Uninitialized node"))
    }
    pub fn allocate(&mut self, node: Node) -> Result<i64> {
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::count("allocated_node_records", 1);
        let pointer = self.get_node(1)?;
        self.nodes.insert(pointer, Some(node));
        Ok(pointer)
    }
    pub fn get_node(&mut self, size: usize) -> Result<i64> {
        let _capacity = self
            .nodes
            .contract(crate::checked_material::Consumer::Ownership);
        if size != 1 {
            return Err(Error::value("Node allocation is one record at a time"));
        }
        if reached(self.node_limit, self.nodes.len()) {
            return Err(Error::runtime(format!(
                "Out of node memory ({} live nodes)",
                self.nodes.len()
            )));
        }
        let pointer = self.node_ptr;
        self.node_ptr += 1;
        self.nodes.insert(pointer, None);
        self.peak_nodes = self.peak_nodes.max(self.nodes.len());
        Ok(pointer)
    }
    fn set_current(&mut self, item: Option<LigatureItem>) {
        self.context.cur_l = item.as_ref().map(|item| item.code);
        self.context.cur_l_origins = item
            .as_ref()
            .map_or_else(Vec::new, |item| item.origins.clone());
        self.context.cur_item = item;
    }
    pub fn handle_char(&mut self, code: u32, fonts: &FontCatalog, env: &Environment) -> Result<()> {
        let sf = integer(
            env,
            &ValueKey {
                table: "sfcode".into(),
                index: ValueIndex::Number(code as i64),
            },
            if (65..=90).contains(&code) { 999 } else { 1000 },
        );
        if sf != 0 {
            self.context.space_factor = if sf > 1000 && self.context.space_factor < 1000 {
                1000
            } else {
                sf
            };
        }
        let font = &fonts.items[fonts.current];
        if !font.has_character(code) {
            self.flush_lig_stack(fonts, env, false)?;
            self.events.push(BuilderEvent::MissingCharacter {
                font: fonts.current,
                code,
                source: self.source.clone(),
            });
            if self.recovery_glyphs && parameter(env, "tracinglostchars", 1) > 0 {
                let width = (font.get_parameter(6) / 2).max(C::UNITY * 3);
                let height = (font.size * 7 / 10).max(C::UNITY * 5);
                let source = self.source.as_ref().map(SourceSpan::without_range);
                let pointer=self.allocate(Node::from(NodeFields {node_type:NodeKind::Rule,width,height,origins:source.into_iter().map(Some).collect(),box_warning:Some(format!("Missing glyph U+{code:04X}: a placeholder marks the unavailable character").into()),..NodeFields::default()}))?;
                self.tail_append(pointer)?;
            }
            return Ok(());
        }
        if font.unicode_font.is_some() {
            self.context.unicode_run.push((code, self.source.clone()));
            return Ok(());
        }
        if self.context.cur_l.is_none() && self.context.lig_stack.is_empty() {
            self.context.cur_item = None;
            if font.boundary_label.is_some() && !self.context.cancel_boundary {
                self.set_current(Some(LigatureItem {
                    code: 256,
                    boundary: true,
                    left_hit: true,
                    ..LigatureItem::default()
                }));
            }
            self.context.cancel_boundary = false;
            if self.context.cur_item.is_none() {
                self.append_auto_kern(fonts, env, font, 256, code, C::NULL)?;
            }
        }
        self.context.lig_stack.push_back(LigatureItem {
            code,
            components: vec![code],
            origins: self.source.iter().cloned().collect(),
            ..LigatureItem::default()
        });
        self.process_lig_stack(fonts, env)
    }
    pub fn process_lig_stack(&mut self, fonts: &FontCatalog, env: &Environment) -> Result<()> {
        let mut guard = None;
        while !self.context.lig_stack.is_empty() {
            if self.context.cur_l.is_none() {
                let item = self.context.lig_stack.pop_front();
                self.set_current(item);
                continue;
            }
            let right = self.context.lig_stack[0].clone();
            let left = self.context.cur_l.unwrap();
            let font = &fonts.items[fonts.current];
            self.context.cur_r = Some(right.code);
            let lookup_right = if !right.boundary && right.code == font.false_boundary_char {
                256
            } else {
                right.code
            };
            let amount = auto_kern(
                font,
                left,
                right.code,
                parameter(env, "pdfprependkern", 0) > 0,
                parameter(env, "pdfappendkern", 0) > 0,
            )?;
            if amount != 0 {
                let character = self.output_cur_l(fonts, env)?;
                self.append_auto_kern(fonts, env, font, left, right.code, character)?;
                let item = self.context.lig_stack.pop_front();
                self.set_current(item);
                continue;
            }
            let Some(result) = try_lig_kern(font, left, lookup_right)? else {
                self.output_cur_l(fonts, env)?;
                let item = self.context.lig_stack.pop_front();
                self.set_current(item);
                continue;
            };
            if let Some(kern) = result.kern {
                self.output_cur_l(fonts, env)?;
                let pointer = self.new_kern(kern)?;
                self.tail_append(pointer)?;
                let item = self.context.lig_stack.pop_front();
                self.set_current(item);
            } else {
                let guard = guard.get_or_insert_with(|| PumpingGuard::new("ligature program"));
                guard.visit(
                    (fonts.current, left, right.code, right.boundary),
                    self.context.lig_stack.len(),
                )?;
                if self.apply_ligature_operation(result, fonts, env)? {
                    self.output_cur_l(fonts, env)?;
                    let item = self.context.lig_stack.pop_front();
                    self.set_current(item);
                }
            }
        }
        Ok(())
    }
    fn output_cur_l(&mut self, fonts: &FontCatalog, env: &Environment) -> Result<i64> {
        let Some(item) = self.context.cur_item.clone().filter(|item| !item.boundary) else {
            return Ok(C::NULL);
        };
        let pointer = self.new_char_node(fonts, fonts.current, item.code)?;
        self.node_mut(pointer)?.origins = item.origins.iter().cloned().map(Some).collect();
        if item.ligature {
            let node = self.node_mut(pointer)?;
            node.node_type = NodeKind::Ligature;
            node.components = item.components.clone();
            node.left_boundary = item.left_hit;
            node.right_boundary = item.right_hit;
            self.append_auto_kern(
                fonts,
                env,
                &fonts.items[fonts.current],
                256,
                item.code,
                C::NULL,
            )?;
        }
        self.tail_append(pointer)?;
        if item
            .components
            .last()
            .is_some_and(|&code| code as i64 == fonts.items[fonts.current].hyphenchar)
            && self.context.mode == ListMode::Horizontal
            && !self.context.restricted
        {
            let disc = self.allocate(Node::from(NodeFields {
                node_type: NodeKind::Disc,
                origins: item.origins.into_iter().map(Some).collect(),
                ..NodeFields::default()
            }))?;
            self.tail_append(disc)?;
        }
        Ok(pointer)
    }
    fn apply_ligature_operation(
        &mut self,
        matched: LigatureMatch,
        fonts: &FontCatalog,
        env: &Environment,
    ) -> Result<bool> {
        let character = matched
            .character
            .ok_or_else(|| Error::value("Missing ligature character"))?;
        let behavior = matched
            .operation
            .ok_or_else(|| Error::value("Missing ligature operation"))?
            .behavior();
        let left = self.context.cur_item.clone().unwrap();
        let right = self.context.lig_stack[0].clone();
        let mut seen = HashSet::new();
        let origins: Vec<_> = left
            .origins
            .iter()
            .chain(&right.origins)
            .filter(|origin| seen.insert((*origin).clone()))
            .cloned()
            .collect();
        if self.tracing {
            self.events.push(BuilderEvent::Ligature {
                left: left.code,
                right: right.code,
                result: character,
                operation: matched.tfm_opcode,
                origins: origins.clone(),
            });
        }
        let ligature = |components| LigatureItem {
            code: character,
            components,
            origins: origins.clone(),
            ligature: true,
            left_hit: left.left_hit || right.left_hit,
            right_hit: left.right_hit || right.right_hit,
            boundary: false,
        };
        match behavior.action {
            LigatureAction::ReplaceBoth => {
                let mut components = left.components.clone();
                components.extend_from_slice(&right.components);
                self.set_current(Some(ligature(components)));
                self.context.lig_stack.pop_front();
            }
            LigatureAction::ReplaceLeft => {
                self.set_current(Some(ligature(left.components.clone())))
            }
            LigatureAction::ReplaceRight => {
                self.context.lig_stack[0] = ligature(right.components.clone())
            }
            LigatureAction::InsertBetween if behavior.pass_left_character => {
                self.output_cur_l(fonts, env)?;
                self.set_current(Some(ligature(Vec::new())));
            }
            LigatureAction::InsertBetween => {
                self.context.lig_stack.push_front(ligature(Vec::new()))
            }
        }
        Ok(behavior.finish_text_pair)
    }
    pub fn flush_unicode_run(&mut self, fonts: &FontCatalog) -> Result<()> {
        let run = std::mem::take(&mut self.context.unicode_run);
        let font_id = fonts.current;
        let font = &fonts.items[font_id];
        let program = font
            .unicode_font
            .as_ref()
            .ok_or_else(|| Error::value("Pending Unicode run requires a Unicode font"))?;
        let text: String = run
            .iter()
            .map(|(code, _)| char::from_u32(*code).unwrap())
            .collect();
        for glyph in program.shape(&text, !font.ligatures_disabled)?.iter() {
            let mut origins = Vec::new();
            for (_, origin) in &run[glyph.cluster_start..glyph.cluster_end] {
                if let Some(origin) = origin {
                    if !origins.iter().any(|other| other == origin) {
                        origins.push(origin.clone());
                    }
                }
            }
            let pointer = self.new_shaped_node(font_id, glyph.clone(), origins.clone())?;
            self.tail_append(pointer)?;
            if char::from_u32(font.hyphenchar as u32)
                .is_some_and(|hyphen| glyph.text == hyphen.to_string())
                && !self.context.restricted
            {
                let disc = self.allocate(Node::from(NodeFields {
                    node_type: NodeKind::Disc,
                    origins: origins.into_iter().map(Some).collect(),
                    ..NodeFields::default()
                }))?;
                self.tail_append(disc)?;
            }
        }
        Ok(())
    }
    pub fn new_shaped_node(
        &mut self,
        font: usize,
        glyph: ShapedGlyph,
        origins: Vec<Arc<SourceSpan>>,
    ) -> Result<i64> {
        let components: Vec<_> = glyph.text.chars().map(|ch| ch as u32).collect();
        let node = Node::from(NodeFields {
            node_type: NodeKind::Ligature,
            font,
            char: components.first().copied().unwrap_or(0),
            components,
            origins: origins.into_iter().map(Some).collect(),
            width: glyph.advance_sp,
            height: (glyph.ink_sp[3] + glyph.y_offset_sp).max(0),
            depth: (-glyph.ink_sp[1] - glyph.y_offset_sp).max(0),
            shaped: Some(Arc::new(glyph)),
            ..NodeFields::default()
        });
        self.allocate(node)
    }
    pub fn flush_lig_stack(
        &mut self,
        fonts: &FontCatalog,
        env: &Environment,
        right_boundary: bool,
    ) -> Result<()> {
        if !self.context.unicode_run.is_empty() {
            return self.flush_unicode_run(fonts);
        }
        self.process_lig_stack(fonts, env)?;
        let font = &fonts.items[fonts.current];
        if self.context.cur_item.is_some() && right_boundary && font.boundary_char < 256 {
            self.context.lig_stack.push_back(LigatureItem {
                code: font.boundary_char,
                boundary: true,
                right_hit: true,
                ..LigatureItem::default()
            });
            self.process_lig_stack(fonts, env)?;
        }
        let last = self.context.cur_item.clone();
        let character = self.output_cur_l(fonts, env)?;
        if let Some(last) = last.filter(|last| !last.boundary && right_boundary) {
            self.append_auto_kern(fonts, env, font, last.code, 256, character)?;
        }
        self.set_current(None);
        self.context.lig_origins.clear();
        Ok(())
    }
    pub fn append_auto_kern(
        &mut self,
        fonts: &FontCatalog,
        env: &Environment,
        font: &FontInfo,
        left: u32,
        right: u32,
        after: i64,
    ) -> Result<()> {
        let amount = auto_kern(
            font,
            left,
            right,
            parameter(env, "pdfprependkern", 0) > 0,
            parameter(env, "pdfappendkern", 0) > 0,
        )?;
        if amount != 0 {
            let pointer = self.new_kern(amount)?;
            let node = self.node_mut(pointer)?;
            node.kern_kind = "auto".into();
            node.payload = NodePayload::CharacterPolicy {
                font: fonts.current,
                code: left,
            };
            let following = if after != C::NULL {
                self.node(after)?.link
            } else {
                C::NULL
            };
            if following != C::NULL && self.node(following)?.node_type == NodeKind::Disc {
                self.node_mut(pointer)?.link = following;
                self.node_mut(after)?.link = pointer;
            } else {
                self.tail_append(pointer)?;
            }
        }
        Ok(())
    }
    pub fn handle_space(
        &mut self,
        fonts: &FontCatalog,
        env: &Environment,
        explicit: bool,
    ) -> Result<()> {
        self.flush_lig_stack(fonts, env, true)?;
        let font = &fonts.items[fonts.current];
        let adjusted = parameter(env, "pdfadjustinterwordglue", 0) > 0;
        let factor = if explicit || adjusted {
            1000
        } else {
            self.context.space_factor
        };
        let zero = GlueSpec::default();
        let mut spec = if factor >= 2000 {
            glue_parameter(env, "xspaceskip")
        } else {
            zero
        };
        if spec == zero {
            spec = glue_parameter(env, "spaceskip");
            if spec == zero {
                spec = GlueSpec {
                    width: font.get_parameter(2),
                    stretch: font.get_parameter(3),
                    shrink: font.get_parameter(4),
                    ..zero
                };
            }
            spec = GlueSpec {
                width: spec.width
                    + if factor >= 2000 {
                        font.get_parameter(7)
                    } else {
                        0
                    },
                stretch: trunc_div(spec.stretch as i128 * factor as i128, 1000)? as i64,
                shrink: trunc_div(spec.shrink as i128 * 1000, factor as i128)? as i64,
                ..spec
            };
        }
        if adjusted && self.context.cur_list_tail != C::NULL {
            let previous = self.node(self.context.cur_list_tail)?;
            let character = if previous.glyph() {
                Some((previous.font, previous.char))
            } else if previous.node_type == NodeKind::Kern && previous.kern_kind == "auto" {
                if let NodePayload::CharacterPolicy { font, code } = previous.payload {
                    Some((font, code))
                } else {
                    None
                }
            } else {
                None
            };
            if let Some((number, code)) = character {
                let owner = &fonts.items[number];
                spec.width += em_amount(owner, FontCode::SpaceWidth, code)?;
                spec.stretch += em_amount(owner, FontCode::SpaceStretch, code)?;
                spec.shrink += em_amount(owner, FontCode::SpaceShrink, code)?;
            }
        }
        let pointer = self.new_glue(spec, "normal", fonts.current)?;
        self.tail_append(pointer)
    }
    pub fn new_kern(&mut self, width: i64) -> Result<i64> {
        self.allocate(Node::from(NodeFields {
            node_type: NodeKind::Kern,
            width,
            ..NodeFields::default()
        }))
    }
    pub fn new_char_node(&mut self, fonts: &FontCatalog, font: usize, char: u32) -> Result<i64> {
        let selected = &fonts.items[font];
        let width = selected.get_char_width(char);
        let pointer = self.allocate(Node::from(NodeFields {
            node_type: NodeKind::Char,
            font,
            char,
            width,
            height: selected.get_char_height(char),
            depth: selected.get_char_depth(char),
            ..NodeFields::default()
        }))?;
        if self.tracing {
            self.events.push(BuilderEvent::DebugNode {
                pointer,
                kind: NodeKind::Char,
                character: Some(char),
                width,
            });
        }
        Ok(pointer)
    }
    pub fn new_null_box(&mut self) -> Result<i64> {
        let pointer = self.allocate(Node::default())?;
        if self.tracing {
            self.events.push(BuilderEvent::DebugNode {
                pointer,
                kind: NodeKind::Hlist,
                character: None,
                width: 0,
            });
        }
        Ok(pointer)
    }
    pub fn new_glue(&mut self, spec: GlueSpec, kind: &str, font: usize) -> Result<i64> {
        let mut spec = spec;
        if self.recovery_finite_shrinkage {
            spec.shrink_order = 0;
        }
        let pointer = self.allocate(Node::from(NodeFields {
            node_type: NodeKind::Glue,
            glue: Some(spec),
            glue_kind: kind.into(),
            font,
            width: spec.width,
            ..NodeFields::default()
        }))?;
        if self.tracing {
            self.events.push(BuilderEvent::DebugNode {
                pointer,
                kind: NodeKind::Glue,
                character: None,
                width: spec.width,
            });
        }
        Ok(pointer)
    }
    pub fn tail_append(&mut self, pointer: i64) -> Result<()> {
        if self.nodes.link(pointer)? != C::NULL {
            return Err(Error::value(
                "Appending a node that still owns sibling links",
            ));
        }
        if pointer == self.context.cur_list_tail {
            return Err(Error::value("Appending an already owned node"));
        }
        if self.context.cur_list_tail == C::NULL {
            self.context.cur_list_head = pointer;
        } else {
            let tail = self.context.cur_list_tail;
            if self.nodes.get_inner(&tail).is_some_and(Option::is_some) {
                self.nodes.set_link(tail, pointer)?;
            }
        }
        self.context.cur_list_tail = pointer;
        Ok(())
    }
    pub fn append_list(&mut self, head: i64) -> Result<()> {
        let mut pointer = head;
        let mut seen = BTreeSet::new();
        while pointer != C::NULL {
            if !seen.insert(pointer) {
                return Err(Error::value("Cycle in appended list"));
            }
            let next = self.nodes.link(pointer)?;
            self.nodes.set_link(pointer, C::NULL)?;
            self.tail_append(pointer)?;
            pointer = next;
        }
        Ok(())
    }
    pub fn take_list(&mut self, fonts: &FontCatalog, env: &Environment) -> Result<i64> {
        self.flush_lig_stack(fonts, env, true)?;
        let head = self.context.cur_list_head;
        self.context.cur_list_head = C::NULL;
        self.context.cur_list_tail = C::NULL;
        Ok(head)
    }
    pub fn release(&mut self, pointer: i64, siblings: bool) -> Result<()> {
        let _grant = self
            .nodes
            .contract(crate::checked_material::Consumer::Ownership);
        let mut pending = vec![(pointer, siblings)];
        let mut seen = BTreeSet::new();
        while let Some((pointer, follow)) = pending.pop() {
            if pointer == C::NULL {
                continue;
            }
            if !seen.insert(pointer) {
                return Err(Error::value("Shared or cyclic node ownership"));
            }
            let node = self
                .nodes
                .remove(&pointer)
                .ok_or_else(|| Error::value("Unknown node"))?
                .ok_or_else(|| Error::value("Uninitialized node"))?;
            self.reclaimed_nodes += 1;
            if follow {
                pending.push((node.link, true));
            }
            pending.extend([
                (node.list_ptr, true),
                (node.pre_break, true),
                (node.post_break, true),
                (node.leader_ptr, false),
            ]);
        }
        Ok(())
    }
    pub fn context_roots(&self) -> Vec<i64> {
        std::iter::once(&self.context)
            .chain(&self.context_stack)
            .map(|context| context.cur_list_head)
            .collect()
    }
    pub fn reclaim_unreachable(&mut self, roots: &[i64]) -> Result<()> {
        let _grant = self
            .nodes
            .contract(crate::checked_material::Consumer::Ownership);
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase("node_reclamation", "", None);
        let mut pending = roots.to_vec();
        pending.extend(self.context_roots());
        pending.extend(
            self.ownership
                .collect_live(|root| self.nodes.contains_key(&root)),
        );
        let first = self.nodes.first_key_value().map(|(key, _)| *key);
        let last = self.nodes.last_key_value().map(|(key, _)| *key);
        let mut live = ReachableNodes::new(first, last);
        // Compact arenas can index graph edges directly. Scratch storage stays
        // below 65536 pointers; sparse/unusual identities keep ordered lookup.
        let dense = first.zip(last).and_then(|(first, last)| {
            let span = last.checked_sub(first)?.checked_add(1)?;
            let span = usize::try_from(span).ok()?;
            if span > 65536 || span > self.nodes.len().saturating_mul(4) {
                return None;
            }
            let mut nodes = vec![None; span];
            for (&pointer, node) in self.nodes.iter() {
                nodes[(pointer - first) as usize] = Some(node);
            }
            Some((first, nodes))
        });
        while let Some(pointer) = pending.pop() {
            if pointer == C::NULL || !live.insert(pointer) {
                continue;
            }
            let node = if let Some((first, nodes)) = &dense {
                pointer
                    .checked_sub(*first)
                    .and_then(|n| usize::try_from(n).ok())
                    .and_then(|index| nodes.get(index))
                    .copied()
                    .flatten()
            } else {
                self.nodes.get(&pointer)
            }
            .ok_or_else(|| Error::value("Unknown ownership root"))?;
            if let Some(node) = node {
                pending.extend([
                    node.link,
                    node.list_ptr,
                    node.pre_break,
                    node.post_break,
                    node.leader_ptr,
                ]);
            }
        }
        drop(dense);
        let previous = self.nodes.len();
        self.nodes.retain(|pointer, _| live.contains(pointer));
        self.reclaimed_nodes += previous - self.nodes.len();
        Ok(())
    }
    pub fn siblings(&self, head: i64) -> Result<Vec<i64>> {
        let mut pointers = Vec::new();
        let mut seen = BTreeSet::new();
        let mut pointer = head;
        while pointer != C::NULL {
            if !seen.insert(pointer) {
                return Err(Error::value("Cycle in node list"));
            }
            pointers.push(pointer);
            pointer = self.nodes.link(pointer)?;
        }
        Ok(pointers)
    }
    pub fn push_context(
        &mut self,
        fonts: &FontCatalog,
        env: &Environment,
        mode: ListMode,
        restricted: bool,
    ) -> Result<()> {
        self.flush_lig_stack(fonts, env, true)?;
        if reached(self.capacities.nest_size, self.context_stack.len()) {
            return Err(Error::value("Semantic nest overflow")
                .with_issue(crate::core::recovery::Issue::Capacity));
        }
        let previous = std::mem::replace(
            &mut self.context,
            ListContext {
                mode,
                restricted,
                ..ListContext::default()
            },
        );
        self.context_stack.push(previous);
        if self.tracing {
            self.events.push(BuilderEvent::ModePush {
                mode,
                restricted,
                depth: self.context_stack.len(),
            });
        }
        Ok(())
    }
    pub fn pop_context(&mut self) -> Result<()> {
        if self.context.cur_list_head != C::NULL
            || self.context.cur_l.is_some()
            || !self.context.unicode_run.is_empty()
        {
            return Err(Error::value(
                "Cannot pop a context that still owns material",
            ));
        }
        self.context = self
            .context_stack
            .pop()
            .ok_or_else(|| Error::value("Empty semantic nest"))?;
        if self.tracing {
            self.events.push(BuilderEvent::ModePop {
                mode: self.context.mode,
                restricted: self.context.restricted,
                depth: self.context_stack.len(),
            });
        }
        Ok(())
    }
    pub fn copy_box(&mut self, pointer: i64) -> Result<i64> {
        let _grant = self.nodes.contract(crate::checked_material::Consumer::Copy);
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase(
            "box_copy",
            &format!(
                "root={pointer} {:?}",
                self.node(pointer).ok().map(|n| (
                    n.node_type,
                    n.width,
                    n.height,
                    n.depth,
                    n.list_ptr
                ))
            ),
            None,
        );
        #[cfg(feature = "latency-diagnostics")]
        crate::output_diagnostics::copy_begin();
        if pointer == C::NULL {
            return Ok(pointer);
        }
        let mut allocated = Vec::new();
        let mut seen = BTreeSet::new();
        let mut pending = vec![(pointer, None, ChildLink::List, false)];
        let mut root = C::NULL;
        let result = (|| -> Result<()> {
            while let Some((source, parent, attribute, siblings)) = pending.pop() {
                if !seen.insert(source) {
                    return Err(Error::value("Cycle or shared ownership in box contents"));
                }
                let mut copied = self
                    .node(source)
                    .map_err(|_| Error::value("Uninitialized node in box contents"))?
                    .clone();
                let links = [
                    copied.link,
                    copied.leader_ptr,
                    copied.post_break,
                    copied.pre_break,
                    copied.list_ptr,
                ];
                copied.link = C::NULL;
                copied.list_ptr = C::NULL;
                copied.pre_break = C::NULL;
                copied.post_break = C::NULL;
                copied.leader_ptr = C::NULL;
                let result = self.allocate(copied)?;
                if let Some(o) = &self.nodes.observer {
                    o.borrow_mut().copied(source, result);
                }
                #[cfg(feature = "latency-diagnostics")]
                crate::local_update_audit::copied(source, result);
                #[cfg(feature = "latency-diagnostics")]
                crate::latency_diagnostics::copied_node(
                    source,
                    result,
                    self.nodes[&source].as_ref().unwrap(),
                );
                #[cfg(feature = "latency-diagnostics")]
                crate::output_diagnostics::copied(result, self.nodes[&source].as_ref().unwrap());
                allocated.push(result);
                if let Some(parent) = parent {
                    attribute.set(self.node_mut(parent)?, result);
                } else {
                    root = result;
                }
                if siblings && links[0] != C::NULL {
                    pending.push((links[0], Some(result), ChildLink::Sibling, true));
                }
                for (attribute, child) in [
                    (ChildLink::Leader, links[1]),
                    (ChildLink::Post, links[2]),
                    (ChildLink::Pre, links[3]),
                    (ChildLink::List, links[4]),
                ] {
                    if child != C::NULL {
                        pending.push((
                            child,
                            Some(result),
                            attribute,
                            !matches!(attribute, ChildLink::Leader),
                        ));
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            for pointer in &allocated {
                self.nodes.discard(pointer);
            }
            self.reclaimed_nodes += allocated.len();
            return Err(error);
        }
        #[cfg(feature = "latency-diagnostics")]
        {
            crate::output_diagnostics::count("box_nodes_cloned", allocated.len());
            crate::output_diagnostics::copy_end();
        }
        Ok(root)
    }
}
#[derive(Clone, Copy)]
enum ChildLink {
    Sibling,
    List,
    Pre,
    Post,
    Leader,
}
impl ChildLink {
    fn set(self, node: &mut Node, pointer: i64) {
        match self {
            Self::Sibling => node.link = pointer,
            Self::List => node.list_ptr = pointer,
            Self::Pre => node.pre_break = pointer,
            Self::Post => node.post_break = pointer,
            Self::Leader => node.leader_ptr = pointer,
        }
    }
}
pub fn try_lig_kern(font: &FontInfo, left: u32, right: u32) -> Result<Option<LigatureMatch>> {
    let mut index = if left == 256 {
        font.boundary_label
    } else {
        font.tfm_lig_start
            .as_ref()
            .and_then(|starts| starts.get(&left))
            .copied()
    };
    let Some(mut index) = index.take() else {
        return Ok(None);
    };
    let Some(program) = &font.tfm_lig_kern else {
        return Ok(None);
    };
    if index >= program.len() {
        return Ok(None);
    }
    if let Some(restart) = program[index].restart_address.filter(|_| left != 256) {
        index = restart;
        if index >= program.len() {
            return Err(Error::value(
                "Ligature restart address outside font program",
            ));
        }
    }
    while index < program.len() {
        let instruction = &program[index];
        if instruction.next_character == Some(right) {
            if let Some(kern) = instruction.kern_index {
                if let Some(&kern) = font.tfm_kerns.as_ref().and_then(|kerns| kerns.get(kern)) {
                    return Ok(Some(LigatureMatch {
                        kern: Some(kern),
                        tfm_opcode: instruction.tfm_opcode,
                        ..LigatureMatch::default()
                    }));
                }
            } else {
                return Ok(Some(LigatureMatch {
                    character: instruction.replacement,
                    operation: instruction.operation,
                    tfm_opcode: instruction.tfm_opcode,
                    ..LigatureMatch::default()
                }));
            }
        }
        let Some(next) = instruction.next_instruction else {
            break;
        };
        if next <= index {
            return Err(Error::value("Nonincreasing ligature instruction successor"));
        }
        index = next;
    }
    Ok(None)
}
pub fn integer(env: &Environment, key: &ValueKey, default: i64) -> i64 {
    match env.values.get(key) {
        Some(Quantity::Integer(value)) => *value,
        _ => default,
    }
}
pub fn parameter(env: &Environment, name: &str, default: i64) -> i64 {
    integer(env, &ValueKey::parameter(name), default)
}
pub fn glue_parameter(env: &Environment, name: &str) -> GlueSpec {
    match env.values.get(&ValueKey::parameter(name)) {
        Some(Quantity::Glue(glue)) => *glue,
        _ => GlueSpec::default(),
    }
}
impl PersistentComponent for NodeBuilder {
    type State = NodeSnapshot;
    fn dump_state(&self) -> Self::State {
        NodeSnapshot {
            nodes: self.nodes.clone(),
            node_ptr: self.node_ptr,
        }
    }
    fn restore_state(&mut self, state: Self::State) -> Result<()> {
        self.nodes = state.nodes;
        self.node_ptr = state.node_ptr;
        Ok(())
    }
}
/// Environment assignments pass arena operations explicitly, preserving roots.
pub struct ArenaStateServices<'a> {
    pub builder: &'a mut NodeBuilder,
    pub error: Option<Error>,
    pub restorations: Vec<crate::language::groups::RestorationKey>,
}
impl StateServices for ArenaStateServices<'_> {
    fn copy_box(&mut self, pointer: i64) -> Result<i64> {
        self.builder.copy_box(pointer)
    }
    fn reclaim_unreachable(&mut self, roots: &[i64]) {
        if let Err(error) = self.builder.reclaim_unreachable(roots) {
            self.error = Some(error);
        }
    }
    fn restored(&mut self, key: &crate::language::groups::RestorationKey) {
        self.restorations.push(key.clone());
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;

    #[test]
    fn copied_graphs_keep_independent_links_payloads_and_reclamation() {
        let mut builder = NodeBuilder::new(Capacities::default());
        let root = builder.new_null_box().unwrap();
        let child = builder.new_null_box().unwrap();
        let next = builder.new_null_box().unwrap();
        let leader = builder.new_null_box().unwrap();
        let pre = builder.new_null_box().unwrap();
        let post = builder.new_null_box().unwrap();
        builder.node_mut(root).unwrap().list_ptr = child;
        builder.node_mut(child).unwrap().link = next;
        builder.node_mut(child).unwrap().leader_ptr = leader;
        builder.node_mut(next).unwrap().pre_break = pre;
        builder.node_mut(next).unwrap().post_break = post;
        builder.node_mut(child).unwrap().components = vec![65, 66];
        let origin = Arc::new(SourceSpan::new("copy.tex", 7, 3));
        builder.node_mut(child).unwrap().origins = smallvec::smallvec![Some(origin.clone())];
        builder.node_mut(child).unwrap().payload =
            NodePayload::Pdf(super::super::model::PdfEffect::SetMatrix("1 0 0 1".into()));
        let copied = builder.copy_box(root).unwrap();
        let copied_child = builder.node(copied).unwrap().list_ptr;
        let copied_next = builder.node(copied_child).unwrap().link;
        assert!(Arc::ptr_eq(
            builder.node(copied_child).unwrap().origins[0]
                .as_ref()
                .unwrap(),
            &origin,
        ));
        assert_ne!(copied_child, child);
        assert_ne!(builder.node(copied_child).unwrap().leader_ptr, leader);
        assert_ne!(builder.node(copied_next).unwrap().pre_break, pre);
        assert_ne!(builder.node(copied_next).unwrap().post_break, post);
        // Both the original and its copy may be changed independently.
        builder.node_mut(child).unwrap().components.push(67);
        builder.node_mut(copied_child).unwrap().width = 123;
        if let NodePayload::Pdf(super::super::model::PdfEffect::SetMatrix(value)) =
            &mut builder.node_mut(copied_child).unwrap().payload
        {
            value.push_str(" changed");
        } else {
            panic!("Copied payload missing");
        }
        assert_eq!(builder.node(copied_child).unwrap().components, [65, 66]);
        assert_eq!(builder.node(child).unwrap().width, 0);
        let NodePayload::Pdf(super::super::model::PdfEffect::SetMatrix(value)) =
            &builder.node(child).unwrap().payload
        else {
            panic!("Original payload missing")
        };
        assert_eq!(value, "1 0 0 1");
        builder.release(root, false).unwrap();
        assert_eq!(builder.nodes.len(), 6);
        builder.reclaim_unreachable(&[copied]).unwrap();
        assert_eq!(builder.nodes.len(), 6);
        builder.release(copied, false).unwrap();
        assert!(builder.nodes.is_empty());
    }

    #[test]
    fn failed_box_copy_discards_partial_allocations_and_preserves_source() {
        let mut builder = NodeBuilder::new(Capacities::default());
        let root = builder.new_null_box().unwrap();
        let child = builder.new_null_box().unwrap();
        builder.node_mut(root).unwrap().list_ptr = child;
        builder.node_limit = Some(3);
        assert!(builder.copy_box(root).is_err());
        assert_eq!(builder.nodes.len(), 2);
        assert_eq!(builder.node(root).unwrap().list_ptr, child);
        builder.node_limit = None;
        // Malformed multiply-owned input remains an error, with no leaked copy.
        builder.node_mut(root).unwrap().leader_ptr = child;
        assert!(builder.copy_box(root).is_err());
        assert_eq!(builder.nodes.len(), 2);
        builder.node_mut(root).unwrap().leader_ptr = C::NULL;
        builder.release(root, false).unwrap();
        assert!(builder.nodes.is_empty());
    }

    #[test]
    fn compact_reclamation_detects_missing_edges_and_extreme_roots_without_partial_drop() {
        let mut builder = NodeBuilder::new(Capacities::default());
        builder.nodes.insert(
            1,
            Some(Node::from(NodeFields {
                link: 3,
                ..NodeFields::default()
            })),
        );
        builder.nodes.insert(
            4,
            Some(Node::from(NodeFields {
                link: 1,
                ..NodeFields::default()
            })),
        );
        assert!(builder.reclaim_unreachable(&[1]).is_err());
        assert_eq!(builder.nodes.len(), 2);
        builder.node_mut(1).unwrap().link = 4;
        builder.reclaim_unreachable(&[1]).unwrap();
        assert_eq!(builder.nodes.len(), 2);
        for bad in [0, i64::MIN, i64::MAX] {
            assert!(builder.reclaim_unreachable(&[bad]).is_err());
            assert_eq!(builder.nodes.len(), 2);
        }
    }
    #[test]
    fn node_tables_detach_only_changed_pages_and_preserve_legacy_format_bytes() {
        let mut table = NodeTable::default();
        table.insert(
            1,
            Some(Node::from(NodeFields {
                width: 10,
                ..NodeFields::default()
            })),
        );
        table.insert(
            129,
            Some(Node::from(NodeFields {
                width: 20,
                ..NodeFields::default()
            })),
        );
        let snapshot = table.clone();
        let legacy: std::collections::BTreeMap<i64, Option<Node>> =
            std::collections::BTreeMap::from([
                (1, snapshot[&1].clone()),
                (129, snapshot[&129].clone()),
            ]);
        assert_eq!(
            bincode::serialize(&table).unwrap(),
            bincode::serialize(&legacy).unwrap()
        );
        let restored: NodeTable =
            bincode::deserialize(&bincode::serialize(&legacy).unwrap()).unwrap();
        assert_eq!(restored[&1].as_ref().unwrap().width, 10);
        table.get_mut(&1).unwrap().as_mut().unwrap().width = 11;
        assert_eq!(snapshot[&1].as_ref().unwrap().width, 10);
        assert!(!Arc::ptr_eq(&table.pages[&0], &snapshot.pages[&0]));
        assert!(Arc::ptr_eq(&table.pages[&2], &snapshot.pages[&2]));
        table.discard(&129);
        assert!(snapshot.contains_key(&129));
        table.insert(1, None);
        assert!(table[&1].is_none());
        assert_eq!(snapshot[&1].as_ref().unwrap().width, 10);
        let mut restored = restored;
        let removed = restored.remove(&1).unwrap().unwrap();
        assert_eq!(removed.width, 10);
    }
    #[test]
    fn consumed_component_roots_and_invalid_transfers_remain_distinct() {
        let mut builder = NodeBuilder::new(Capacities::default());
        let consumed = builder.new_null_box().unwrap();
        let live = builder.new_null_box().unwrap();
        let component = builder.ownership.hold_component([consumed, live]);
        builder.release(consumed, false).unwrap();
        builder.reclaim_unreachable(&[]).unwrap();
        assert!(builder.nodes.contains_key(&live));
        let invalid_transfer = builder.ownership.hold([consumed]);
        assert!(builder.reclaim_unreachable(&[]).is_err());
        drop(invalid_transfer);
        drop(component);
        builder.reclaim_unreachable(&[]).unwrap();
        assert!(builder.nodes.is_empty());
        assert_eq!(builder.reclaimed_nodes, 2);
    }
    #[test]
    fn reachability_keeps_cycles_and_checks_sparse_and_unknown_roots() {
        let mut builder = NodeBuilder::new(Capacities::default());
        let first = builder.new_null_box().unwrap();
        let second = builder.new_null_box().unwrap();
        let garbage = builder.new_null_box().unwrap();
        builder.node_mut(first).unwrap().link = second;
        builder.node_mut(second).unwrap().link = first;
        builder.reclaim_unreachable(&[first]).unwrap();
        assert_eq!(builder.nodes.len(), 2);
        assert!(!builder.nodes.contains_key(&garbage));
        // Sparse restored identities keep the same root/edge validation.
        let distant = builder.node(first).unwrap().clone();
        builder.nodes.insert(i64::MAX, Some(distant));
        builder.reclaim_unreachable(&[i64::MAX]).unwrap();
        assert_eq!(builder.nodes.len(), 3);
        assert!(builder.reclaim_unreachable(&[i64::MIN]).is_err());
        assert_eq!(builder.nodes.len(), 3);
        builder.reclaim_unreachable(&[]).unwrap();
        assert!(builder.nodes.is_empty());
    }
}
