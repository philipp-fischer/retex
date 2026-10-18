//! Opt-in investigative accounting. Never a semantic cache or interpreter path.
//! Clocks occur at phase boundaries and every 128 resident steps, not per token.
use crate::core::source::SourceSpan;
use serde::Serialize;
/// Selected complete outer expansion calls, not complete macro invocations.
/// Operation flags describe observations, never authorize semantic reuse.
#[derive(Clone, Debug, Serialize)]
pub struct ExpansionPacketMeasurement {
    pub first_macro: String,
    pub operations: u8,
    pub conditional_depth_unchanged: bool,
    pub calls: u64,
    pub macros: u64,
    pub tokens: u64,
    pub active_ns: u64,
    pub max_ns: u64,
}
use std::{
    cell::RefCell,
    collections::BTreeMap,
    sync::{Arc, Weak},
};

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Region {
    pub file: String,
    pub first: usize,
    pub last: usize,
    pub truncated: bool,
}
thread_local! { static ROOTS:RefCell<BTreeMap<usize,(Weak<SourceSpan>,Region)>>=RefCell::new(BTreeMap::new()); }
pub fn remember_bounded(original: Option<&Arc<SourceSpan>>, bounded: Option<&Arc<SourceSpan>>) {
    if !enabled() {
        return;
    }
    let root = region(original);
    if root.truncated {
        return;
    }
    if let Some(bounded) = bounded {
        ROOTS.with(|roots| {
            let mut roots = roots.borrow_mut();
            // Reclaim once at revision reset; never scan a full live map per call.
            if roots.len() < 8192 {
                roots.insert(
                    Arc::as_ptr(bounded) as usize,
                    (Arc::downgrade(bounded), root),
                );
            }
        });
    }
}
pub fn region(source: Option<&Arc<SourceSpan>>) -> Region {
    let Some(mut source) = source else {
        return Region::default();
    };
    let mut truncated = source.call_site_truncated;
    for _ in 0..16 {
        if let Some(root) = ROOTS.with(|roots| {
            roots
                .borrow()
                .get(&(Arc::as_ptr(source) as usize))
                .filter(|(weak, _)| {
                    weak.upgrade()
                        .is_some_and(|live| Arc::ptr_eq(&live, source))
                })
                .map(|(_, root)| root.clone())
        }) {
            return root;
        }
        let Some(parent) = source.call_site.as_ref() else {
            break;
        };
        source = parent;
        truncated |= source.call_site_truncated;
    }
    Region {
        file: source.file.to_string(),
        first: source.line,
        last: source.line,
        truncated,
    }
}
#[derive(Clone, Debug, Default, Serialize)]
struct Row {
    region: Region,
    phase: String,
    label: String,
    calls: u64,
    inclusive_ns: u64,
    exclusive_ns: u64,
    counters: BTreeMap<String, u64>,
}
struct Frame {
    row: Option<usize>,
    start: u64,
    children: u64,
}
#[derive(Default)]
struct State {
    enabled: bool,
    local: bool,
    detail: bool,
    bypass_provenance: bool,
    keys: BTreeMap<(Region, String, String), usize>,
    rows: Vec<Row>,
    stack: Vec<Frame>,
    macros: u64,
    execution_cpu: u64,
    execution_slice: Option<u64>,
    dropped: u64,
    copied: BTreeMap<i64, String>,
    macro_inputs: BTreeMap<String, BTreeMap<u64, u64>>,
    last_macro: Option<usize>,
}
thread_local! { static STATE:RefCell<State>=RefCell::new(State::default()); }
#[cfg(target_arch = "wasm32")]
pub(crate) fn now() -> u64 {
    use wasm_bindgen::prelude::*;
    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace=performance,js_name=now)]
        fn perf_now() -> f64;
    }
    (perf_now() * 1_000_000.0) as u64
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn now() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_nanos() as u64
}
// A segment clock shared across resident calls. It includes resident execution
// and incremental bookkeeping, and excludes time waiting between calls. There
// are two clocks per resident call, never a timer per token/interpreter step.
pub(crate) struct ExecutionSlice(bool);
pub(crate) fn execution_slice() -> ExecutionSlice {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if !s.enabled && !s.local {
            return ExecutionSlice(false);
        }
        debug_assert!(s.execution_slice.is_none());
        s.execution_slice = Some(now());
        ExecutionSlice(true)
    })
}
impl Drop for ExecutionSlice {
    fn drop(&mut self) {
        if self.0 {
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                if let Some(start) = s.execution_slice.take() {
                    s.execution_cpu += now().saturating_sub(start);
                }
            });
        }
    }
}
pub(crate) fn execution_clock() -> Option<u64> {
    STATE.with(|s| {
        let s = s.borrow();
        (s.enabled || s.local).then(|| {
            s.execution_cpu
                + s.execution_slice
                    .map_or(0, |start| now().saturating_sub(start))
        })
    })
}
pub fn local_enabled() -> bool {
    STATE.with(|s| s.borrow().local)
}
pub fn enabled() -> bool {
    STATE.with(|s| s.borrow().enabled)
}
pub fn bypass_provenance() -> bool {
    STATE.with(|s| s.borrow().bypass_provenance)
}
pub fn detail() -> bool {
    STATE.with(|s| s.borrow().enabled && s.borrow().detail)
}
#[allow(dead_code)]
pub fn configure(mode: &str) {
    crate::local_update_audit::reset();
    crate::output_diagnostics::configure(mode);
    ROOTS.with(|roots| roots.borrow_mut().clear());
    STATE.with(|s| {
        *s.borrow_mut() = State {
            enabled: matches!(mode, "coarse" | "detail"),
            local: mode == "local",
            bypass_provenance: mode == "provenance-bypass",
            detail: mode == "detail",
            ..State::default()
        }
    });
}
#[allow(dead_code)]
pub fn reset() {
    crate::local_update_audit::reset();
    crate::output_diagnostics::reset();
    ROOTS.with(|roots| {
        roots
            .borrow_mut()
            .retain(|_, (weak, _)| weak.strong_count() > 0)
    });
    STATE.with(|s| {
        let old = s.borrow();
        let enabled = old.enabled;
        let local = old.local;
        let detail = old.detail;
        let bypass_provenance = old.bypass_provenance;
        drop(old);
        *s.borrow_mut() = State {
            enabled,
            local,
            detail,
            bypass_provenance,
            ..State::default()
        };
    });
}
fn row_id(s: &mut State, key: (Region, String, String)) -> Option<usize> {
    if let Some(&id) = s.keys.get(&key) {
        return Some(id);
    }
    if s.rows.len() >= 8192 {
        s.dropped += 1;
        return None;
    }
    let id = s.rows.len();
    s.rows.push(Row {
        region: key.0.clone(),
        phase: key.1.clone(),
        label: key.2.clone(),
        ..Row::default()
    });
    s.keys.insert(key, id);
    Some(id)
}
fn add(row: &mut Row, name: &str, n: u64) {
    if let Some(value) = row.counters.get_mut(name) {
        *value += n;
    } else {
        row.counters.insert(name.into(), n);
    }
}
fn bounded_region(mut source: Region) -> Region {
    // The engine deliberately truncates provenance. Do not invent a source line.
    if source.truncated {
        source.file = "@truncated-provenance".into();
        source.first = 0;
        source.last = 0;
    }
    source
}
// The second field is held for its Drop implementation.
#[allow(dead_code)]
pub struct Guard(bool, crate::output_diagnostics::Operation);
pub fn phase(phase: &str, label: &str, source: Option<Region>) -> Guard {
    let output = crate::output_diagnostics::operation(phase);
    if crate::output_diagnostics::detailed()
        && matches!(phase, "font_resolution" | "font_load" | "font_materialize")
    {
        crate::output_diagnostics::event(phase, label.into());
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if !s.enabled {
            return Guard(false, output);
        }
        let region = source.unwrap_or_else(|| {
            s.stack
                .last()
                .and_then(|f| f.row)
                .map(|id| s.rows[id].region.clone())
                .unwrap_or_default()
        });
        let key = (
            bounded_region(region),
            phase.into(),
            label.chars().take(512).collect(),
        );
        let id = row_id(&mut s, key);
        if let Some(id) = id {
            s.rows[id].calls += 1;
        }
        s.stack.push(Frame {
            row: id,
            start: now(),
            children: 0,
        });
        Guard(true, output)
    })
}
fn finish(s: &mut State, frame: Frame, end: u64) {
    let elapsed = end.saturating_sub(frame.start);
    if let Some(id) = frame.row {
        let row = &mut s.rows[id];
        row.inclusive_ns += elapsed;
        row.exclusive_ns += elapsed.saturating_sub(frame.children);
    }
    if let Some(parent) = s.stack.last_mut() {
        parent.children += elapsed;
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        if self.0 {
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                if let Some(frame) = s.stack.pop() {
                    finish(&mut s, frame, now());
                }
            });
        }
    }
}
pub fn switch_region(source: Region, output: bool, chain: &str) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if !s.enabled || s.stack.len() != 1 {
            return;
        }
        let source = if output {
            Region {
                file: "@output-routine".into(),
                ..Default::default()
            }
        } else {
            bounded_region(source)
        };
        if s.stack[0]
            .row
            .is_some_and(|id| s.rows[id].region == source && s.rows[id].label == chain)
        {
            return;
        }
        let frame = s.stack.pop().unwrap();
        let time = now();
        finish(&mut s, frame, time);
        let key = (
            source,
            "engine_residual".into(),
            chain.chars().take(512).collect(),
        );
        let id = row_id(&mut s, key);
        if let Some(id) = id {
            s.rows[id].calls += 1;
        }
        s.stack.push(Frame {
            row: id,
            start: time,
            children: 0,
        });
    });
}
pub fn source_chain(source: Option<&Arc<SourceSpan>>) -> String {
    let mut names = Vec::new();
    let mut source = source;
    for _ in 0..8 {
        let Some(span) = source else {
            break;
        };
        if let Some(name) = &span.expansion {
            names.push(format!("{}|{}:{}", name, span.file, span.line));
        }
        source = span.call_site.as_ref();
    }
    names.join(" > ")
}
pub fn macro_phase(phase_name: &str, label: &str, source: Option<Region>) -> Guard {
    let selected = STATE.with(|s| {
        let s = s.borrow();
        s.enabled && s.detail && s.macros % 16 == 0
    });
    if selected {
        phase(phase_name, label, source)
    } else {
        Guard(false, crate::output_diagnostics::operation("disabled"))
    }
}
pub fn paragraph_region(items: &[i64], builder: &crate::typesetting::nodes::NodeBuilder) -> Region {
    let mut files: BTreeMap<String, (usize, usize, usize, bool)> = BTreeMap::new();
    for &pointer in items {
        if let Ok(node) = builder.node(pointer) {
            for source in node.origins.iter().flatten() {
                let r = region(Some(source));
                let entry = files
                    .entry(r.file)
                    .or_insert((0, r.first, r.last, r.truncated));
                entry.0 += 1;
                entry.1 = entry.1.min(r.first);
                entry.2 = entry.2.max(r.last);
                entry.3 |= r.truncated;
            }
        }
    }
    files
        .into_iter()
        .max_by_key(|(_, r)| r.0)
        .map(|(file, (_, first, last, truncated))| Region {
            file,
            first,
            last,
            truncated,
        })
        .unwrap_or_default()
}
pub fn count(name: &str, n: usize) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if !s.enabled {
            return;
        }
        let Some(id) = s.stack.last().and_then(|f| f.row) else {
            return;
        };
        add(&mut s.rows[id], name, n as u64);
    });
}
pub fn macro_call(
    name: &str,
    definition: Option<&Arc<SourceSpan>>,
    call: Option<&Arc<SourceSpan>>,
    body: usize,
) -> Guard {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if !s.enabled {
            return;
        }
        s.macros += 1;
        let def = definition
            .map(|d| format!("{}:{}:{}", d.file, d.line, d.column))
            .unwrap_or_default();
        let key = (
            bounded_region(region(call)),
            "macro_calls".into(),
            format!("{name}|{def}"),
        );
        let id = row_id(&mut s, key);
        if let Some(id) = id {
            let row = &mut s.rows[id];
            row.calls += 1;
            add(row, "body_tokens", body as u64);
        }
        s.last_macro = id;
    });
    // Selected synchronous preparations only. These are not whole expansion
    // lifetimes; their inclusive times must not be added to the exclusive totals.
    let selected = STATE.with(|s| {
        let s = s.borrow();
        s.enabled && s.detail && s.macros % 16 == 0
    });
    if selected {
        phase("macro_prepare_sample", name, Some(region(call)))
    } else {
        Guard(false, crate::output_diagnostics::operation("disabled"))
    }
}
pub fn macro_material(
    name: &str,
    body: &[crate::language::character_tokens::Token],
    args: &[crate::language::tokens::ArgumentTokens],
    replacement: usize,
) {
    if !enabled() {
        return;
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(id) = s.last_macro {
            let row = &mut s.rows[id];
            {
                *row.counters.entry("argument_tokens".into()).or_default() +=
                    args.iter().map(|a| a.tokens.len() as u64).sum::<u64>();
                *row.counters.entry("replacement_tokens".into()).or_default() += replacement as u64;
            }
        }
    });
    let selected = STATE.with(|s| {
        let s = s.borrow();
        s.enabled && s.detail && s.macros % 16 == 0
    });
    if !selected {
        return;
    }
    use std::hash::{Hash, Hasher};
    count("argument_tokens", args.iter().map(|a| a.tokens.len()).sum());
    count("replacement_tokens", replacement);
    // Bounded diagnostic fingerprints; equality is evidence about token inputs,
    // not a proof that all interpreter dependencies or effects are unchanged.
    if body.len() + args.iter().map(|a| a.tokens.len()).sum::<usize>() > 4096 {
        return;
    }
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut hash);
    for arg in args {
        arg.tokens.hash(&mut hash);
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.macro_inputs.len() < 512 || s.macro_inputs.contains_key(name) {
            let entries = s.macro_inputs.entry(name.into()).or_default();
            if entries.len() < 128 || entries.contains_key(&hash.finish()) {
                *entries.entry(hash.finish()).or_default() += 1;
            }
        }
    });
}
pub fn copied_node(_source: i64, target: i64, node: &crate::typesetting::model::Node) {
    if !enabled() {
        return;
    }
    count("copied_nodes", 1);
    count("node_record_bytes", std::mem::size_of_val(node));
    count(
        "inline_owned_bytes",
        node.components.len() * 4
            + node.origins.len() * std::mem::size_of::<Option<Arc<SourceSpan>>>()
            + node.kern_kind.len()
            + node.leader_kind.len()
            + node.glue_kind.len()
            + node.whatsit.len(),
    );
    if detail() {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            if s.copied.len() < 65536 {
                let label = format!(
                    "{}",
                    s.stack
                        .last()
                        .and_then(|f| f.row)
                        .map(|id| s.rows[id].label.as_str())
                        .unwrap_or("")
                );
                s.copied.insert(target, label);
            }
        });
    }
}
pub fn copied_access(pointer: i64, mutation: bool) {
    if !detail() {
        return;
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(label) = s.copied.get(&pointer).cloned() else {
            return;
        };
        let key = (Region::default(), "copied_node_access".into(), label);
        if let Some(id) = row_id(&mut s, key) {
            let row = &mut s.rows[id];
            *row.counters
                .entry(
                    if mutation {
                        "mutation_access"
                    } else {
                        "traversal_access"
                    }
                    .into(),
                )
                .or_default() += 1;
        }
    });
}
#[allow(dead_code)]
pub fn snapshot() -> serde_json::Value {
    let local_update = crate::local_update_audit::snapshot();
    STATE.with(|s|{let s=s.borrow();serde_json::json!({"local_update":local_update,"output":crate::output_diagnostics::snapshot(),"enabled":s.enabled,"detail":s.detail,"potentially_incorrect_provenance_bypass":s.bypass_provenance,"provenance_identity_entries":ROOTS.with(|roots| roots.borrow().len()),"rows":s.rows,"macro_input_fingerprints":s.macro_inputs,"dropped_rows":s.dropped,"tracked_copied_nodes":s.copied.len(),"limits":"Exclusive phase rows partition timed resident calls; nested inclusive rows overlap. Regions sample source every128 resident steps. Macro timings sample preparations every16 invocations; fingerprints omit unobserved dependencies."})})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostic_bypass_is_explicit_and_survives_only_revision_reset() {
        configure("off");
        assert!(!bypass_provenance());
        configure("provenance-bypass");
        assert!(bypass_provenance());
        assert!(!enabled());
        reset();
        assert!(bypass_provenance());
        configure("coarse");
        assert!(!bypass_provenance());
        configure("off");
    }
    #[test]
    fn truncated_provenance_keeps_only_live_immutable_identity() {
        configure("coarse");
        let original = Arc::new(SourceSpan::new("document.tex", 126, 2));
        let mut shortened = SourceSpan::new("macros.sty", 22, 1);
        shortened.call_site_truncated = true;
        let bounded = Arc::new(shortened);
        remember_bounded(Some(&original), Some(&bounded));
        assert_eq!(region(Some(&bounded)).file, "document.tex");
        reset();
        assert_eq!(region(Some(&bounded)).first, 126);
        drop(bounded);
        reset();
        assert!(ROOTS.with(|roots| roots.borrow().is_empty()));
        configure("off");
    }
    #[test]
    fn nested_exclusive_accounting_and_reset_are_isolated() {
        configure("coarse");
        {
            let _a = phase("outer", "", None);
            {
                let _b = phase("inner", "", None);
                count("items", 3);
            }
        }
        let report = snapshot();
        let rows = report["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let outer = rows.iter().find(|r| r["phase"] == "outer").unwrap();
        let inner = rows.iter().find(|r| r["phase"] == "inner").unwrap();
        assert_eq!(
            outer["inclusive_ns"].as_u64().unwrap(),
            outer["exclusive_ns"].as_u64().unwrap() + inner["inclusive_ns"].as_u64().unwrap()
        );
        reset();
        assert!(snapshot()["rows"].as_array().unwrap().is_empty());
        configure("off");
    }
}
