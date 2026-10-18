//! Presentation provenance. These records authorize navigation, never execution reuse.
use crate::{core::source::SourceSpan, typesetting::display::PageDisplayList};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};

/// Four corners in PDF bp, bottom-left page origin, in painting order.
#[derive(Clone, Debug, Deserialize)]
pub struct Geometry {
    pub id: usize,
    pub data: Arc<GeometryData>,
    #[serde(skip)]
    region: Option<Arc<crate::paragraph_preview::Region>>,
}
impl std::ops::Deref for Geometry {
    type Target = GeometryData;
    fn deref(&self) -> &GeometryData {
        &self.data
    }
}
impl Geometry {
    fn region(&self) -> Option<&str> {
        self.region.as_ref().map(|r| r.key.as_str())
    }
}
#[derive(Clone, Debug, Deserialize)]
pub struct GeometryData {
    #[serde(skip)]
    pub(crate) material: Option<std::num::NonZeroU64>,
    pub kind: String,
    pub quad: [[f64; 2]; 4],
    pub origins: crate::core::source::MappedOrigins,
    pub warning: Option<Arc<str>>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, Hash)]
pub struct Range {
    pub file: Arc<str>,
    pub line: usize,
    pub start: usize,
    pub end: usize,
}
impl Range {
    pub fn physical(source: &SourceSpan) -> Option<Self> {
        let (start, end) = (source.byte_column?.get(), source.end_byte_column?.get());
        (source.line > 0 && start > 0 && end > start).then(|| Self {
            file: source.file.clone(),
            line: source.line,
            start,
            end,
        })
    }
}
// The engine bounds expansion provenance. Apply an independent presentation
// bound as well; these links identify invocations, never individual letters.
fn source_chain(source: &SourceSpan) -> impl Iterator<Item = &SourceSpan> {
    std::iter::successors(Some(source), |s| s.call_site.as_deref()).take(9)
}
fn same_mapped_source(mut a: &Arc<SourceSpan>, mut b: &Arc<SourceSpan>) -> bool {
    for _ in 0..9 {
        if Arc::ptr_eq(a, b) {
            return true;
        }
        if a.file != b.file
            || a.line != b.line
            || a.byte_column != b.byte_column
            || a.end_byte_column != b.end_byte_column
        {
            return false;
        }
        match (&a.call_site, &b.call_site) {
            (Some(next_a), Some(next_b)) => {
                a = next_a;
                b = next_b;
            }
            (None, None) => return true,
            _ => return false,
        }
    }
    true
}
#[derive(Clone, Debug, Serialize)]
pub struct Item {
    pub id: usize,
    pub kind: String,
    pub quad: [[f64; 2]; 4],
    pub origins: Vec<usize>,
    pub invocations: Vec<usize>,
    pub region: Option<String>,
    pub warning: Option<Arc<str>>,
}
#[derive(Clone, Debug, Default)]
pub struct FullMap {
    pub ranges: Vec<Range>,
    pub items: Vec<Item>,
    pub truncated: bool,
}
// Intern repeated filenames/paragraph IDs and encode axis-aligned quads as four
// coordinates. Paint identity remains independent of this wire representation.
impl Serialize for FullMap {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeStruct, SerializeTuple};
        struct Coordinates<'a>(&'a [[f64; 2]; 4]);
        impl Serialize for Coordinates<'_> {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                let q = self.0;
                if q[0][1] == q[1][1]
                    && q[1][0] == q[2][0]
                    && q[2][1] == q[3][1]
                    && q[3][0] == q[0][0]
                {
                    [q[0][0], q[0][1], q[2][0], q[2][1]].serialize(s)
                } else {
                    q.iter().flatten().copied().collect::<Vec<_>>().serialize(s)
                }
            }
        }
        struct Origins<'a>(&'a [usize]);
        impl Serialize for Origins<'_> {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                if self.0.len() == 1 {
                    self.0[0].serialize(s)
                } else {
                    self.0.serialize(s)
                }
            }
        }
        struct Entry<'a>(&'a Item, Option<usize>, Option<usize>);
        impl Serialize for Entry<'_> {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                let i = self.0;
                let kind = match i.kind.as_str() {
                    "glyph" => 0u8,
                    "rule" => 1,
                    "hbox" => 2,
                    _ => 3,
                };
                let mut tuple = s.serialize_tuple(if i.invocations.is_empty() { 6 } else { 7 })?;
                tuple.serialize_element(&i.id)?;
                tuple.serialize_element(&kind)?;
                tuple.serialize_element(&Coordinates(&i.quad))?;
                tuple.serialize_element(&Origins(&i.origins))?;
                tuple.serialize_element(&self.1)?;
                tuple.serialize_element(&self.2)?;
                if !i.invocations.is_empty() {
                    tuple.serialize_element(&Origins(&i.invocations))?;
                }
                tuple.end()
            }
        }
        fn intern<'a>(
            value: &'a str,
            values: &mut Vec<&'a str>,
            index: &mut HashMap<&'a str, usize>,
        ) -> usize {
            if let Some(&id) = index.get(value) {
                id
            } else {
                let id = values.len();
                values.push(value);
                index.insert(value, id);
                id
            }
        }
        let (mut files, mut file_ids) = (Vec::new(), HashMap::new());
        let ranges = self
            .ranges
            .iter()
            .map(|r| {
                [
                    intern(&r.file, &mut files, &mut file_ids),
                    r.line,
                    r.start,
                    r.end,
                ]
            })
            .collect::<Vec<_>>();
        let (mut regions, mut region_ids) = (Vec::new(), HashMap::new());
        let (mut warnings, mut warning_ids) = (Vec::new(), HashMap::new());
        let items = self
            .items
            .iter()
            .map(|i| {
                Entry(
                    i,
                    i.region
                        .as_deref()
                        .map(|r| intern(r, &mut regions, &mut region_ids)),
                    i.warning
                        .as_deref()
                        .map(|w| intern(w, &mut warnings, &mut warning_ids)),
                )
            })
            .collect::<Vec<_>>();
        let mut record = serializer.serialize_struct("SourceMap", 7)?;
        record.serialize_field("wire_version", &1u8)?;
        record.serialize_field("files", &files)?;
        record.serialize_field("ranges", &ranges)?;
        record.serialize_field("regions", &regions)?;
        record.serialize_field("warnings", &warnings)?;
        record.serialize_field("items", &items)?;
        record.serialize_field("truncated", &self.truncated)?;
        record.end()
    }
}
impl FullMap {
    /// Fixed admission budget; an unmapped tail is preferable to delaying paint
    /// or inventing a correspondence. Existing source origins remain immutable.
    pub fn page(page: &PageDisplayList) -> Self {
        Self::geometries(page.lines.iter().flat_map(|line| &line.source_geometry))
    }
    fn geometries<'a>(geometries: impl Iterator<Item = &'a Geometry>) -> Self {
        let mut map = Self::default();
        let mut intern = HashMap::new();
        let mut charge = 0usize;
        for geometry in geometries {
            charge += 160 + geometry.origins.len() * 16;
            if charge > 8 * 1024 * 1024 || map.items.len() >= 65536 {
                map.truncated = true;
                break;
            }
            let mut intern_source = |source: &SourceSpan| {
                let range = Range::physical(source)?;
                let index = if let Some(index) = intern.get(&range) {
                    *index
                } else {
                    charge += range.file.len() + 64;
                    let index = map.ranges.len();
                    intern.insert(range.clone(), index);
                    map.ranges.push(range);
                    index
                };
                Some(index)
            };
            let mut origins = Vec::new();
            let mut invocations = Vec::new();
            for source in &geometry.origins {
                if let Some(index) = intern_source(source) {
                    if !origins.contains(&index) {
                        origins.push(index);
                    }
                }
                for call in source_chain(source).skip(1) {
                    if let Some(index) = intern_source(call) {
                        if !invocations.contains(&index) {
                            invocations.push(index);
                        }
                    }
                }
            }
            if geometry
                .origins
                .iter()
                .any(|source| Range::physical(source).is_none())
            {
                origins.clear();
            }
            if geometry.origins.iter().any(|source| {
                !source_chain(source)
                    .skip(1)
                    .any(|call| Range::physical(call).is_some())
            }) {
                invocations.clear();
            }
            if charge > 8 * 1024 * 1024 || map.ranges.len() > 131072 || invocations.len() > 4096 {
                map.truncated = true;
                break;
            }
            if origins.is_empty() && invocations.is_empty() && geometry.warning.is_none() {
                continue;
            }
            map.items.push(Item {
                id: geometry.id,
                kind: geometry.kind.clone(),
                quad: geometry.quad,
                origins,
                invocations,
                region: geometry.region().map(str::to_owned),
                warning: geometry.warning.clone(),
            });
        }
        map
    }
}

// A chunk is admitted by immutable native material identity, never by pixels,
// text or a geometry hash. Emission still runs in its ordinary font/PDF state;
// comparing its actual quads also guards placement, wrapping and transforms.
// Monotonic material generations cannot alias a newly allocated node or keep
// a retired execution graph alive. Source rebasing is accepted only for validated imports.
use std::{cell::RefCell, collections::BTreeMap, sync::OnceLock};
const CACHE_BYTES: usize = 16 * 1024 * 1024;
#[derive(Debug)]
struct Chunk {
    id: u64,
    geometry: Vec<Geometry>,
    map: OnceLock<FullMap>,
    wire: OnceLock<Box<serde_json::value::RawValue>>,
    bytes: usize,
}
impl Chunk {
    fn map(&self) -> &FullMap {
        self.map
            .get_or_init(|| FullMap::geometries(self.geometry.iter()))
    }
    fn wire(&self) -> &serde_json::value::RawValue {
        self.wire.get_or_init(|| {
            serde_json::value::to_raw_value(self.map()).expect("finite emitted geometry")
        })
    }
}
#[derive(Clone, Debug)]
struct Part {
    chunk: Arc<Chunk>,
    placements: Vec<usize>,
    lines: BTreeMap<Arc<str>, isize>,
    regions: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Default)]
pub struct SourceMap {
    parts: Vec<Part>,
    full: OnceLock<FullMap>,
    truncated: bool,
}
impl std::ops::Deref for SourceMap {
    type Target = FullMap;
    fn deref(&self) -> &FullMap {
        self.full.get_or_init(|| {
            let mut full = FullMap {
                truncated: self.truncated,
                ..Default::default()
            };
            for part in &self.parts {
                let map = part.chunk.map();
                let offset = full.ranges.len();
                full.ranges.extend(map.ranges.iter().map(|r| {
                    let mut r = r.clone();
                    r.line = r
                        .line
                        .checked_add_signed(*part.lines.get(&r.file).unwrap_or(&0))
                        .expect("validated range shift");
                    r
                }));
                full.items.extend(map.items.iter().map(|item| {
                    let mut item = item.clone();
                    item.id = part.placements[item.id];
                    for origin in item.origins.iter_mut().chain(&mut item.invocations) {
                        *origin += offset;
                    }
                    if let Some(region) = &item.region {
                        item.region = part
                            .regions
                            .get(region)
                            .cloned()
                            .or_else(|| Some(region.clone()));
                    }
                    item
                }));
                full.truncated |= map.truncated;
            }
            full
        })
    }
}
#[derive(Default)]
struct Cache {
    entries: HashMap<u64, Vec<Arc<Chunk>>>,
    glyphs: HashMap<u64, GeometryEntry>,
    glyph_refs: usize,
    geometry_tick: u64,
    order: std::collections::VecDeque<(u64, u64)>,
    resident: std::collections::HashSet<u64>,
    next: u64,
    bytes: usize,
    disabled: bool,
    profile: bool,
    stats: MapStats,
}
#[derive(Default)]
struct GeometryEntry {
    views: Vec<std::sync::Weak<GeometryData>>,
    age: u64,
}
impl Cache {
    fn prune_geometry(&mut self) {
        self.glyphs.retain(|_, entry| {
            entry.views.retain(|g| g.strong_count() > 0);
            !entry.views.is_empty()
        });
        self.glyph_refs = self.glyphs.values().map(|e| e.views.len()).sum();
    }
    fn trim_geometry(&mut self) {
        if self.glyph_refs < 8192 {
            return;
        }
        self.prune_geometry();
        if self.glyph_refs < 8192 {
            return;
        }
        // Batch eviction avoids both an O(cache-size) scan per glyph and the
        // former all-or-nothing clear that discarded a hot displayed page.
        let mut oldest = self
            .glyphs
            .iter()
            .map(|(&id, e)| (e.age, id))
            .collect::<Vec<_>>();
        oldest.sort_unstable();
        for (_, id) in oldest {
            if self.glyph_refs <= 6144 {
                break;
            }
            if let Some(entry) = self.glyphs.remove(&id) {
                self.glyph_refs -= entry.views.len();
            }
        }
    }
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct MapStats {
    pub font_identity_ms: f64,
    pub font_identity_bytes: usize,
    pub font_identity_hits: usize,
    pub font_identity_misses: usize,
    pub font_identity_hashed_bytes: usize,
    pub font_wire_bytes: usize,
    pub font_wire_hits: usize,
    pub font_wire_misses: usize,
    pub source_span_bytes: usize,
    pub diagnostics_ms: f64,
    pub geometry_ms: f64,
    pub reused_geometry: usize,
    pub regenerated_geometry: usize,
    pub construct_ms: f64,
    pub serialize_ms: f64,
    pub reused_items: usize,
    pub regenerated_items: usize,
    pub transferred_items: usize,
    pub transferred_bytes: usize,
    pub referenced_chunks: usize,
    pub retained_bytes: usize,
}
thread_local! { static CACHE: RefCell<Cache> = RefCell::new(Cache::default()); }
#[cfg(target_arch = "wasm32")]
fn now() -> f64 {
    use wasm_bindgen::prelude::*;
    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace=performance,js_name=now)]
        fn clock() -> f64;
    }
    clock()
}
#[cfg(not(target_arch = "wasm32"))]
fn now() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
        * 1000.0
}
/// Investigation control, independent of execution/checkpoint policy.
pub fn configure(reuse: bool, profile: bool) {
    CACHE.with(|cache| {
        let mut c = cache.borrow_mut();
        c.disabled = !reuse;
        c.profile = profile;
        c.stats = MapStats::default();
    });
}
pub fn begin_publication(resident: &[u64]) {
    CACHE.with(|cache| {
        let mut c = cache.borrow_mut();
        c.resident = resident.iter().take(2048).copied().collect();
        c.stats = MapStats::default();
    });
}
pub fn statistics() -> MapStats {
    CACHE.with(|cache| {
        let c = cache.borrow();
        let mut stats = c.stats.clone();
        (
            stats.font_wire_bytes,
            stats.font_wire_hits,
            stats.font_wire_misses,
        ) = crate::pdf::preview::font_wire_statistics();
        (
            stats.font_identity_bytes,
            stats.font_identity_hits,
            stats.font_identity_misses,
            stats.font_identity_hashed_bytes,
        ) = crate::pdf::preview::font_identity_statistics();
        stats.source_span_bytes = std::mem::size_of::<SourceSpan>();
        stats.retained_bytes = c.bytes + c.glyph_refs * (std::mem::size_of::<GeometryData>() + 128);
        stats
    })
}
fn material(g: &Geometry) -> Option<u64> {
    g.material.map(std::num::NonZeroU64::get)
}

fn compatible(chunk: &Arc<Chunk>, current: &[&Geometry]) -> Option<Part> {
    if chunk.geometry.len() != current.len() {
        return None;
    }
    // A changed paragraph can leave the first boundary material intact while
    // producing many versions of the suffix. Reject changed native endpoints
    // before walking their shared prefix; this is only a lookup hint, never
    // admission proof. Every surviving candidate still gets the full guards.
    if chunk
        .geometry
        .last()
        .zip(current.last())
        .is_some_and(|(old, new)| material(old) != material(new) || old.quad != new.quad)
    {
        return None;
    }
    if chunk
        .geometry
        .iter()
        .zip(current)
        .all(|(old, new)| Arc::ptr_eq(&old.data, &new.data) && old.region() == new.region())
    {
        return Some(Part {
            chunk: chunk.clone(),
            placements: current.iter().map(|g| g.id).collect(),
            lines: BTreeMap::new(),
            regions: BTreeMap::new(),
        });
    }
    let mut lines = BTreeMap::new();
    let mut regions = BTreeMap::new();
    for (old, new) in chunk.geometry.iter().zip(current) {
        if material(old).is_none()
            || material(old) != material(new)
            || old.kind != new.kind
            || old.quad != new.quad
            || old.warning != new.warning
            || old.origins.len() != new.origins.len()
        {
            return None;
        }
        match (old.region(), new.region()) {
            (Some(a), Some(b)) if a == b => {}
            (Some(a), Some(b)) => {
                if let Some(previous) = regions.get(a) {
                    if previous != b {
                        return None;
                    }
                } else {
                    regions.insert(a.to_owned(), b.to_owned());
                }
            }
            (None, None) => {}
            _ => return None,
        }
        for (a, b) in old.origins.iter().zip(&new.origins) {
            let mut a = source_chain(a);
            let mut b = source_chain(b);
            loop {
                let (a, b) = match (a.next(), b.next()) {
                    (Some(a), Some(b)) => (a, b),
                    (None, None) => break,
                    _ => return None,
                };
                match (Range::physical(a), Range::physical(b)) {
                    (Some(a), Some(b))
                        if a.file == b.file && a.start == b.start && a.end == b.end =>
                    {
                        let delta = b.line as isize - a.line as isize;
                        if let Some(previous) = lines.get(&a.file) {
                            if *previous != delta {
                                return None;
                            }
                        } else {
                            lines.insert(a.file, delta);
                        }
                    }
                    (None, None) => {}
                    _ => return None,
                }
            }
        }
    }
    lines.retain(|_, delta| *delta != 0);
    regions.retain(|a, b| a != b);
    Some(Part {
        chunk: chunk.clone(),
        placements: current.iter().map(|g| g.id).collect(),
        lines,
        regions,
    })
}
impl SourceMap {
    pub fn page(page: &PageDisplayList) -> Self {
        let start = CACHE.with(|cache| cache.borrow().profile.then(now));
        CACHE.with(|cache| cache.borrow_mut().prune_geometry());
        let mut result = Self::default();
        let mut group = Vec::new();
        let mut count = 0;
        let mut charge = 0usize;
        for geometry in page.lines.iter().flat_map(|line| &line.source_geometry) {
            charge += 160
                + geometry
                    .origins
                    .iter()
                    .map(|s| {
                        source_chain(s)
                            .map(|s| 16 + s.file.len() + 64)
                            .sum::<usize>()
                    })
                    .sum::<usize>();
            if count >= 65536 || charge > 8 * 1024 * 1024 {
                result.truncated = true;
                break;
            }
            group.push(geometry);
            count += 1;
            // Content-defined boundaries keep suffix chunks aligned when an
            // earlier paragraph gains/loses glyphs. They carry no proof by themselves.
            let anchor = material(geometry).unwrap_or(0).wrapping_mul(0x9e3779b1);
            if group.len() >= 64 || group.len() >= 8 && (anchor >> 7) & 31 == 0 {
                result.parts.push(Self::part(&group));
                group.clear();
            }
        }
        if !group.is_empty() {
            result.parts.push(Self::part(&group));
        }
        if let Some(start) = start {
            CACHE.with(|cache| cache.borrow_mut().stats.construct_ms += now() - start);
        }
        result
    }
    fn part(current: &[&Geometry]) -> Part {
        CACHE.with(|cache| {
            let mut c = cache.borrow_mut();
            let key = material(current[0]).unwrap_or(0);
            if !c.disabled && key != 0 {
                if let Some((chunk, mut part)) = c.entries.get(&key).and_then(|entries| {
                    entries.iter().find_map(|chunk| {
                        compatible(chunk, current).map(|part| (chunk.clone(), part))
                    })
                }) {
                    c.order.retain(|(_, id)| *id != chunk.id);
                    c.order.push_back((key, chunk.id));
                    part.chunk = chunk;
                    c.stats.reused_items += current.len();
                    return part;
                }
            }
            c.next += 1;
            let geometry = current
                .iter()
                .enumerate()
                .map(|(id, g)| {
                    let mut g = (*g).clone();
                    g.id = id;
                    g
                })
                .collect::<Vec<_>>();
            let bytes = geometry
                .iter()
                .map(|g| {
                    384 + g
                        .origins
                        .iter()
                        .map(|s| source_bytes(s) + 128)
                        .sum::<usize>()
                        + g.region().map_or(0, |r| r.len() * 6)
                        + g.warning.as_ref().map_or(0, |w| w.len() * 6)
                })
                .sum();
            let chunk = Arc::new(Chunk {
                id: c.next,
                geometry,
                map: OnceLock::new(),
                wire: OnceLock::new(),
                bytes,
            });
            chunk.map();
            c.stats.regenerated_items += current.len();
            if !c.disabled && bytes <= CACHE_BYTES {
                while (c.bytes + bytes > CACHE_BYTES || c.order.len() >= 2048)
                    && !c.order.is_empty()
                {
                    let (key, id) = c.order.pop_front().unwrap();
                    if let Some(entries) = c.entries.get_mut(&key) {
                        if let Some(index) = entries.iter().position(|chunk| chunk.id == id) {
                            c.bytes -= entries.remove(index).bytes;
                        }
                    }
                    if c.entries.get(&key).is_some_and(Vec::is_empty) {
                        c.entries.remove(&key);
                    }
                }
                c.bytes += bytes;
                c.order.push_back((key, chunk.id));
                c.entries.entry(key).or_default().push(chunk.clone());
            }
            Part {
                chunk,
                placements: current.iter().map(|g| g.id).collect(),
                lines: BTreeMap::new(),
                regions: BTreeMap::new(),
            }
        })
    }
}
impl Serialize for SourceMap {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        #[derive(Serialize)]
        struct Record<'a> {
            id: u64,
            #[serde(skip_serializing_if = "Option::is_none")]
            data: Option<&'a serde_json::value::RawValue>,
            placements: &'a [usize],
            lines: &'a BTreeMap<Arc<str>, isize>,
            regions: &'a BTreeMap<String, String>,
        }
        let start = CACHE.with(|cache| cache.borrow().profile.then(now));
        let mut records = Vec::with_capacity(self.parts.len());
        for part in &self.parts {
            let known = CACHE.with(|cache| cache.borrow().resident.contains(&part.chunk.id));
            let data = (!known).then(|| part.chunk.wire());
            CACHE.with(|cache| {
                let mut c = cache.borrow_mut();
                if let Some(data) = data {
                    c.stats.transferred_items += part.chunk.geometry.len();
                    c.stats.transferred_bytes += data.get().len();
                } else {
                    c.stats.referenced_chunks += 1;
                }
            });
            records.push(Record {
                id: part.chunk.id,
                data,
                placements: &part.placements,
                lines: &part.lines,
                regions: &part.regions,
            });
        }
        let mut record = serializer.serialize_struct("SourceMap", 3)?;
        record.serialize_field("wire_version", &2u8)?;
        record.serialize_field("chunks", &records)?;
        record.serialize_field("truncated", &self.truncated)?;
        let result = record.end();
        if let Some(start) = start {
            CACHE.with(|cache| cache.borrow_mut().stats.serialize_ms += now() - start);
        }
        result
    }
}

#[cfg(test)]
mod incremental_tests {
    use super::*;
    use crate::typesetting::model::Node;
    fn glyph(node: &Node, line: usize, x: f64) -> Geometry {
        Geometry {
            id: 7,
            region: None,
            data: Arc::new(GeometryData {
                material: Some(node.map_material()),
                kind: "glyph".into(),
                quad: [[x, 3.0], [x + 1.0, 3.0], [x + 1.0, 0.0], [x, 0.0]],
                origins: vec![Arc::new(SourceSpan {
                    file: "/project/a.tex".into(),
                    line,
                    column: 1,
                    byte_column: std::num::NonZeroUsize::new(1),
                    end_byte_column: std::num::NonZeroUsize::new(2),
                    expansion: None,
                    call_site: None,
                    call_site_truncated: false,
                })]
                .into(),
                warning: None,
            }),
        }
    }
    #[test]
    fn only_native_material_identity_and_actual_geometry_authorize_reuse() {
        configure(true, false);
        begin_publication(&[]);
        let node = Node::default();
        let old = glyph(&node, 2, 0.0);
        let a = SourceMap::part(&[&old]);
        let same = glyph(&node, 2, 0.0);
        let b = SourceMap::part(&[&same]);
        assert!(Arc::ptr_eq(&a.chunk, &b.chunk));
        let shifted = glyph(&node, 5, 0.0);
        let c = SourceMap::part(&[&shifted]);
        assert!(Arc::ptr_eq(&a.chunk, &c.chunk));
        assert_eq!(c.lines["/project/a.tex"], 3);
        let d = SourceMap {
            parts: vec![c],
            ..Default::default()
        };
        assert_eq!(d.ranges[0].line, 5);
        assert_eq!(d.items[0].id, 7);
        let reflow = glyph(&node, 2, 2.0);
        assert!(!Arc::ptr_eq(&a.chunk, &SourceMap::part(&[&reflow]).chunk));
        let other = Node::default();
        let identical_pixels = glyph(&other, 2, 0.0);
        assert!(!Arc::ptr_eq(
            &a.chunk,
            &SourceMap::part(&[&identical_pixels]).chunk
        ));
        let mut mutation = node.clone();
        mutation.rebase_map_origins(old.origins.iter().cloned().map(Some).collect());
        assert_eq!(mutation.map_material(), node.map_material());
        mutation.char = 65;
        assert!(!Arc::ptr_eq(
            &a.chunk,
            &SourceMap::part(&[&glyph(&mutation, 2, 0.0)]).chunk
        ));
        assert!(statistics().retained_bytes <= CACHE_BYTES + 4 * 1024 * 1024);
    }
    #[test]
    fn full_resynchronization_does_not_depend_on_paint_or_a_previous_revision() {
        configure(true, false);
        let node = Node::default();
        let g = glyph(&node, 2, 0.0);
        let part = SourceMap::part(&[&g]);
        let id = part.chunk.id;
        let map = SourceMap {
            parts: vec![part],
            ..Default::default()
        };
        begin_publication(&[]);
        let full = serde_json::to_value(&map).unwrap();
        assert!(full["chunks"][0]["data"].is_object());
        begin_publication(&[id]);
        let delta = serde_json::to_value(&map).unwrap();
        assert!(delta["chunks"][0].get("data").is_none());
        begin_publication(&[]);
        assert_eq!(serde_json::to_value(&map).unwrap(), full);
    }
    fn with_call(mut geometry: Geometry, line: usize, start: usize) -> Geometry {
        let mut source = (*geometry.origins[0]).clone();
        source.file = "/packages/logo.sty".into();
        let mut call = SourceSpan::new("/project/main.tex", line, start);
        call.byte_column = std::num::NonZeroUsize::new(start);
        call.end_byte_column = std::num::NonZeroUsize::new(start + 6);
        source.call_site = Some(Arc::new(call));
        Arc::make_mut(&mut geometry.data).origins[0] = Arc::new(source);
        geometry
    }
    #[test]
    fn macro_call_links_stay_separate_and_chunk_reuse_validates_their_positions() {
        configure(true, false);
        let node = Node::default();
        let old = with_call(glyph(&node, 2, 0.0), 5, 11);
        let a = SourceMap::part(&[&old]);
        let full = a.chunk.map();
        assert_eq!(
            full.ranges[full.items[0].origins[0]].file.as_ref(),
            "/packages/logo.sty"
        );
        assert_eq!(
            full.ranges[full.items[0].invocations[0]].file.as_ref(),
            "/project/main.tex"
        );
        let shifted = with_call(glyph(&node, 2, 0.0), 6, 11);
        let b = SourceMap::part(&[&shifted]);
        assert!(Arc::ptr_eq(&a.chunk, &b.chunk));
        assert_eq!(b.lines.get("/project/main.tex"), Some(&1));
        let map = SourceMap {
            parts: vec![b],
            ..Default::default()
        };
        assert_eq!(map.ranges[map.items[0].invocations[0]].line, 6);
        assert_eq!(map.ranges[map.items[0].origins[0]].line, 2);
        let moved = with_call(glyph(&node, 2, 0.0), 6, 12);
        assert!(!Arc::ptr_eq(&a.chunk, &SourceMap::part(&[&moved]).chunk));
        let mut item = crate::typesetting::display::Placement {
            map_material: Some(node.map_material()),
            kind: "glyph".into(),
            origins: old.origins.iter().cloned().map(Some).collect(),
            ..Default::default()
        };
        let before = geometry(&item, old.quad, None).unwrap();
        item.origins = moved.origins.iter().cloned().map(Some).collect();
        let after = geometry(&item, old.quad, None).unwrap();
        assert!(!Arc::ptr_eq(&before.data, &after.data));
    }
    #[test]
    fn a_shared_definition_cannot_hide_inconsistent_invocation_shifts() {
        configure(true, false);
        let (a, b) = (Node::default(), Node::default());
        let first = with_call(glyph(&a, 2, 0.0), 5, 11);
        let second = with_call(glyph(&b, 2, 1.0), 6, 11);
        let old = SourceMap::part(&[&first, &second]);
        let shifted = with_call(glyph(&b, 2, 1.0), 7, 11);
        let new = SourceMap::part(&[&first, &shifted]);
        assert!(!Arc::ptr_eq(&old.chunk, &new.chunk));
    }
    #[test]
    fn generated_macro_material_has_only_an_invocation_link_and_unknown_clusters_stay_unmapped() {
        configure(true, false);
        let node = Node::default();
        let mut sample = with_call(glyph(&node, 2, 0.0), 5, 11);
        let source = SourceSpan::without_range(&sample.origins[0]);
        Arc::make_mut(&mut sample.data).origins[0] = source.clone();
        let map = FullMap::geometries([&sample].into_iter());
        assert!(map.items[0].origins.is_empty());
        assert_eq!(map.items[0].invocations.len(), 1);
        let item = crate::typesetting::display::Placement {
            kind: "glyph".into(),
            origins: [None, Some(source)].into_iter().collect(),
            ..Default::default()
        };
        assert!(geometry(&item, sample.quad, None).is_none());
    }
    #[test]
    fn paragraph_owner_changes_share_geometry_but_keep_current_region_bindings() {
        configure(true, false);
        let node = Node::default();
        let sample = glyph(&node, 2, 0.0);
        let region = |key: &str| {
            Arc::new(crate::paragraph_preview::Region {
                occurrence: 1,
                key: key.into(),
                file: "/project/a.tex".into(),
                first_line: 2,
                first_column: 1,
                last_line: 2,
                last_column: 2,
                lines: 1,
            })
        };
        let mut item = crate::typesetting::display::Placement {
            map_material: Some(node.map_material()),
            kind: "glyph".into(),
            origins: sample.origins.iter().cloned().map(Some).collect(),
            preview_owner: Some(crate::paragraph_preview::Owner {
                region: region("old"),
                line: 0,
                root: false,
            }),
            ..Default::default()
        };
        let old = geometry(&item, sample.quad, None).unwrap();
        let a = SourceMap::part(&[&old]);
        item.preview_owner.as_mut().unwrap().region = region("new");
        let new = geometry(&item, sample.quad, None).unwrap();
        assert!(Arc::ptr_eq(&old.data, &new.data));
        let b = SourceMap::part(&[&new]);
        assert!(Arc::ptr_eq(&a.chunk, &b.chunk));
        assert_eq!(b.regions["old"], "new");
        let map = SourceMap {
            parts: vec![b],
            ..Default::default()
        };
        assert_eq!(map.items[0].region.as_deref(), Some("new"));
    }
    #[test]
    fn a_shared_prefix_cannot_acquire_a_different_ranges_line_shift() {
        configure(true, false);
        let (a, b) = (Node::default(), Node::default());
        let old_a = glyph(&a, 2, 0.0);
        let old_b = glyph(&b, 3, 1.0);
        let before = SourceMap::part(&[&old_a, &old_b]);
        let shifted_b = glyph(&b, 4, 1.0);
        let after = SourceMap::part(&[&old_a, &shifted_b]);
        assert!(!Arc::ptr_eq(&before.chunk, &after.chunk));
        let map = SourceMap {
            parts: vec![after],
            ..Default::default()
        };
        assert_eq!(
            map.ranges.iter().map(|r| r.line).collect::<Vec<_>>(),
            [2, 4]
        );
    }
    #[test]
    fn repeated_placements_and_hot_geometry_survive_bounded_cache_turnover() {
        configure(true, false);
        let node = Node::default();
        let sample = glyph(&node, 2, 0.0);
        let mut item = crate::typesetting::display::Placement {
            map_material: Some(node.map_material()),
            kind: "glyph".into(),
            origins: sample.origins.iter().cloned().map(Some).collect(),
            ..Default::default()
        };
        let first = geometry(&item, sample.quad, None).unwrap();
        let shifted = sample.quad.map(|[x, y]| [x + 10.0, y]);
        let second = geometry(&item, shifted, None).unwrap();
        assert!(Arc::ptr_eq(
            &first.data,
            &geometry(&item, sample.quad, None).unwrap().data
        ));
        assert!(Arc::ptr_eq(
            &second.data,
            &geometry(&item, shifted, None).unwrap().data
        ));
        let mut held = Vec::new();
        for n in 0..10000 {
            let other = Node::default();
            item.map_material = Some(other.map_material());
            held.push(geometry(&item, sample.quad, None).unwrap());
            if n % 8 == 0 {
                item.map_material = Some(node.map_material());
                assert!(Arc::ptr_eq(
                    &first.data,
                    &geometry(&item, sample.quad, None).unwrap().data
                ));
            }
        }
        CACHE.with(|c| assert!(c.borrow().glyph_refs <= 8192));
        assert!(statistics().retained_bytes <= CACHE_BYTES + 4 * 1024 * 1024);
    }
}

fn source_bytes(source: &SourceSpan) -> usize {
    let mut total = 0;
    let mut next = Some(source);
    for _ in 0..9 {
        let Some(source) = next else { break };
        total += 128 + source.file.len() * 6 + source.expansion.as_ref().map_or(0, |e| e.len());
        next = source.call_site.as_deref();
    }
    total
}
/// Reuse actual emitted mapping geometry only for the same immutable material
/// and exact placement/matrix/metric result. This is independent of paint hashes.
pub(crate) fn geometry(
    item: &crate::typesetting::display::Placement,
    quad: [[f64; 2]; 4],
    extra: Option<Vec<Arc<SourceSpan>>>,
) -> Option<Geometry> {
    // A valid PDF can contain unsupported/nonfinite transforms. Such output
    // cannot become a finite hit target or an endlessly rejected delta base.
    if !quad.iter().flatten().all(|v| v.is_finite()) {
        return None;
    }
    CACHE.with(|cache| {
        let mut c = cache.borrow_mut();
        let began = c.profile.then(now);
        let origins = extra.as_deref();
        let physical = |s: &SourceSpan| {
            s.byte_column
                .zip(s.end_byte_column)
                .is_some_and(|(a, b)| b > a)
                && s.line > 0
        };
        let navigable = |s: &SourceSpan| source_chain(s).any(physical);
        // A cluster with an unknown constituent cannot acquire that letter's
        // provenance from another constituent (e.g. generated f + literal i).
        if item.kind == "glyph"
            && item
                .origins
                .iter()
                .any(|s| s.as_deref().is_none_or(|s| !navigable(s)))
        {
            return None;
        }
        if item.box_warning.is_none()
            && match origins {
                Some(origins) => origins.iter().all(|s| !navigable(s)),
                None => item.origins.iter().flatten().all(|s| !navigable(s)),
            }
        {
            return None;
        }
        let key = item.map_material.map(std::num::NonZeroU64::get);
        let origins_match = |old: &[Arc<SourceSpan>]| {
            let same = same_mapped_source;
            if let Some(origins) = origins {
                old.len() == origins.len() && old.iter().zip(origins).all(|(a, b)| same(a, b))
            } else {
                old.len() == item.origins.iter().flatten().count()
                    && old
                        .iter()
                        .zip(item.origins.iter().flatten())
                        .all(|(a, b)| same(a, b))
            }
        };
        c.geometry_tick = c.geometry_tick.wrapping_add(1);
        let age = c.geometry_tick;
        let previous = if c.disabled {
            None
        } else {
            key.and_then(|id| c.glyphs.get_mut(&id)).and_then(|entry| {
                entry.age = age;
                entry
                    .views
                    .iter()
                    .filter_map(std::sync::Weak::upgrade)
                    .find(|g| {
                        g.kind == item.kind
                            && g.quad == quad
                            && g.warning == item.box_warning
                            && origins_match(&g.origins)
                    })
            })
        };
        let data = if let Some(previous) = previous {
            c.stats.reused_geometry += 1;
            previous
        } else {
            c.stats.regenerated_geometry += 1;
            let data = Arc::new(GeometryData {
                material: item.map_material.clone(),
                kind: item.kind.clone(),
                quad,
                origins: extra
                    .map(smallvec::SmallVec::from_vec)
                    .unwrap_or_else(|| item.origins.iter().flatten().cloned().collect()),
                warning: item.box_warning.clone(),
            });
            if !c.disabled {
                if let Some(key) = key {
                    c.trim_geometry();
                    let entry = c.glyphs.entry(key).or_default();
                    let previous = entry.views.len();
                    entry.views.retain(|g| g.strong_count() > 0);
                    if entry.views.len() >= 4 {
                        entry.views.remove(0);
                    }
                    entry.views.push(Arc::downgrade(&data));
                    entry.age = age;
                    let current = entry.views.len();
                    c.glyph_refs = c.glyph_refs - previous + current;
                }
            }
            data
        };
        if let Some(began) = began {
            c.stats.geometry_ms += now() - began;
        }
        Some(Geometry {
            id: item.placement_id,
            region: item.preview_owner.as_ref().map(|o| o.region.clone()),
            data,
        })
    })
}

pub(crate) fn profile_start() -> Option<f64> {
    CACHE.with(|cache| cache.borrow().profile.then(now))
}
pub(crate) fn record_warning_time(start: Option<f64>) {
    if let Some(start) = start {
        CACHE.with(|cache| cache.borrow_mut().stats.diagnostics_ms += now() - start);
    }
}
pub(crate) fn record_font_identity_time(start: Option<f64>) {
    if let Some(start) = start {
        CACHE.with(|cache| cache.borrow_mut().stats.font_identity_ms += now() - start);
    }
}
