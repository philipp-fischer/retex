//! Investigation-only executable Relax markers and bounded observations.
//! No macro-lifetime inference from provenance. Times use a slice-paused clock.
use serde::Serialize;
use std::{cell::RefCell, collections::BTreeMap};
const RECORD_LIMIT: usize = 24000;
#[derive(Default, Serialize)]
struct Stage {
    name: String,
    parent: Option<usize>,
    label: String,
    inclusive_ns: u64,
    exclusive_ns: u64,
    categories: BTreeMap<String, u64>,
    counts: BTreeMap<String, u64>,
}
#[derive(Serialize)]
struct Record {
    stage: usize,
    cpu_ns: u64,
    kind: String,
    input: String,
    inclusive_ns: Option<u64>,
    exclusive_ns: Option<u64>,
}
#[derive(Default)]
struct State {
    enabled: bool,
    detail: bool,
    published: bool,
    frozen: bool,
    stages: Vec<Stage>,
    stack: Vec<usize>,
    records: Vec<Record>,
    dropped: u64,
    errors: Vec<String>,
    cpu: u64,
    slice_start: Option<u64>,
    charged: u64,
    operations: Vec<(usize, String, u64, u64, Option<usize>)>,
    copied: BTreeMap<i64, (u64, u64, bool)>,
    copy_hash: u64,
    copy_nodes: usize,
    copy_register: Option<(i64, bool)>,
    trigger: String,
    hook: String,
    assignments: BTreeMap<String, u64>,
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }
fn now() -> u64 {
    super::latency_diagnostics::now()
}
fn clock(s: &State) -> u64 {
    s.cpu + s.slice_start.map_or(0, |start| now().saturating_sub(start))
}
fn charge(s: &mut State) {
    let time = clock(s);
    let elapsed = time.saturating_sub(s.charged);
    s.charged = time;
    for &id in &s.stack {
        s.stages[id].inclusive_ns += elapsed;
    }
    if let Some(&id) = s.stack.last() {
        s.stages[id].exclusive_ns += elapsed;
    }
}
pub fn configure(mode: &str) {
    STATE.with(|s| {
        *s.borrow_mut() = State {
            enabled: matches!(mode, "stages" | "stage-detail"),
            detail: mode == "stage-detail",
            ..State::default()
        }
    });
}
pub fn enabled() -> bool {
    STATE.with(|s| {
        let s = s.borrow();
        s.enabled && !s.frozen
    })
}
pub fn detailed() -> bool {
    STATE.with(|s| {
        let s = s.borrow();
        s.detail && !s.frozen && s.slice_start.is_some() && !s.stack.is_empty()
    })
}
pub fn reset() {
    STATE.with(|s| {
        let old = s.borrow();
        let enabled = old.enabled;
        let detail = old.detail;
        drop(old);
        *s.borrow_mut() = State {
            enabled,
            detail,
            ..State::default()
        };
    });
}
pub struct Slice(bool);
pub fn slice() -> Slice {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if !s.enabled || s.frozen {
            return Slice(false);
        }
        assert!(s.slice_start.is_none());
        s.slice_start = Some(now());
        Slice(true)
    })
}
impl Drop for Slice {
    fn drop(&mut self) {
        if self.0 {
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                charge(&mut s);
                let start = s.slice_start.take().unwrap();
                s.cpu += now().saturating_sub(start);
                s.charged = s.cpu;
            });
        }
    }
}
pub fn marker(name: &str) {
    let Some(rest) = name.strip_prefix("RTDiag") else {
        return;
    };
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if !s.enabled || s.frozen {
            return;
        }
        charge(&mut s);
        if let Some(name) = rest.strip_prefix("Begin") {
            if s.stack.is_empty() && name != "Routine" {
                return;
            }
            if s.stages.len() >= 512 {
                s.errors.push("stage limit".into());
                return;
            }
            let parent = s.stack.last().copied();
            let id = s.stages.len();
            let label = if name == "Hookuse" {
                s.hook.clone()
            } else {
                String::new()
            };
            s.stages.push(Stage {
                name: name.into(),
                label,
                parent,
                ..Stage::default()
            });
            s.stack.push(id);
        } else if let Some(name) = rest.strip_prefix("End") {
            if s.stack.is_empty() && name != "Routine" {
                return;
            }
            if s.stack.last().is_none_or(|&id| s.stages[id].name != name) {
                s.errors.push(format!("unbalanced end {name}"));
            } else {
                s.stack.pop();
                if s.published && s.stack.is_empty() {
                    s.frozen = true;
                }
            }
        }
    });
}
pub fn event(kind: &str, input: String) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if !s.detail || s.frozen || s.slice_start.is_none() {
            return;
        }
        let Some(&stage) = s.stack.last() else {
            return;
        };
        *s.stages[stage].counts.entry(kind.into()).or_default() += 1;
        if s.records.len() >= RECORD_LIMIT {
            s.dropped += 1;
            return;
        }
        let cpu_ns = clock(&s);
        let index = s.records.len();
        if let Some(frame) = s.operations.last_mut() {
            if (kind == "macro" && frame.1 == "macro_preparation")
                || (kind == "box_copy" && frame.1 == "box_copy")
                || (kind == "table_copy" && frame.1 == "shared_table_clone")
            {
                frame.4 = Some(index);
            }
        }
        s.records.push(Record {
            stage,
            cpu_ns,
            kind: kind.into(),
            input,
            inclusive_ns: None,
            exclusive_ns: None,
        });
    });
}
// Fixed FNV-1a over normalized token names/characters; no allocation identities.
pub fn hash(bytes: &[u8]) -> u64 {
    let mut h = 14695981039346656037u64;
    for &b in bytes {
        h = (h ^ b as u64).wrapping_mul(1099511628211);
    }
    h
}
pub fn tokens(
    tokens: &[crate::language::character_tokens::Token],
    env: &crate::language::state::Environment,
) -> u64 {
    let mut h = 14695981039346656037u64;
    for t in tokens {
        let value = if t.control_sequence_id().is_some() {
            {
                let id = t.control_sequence_id().unwrap();
                let name = env.cs_name(id);
                if name.is_empty() {
                    format!("unnamed:{:?};", env.symbol_keys.get(&id))
                } else {
                    format!("cs:{name};")
                }
            }
        } else {
            format!("tok:{t:?};")
        };
        for b in value.bytes() {
            h = (h ^ b as u64).wrapping_mul(1099511628211);
        }
    }
    h
}
pub fn macro_input(
    name: &str,
    definition: &crate::language::tokens::MacroDefinition,
    args: &[crate::language::tokens::ArgumentTokens],
    env: &crate::language::state::Environment,
) {
    if !detailed() {
        return;
    }
    let args: Vec<_> = args.iter().map(|a| tokens(&a.tokens, env)).collect();
    let parameters = definition.parameters.as_ref().map(|p| {
        (
            tokens(&p.prefix, env),
            p.delimiters
                .iter()
                .map(|d| tokens(d, env))
                .collect::<Vec<_>>(),
            p.hash_brace,
        )
    });
    event(
        "macro",
        format!(
            "{name}|body={:016x}|parameters={parameters:x?}|args={args:x?}|long={}|outer={}|protected={}",
            tokens(&definition.body, env),
            definition.long,
            definition.outer,
            definition.protected
        ),
    );
}
pub struct Operation(bool);
pub fn operation(name: &str) -> Operation {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if !s.enabled
            || s.frozen
            || s.slice_start.is_none()
            || s.stack.is_empty()
            || !matches!(
                name,
                "box_copy"
                    | "box_dimension"
                    | "box_unpack_resume"
                    | "shared_table_clone"
                    | "node_reclamation"
                    | "shipment_and_placement"
                    | "file_check"
                    | "font_resolution"
                    | "font_bind"
                    | "font_load"
                    | "font_materialize"
                    | "resource_read"
                    | "box_register_copy"
                    | "meaning_lookup"
                    | "state_assignment"
                    | "expression_resume"
                    | "macro_preparation"
                    | "argument_scanning"
                    | "substitution_and_provenance"
            )
        {
            return Operation(false);
        }
        let id = *s.stack.last().unwrap();
        let start = clock(&s);
        s.operations.push((id, name.into(), start, 0, None));
        Operation(true)
    })
}
impl Drop for Operation {
    fn drop(&mut self) {
        if self.0 {
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                let (id, name, start, children, record) = s.operations.pop().unwrap();
                let elapsed = clock(&s).saturating_sub(start);
                if let Some(index) = record {
                    s.records[index].inclusive_ns = Some(elapsed);
                    s.records[index].exclusive_ns = Some(elapsed.saturating_sub(children));
                }
                *s.stages[id].categories.entry(name.clone()).or_default() +=
                    elapsed.saturating_sub(children);
                *s.stages[id].counts.entry(name).or_default() += 1;
                if let Some(parent) = s.operations.last_mut() {
                    parent.3 += elapsed;
                }
            });
        }
    }
}
pub fn register(n: i64, copy: bool) {
    if enabled() {
        STATE.with(|s| s.borrow_mut().copy_register = Some((n, copy)));
    }
}
pub fn copy_begin() {
    if detailed() {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.copy_hash = 14695981039346656037;
            s.copy_nodes = 0;
        });
    }
}
fn node_hash(node: &crate::typesetting::model::Node) -> u64 {
    node_hash_dimensions(node, true)
}
fn node_hash_dimensions(node: &crate::typesetting::model::Node, dimensions: bool) -> u64 {
    // Deliberately selected structural fields; PDF payload/object numbers,
    // origins, allocation links and font ordinal identity are not dependencies.
    hash(
        format!(
            "{:?}|{},{},{}|{}|{}|{:?}|{},{},{}|{:?}|{}|{}|{},{},{},{}",
            node.node_type,
            if dimensions { node.width } else { 0 },
            if dimensions { node.height } else { 0 },
            if dimensions { node.depth } else { 0 },
            node.char,
            node.shift,
            node.components,
            node.penalty,
            node.language,
            node.discretionary_break,
            node.glue,
            node.kern_kind,
            node.whatsit,
            node.glue_sign,
            node.glue_order,
            node.glue_numerator,
            node.glue_denominator
        )
        .as_bytes(),
    )
}
pub fn copied(target: i64, node: &crate::typesetting::model::Node) {
    if !detailed() {
        return;
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let h = node_hash(node);
        s.copy_hash = (s.copy_hash ^ h).wrapping_mul(1099511628211);
        s.copy_nodes += 1;
        if s.copied.len() < 65536 {
            let root = s.copy_nodes == 1;
            s.copied
                .insert(target, (h, node_hash_dimensions(node, false), root));
        }
    });
}
pub fn copy_end() {
    if !detailed() {
        return;
    }
    let input = STATE.with(|s| {
        let s = s.borrow();
        format!(
            "register={:?}|nodes={}|selected_structure={:016x}",
            s.copy_register, s.copy_nodes, s.copy_hash
        )
    });
    event("box_copy", input);
}
pub fn observe_copies(builder: &crate::typesetting::nodes::NodeBuilder) {
    if !detailed() {
        return;
    }
    let summary = STATE.with(|s| {
        let s = s.borrow();
        let mut same = 0;
        let mut changed = 0;
        let mut gone = 0;
        let mut root_dimensions_only=0;let mut descendant_dimensions_only=0;let mut other_selected_changes=0;
        for (&p, &(h,content,root)) in &s.copied {
            match builder.node(p) {
                Ok(n) => {
                    if node_hash(n) == h {
                        same += 1;
                    } else {
                        changed += 1;
                        if node_hash_dimensions(n,false)==content {if root {root_dimensions_only+=1;}else{descendant_dimensions_only+=1;}}else{other_selected_changes+=1;}
                    }
                }
                Err(_) => gone += 1,
            }
        }
        format!(
            "same_selected_fields={same}|changed_selected_fields={changed}|root_dimensions_only={root_dimensions_only}|descendant_dimensions_only={descendant_dimensions_only}|other_selected_changes={other_selected_changes}|gone_or_reclaimed={gone}"
        )
    });
    event("copy_observation", summary);
}
pub fn assignment(key: &str) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if !s.enabled || s.frozen || s.slice_start.is_none() || s.stack.is_empty() {
            return;
        }
        *s.assignments.entry(key.into()).or_default() += 1;
    });
}
pub fn snapshot() -> serde_json::Value {
    STATE.with(|s|{let mut s=s.borrow_mut();charge(&mut s);serde_json::json!({"complete":s.frozen,"stages":s.stages,"records":s.records,"dropped_records":s.dropped,"errors":s.errors,"open_stages":s.stack,"assignments":s.assignments,"cpu_ns":clock(&s),"limits":"24000 records, 512 stages, 65536 copied node identities. Selected box fields omit payloads, font/resource identity and provenance. Reads are partial, not a reuse proof."})})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_output_scope_freezes_only_after_publication_and_its_end() {
        configure("stage-detail");
        {
            let _slice = slice();
            marker("RTDiagBeginHookuse");
            marker("RTDiagEndHookuse");
            assert!(snapshot()["stages"].as_array().unwrap().is_empty());
            marker("RTDiagBeginRoutine");
            marker("RTDiagBeginInner");
            {
                let _operation = operation("box_copy");
                event("box_copy", "stable-box".into());
            }
            publication();
            marker("RTDiagEndInner");
            assert!(!complete());
            marker("RTDiagEndRoutine");
            assert!(complete());
        }
        let report = snapshot();
        let rows = report["stages"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(report["errors"].as_array().unwrap().is_empty());
        assert_eq!(report["records"].as_array().unwrap().len(), 1);
        assert!(report["records"][0]["exclusive_ns"].is_u64());
        {
            let _slice = slice();
            marker("RTDiagBeginRoutine");
            marker("RTDiagEndRoutine");
        }
        assert_eq!(snapshot()["stages"].as_array().unwrap().len(), 2);
        reset();
        assert!(enabled());
        assert!(!complete());
        configure("off");
    }
    #[test]
    fn suspended_output_excludes_preview_operations_and_counts() {
        configure("stage-detail");
        {
            let _slice = slice();
            marker("RTDiagBeginRoutine");
        }
        assert!(!detailed());
        assert!(!active());
        {
            let _operation = operation("font_materialize");
            event("preview", "excluded".into());
            count("preview_count", 1);
        }
        assert!(snapshot()["records"].as_array().unwrap().is_empty());
        {
            let _slice = slice();
            event("engine", "included".into());
            marker("RTDiagEndRoutine");
        }
        let r = snapshot();
        assert_eq!(r["records"].as_array().unwrap().len(), 1);
        assert!(r["stages"][0]["categories"].as_object().unwrap().is_empty());
        configure("off");
    }
    #[test]
    fn record_cap_is_reported_without_losing_timing_balance() {
        configure("stage-detail");
        {
            let _slice = slice();
            marker("RTDiagBeginRoutine");
            for _ in 0..RECORD_LIMIT + 3 {
                event("branch", "true".into());
            }
            marker("RTDiagEndRoutine");
        }
        let r = snapshot();
        assert_eq!(r["records"].as_array().unwrap().len(), RECORD_LIMIT);
        assert_eq!(r["dropped_records"], 3);
        assert!(r["open_stages"].as_array().unwrap().is_empty());
        configure("off");
    }
    #[test]
    fn nested_stages_exclude_wait_and_balance() {
        configure("stages");
        {
            let _s = slice();
            marker("RTDiagBeginRoutine");
            marker("RTDiagBeginInner");
            marker("RTDiagEndInner");
        }
        let before = snapshot()["stages"][0]["inclusive_ns"].as_u64().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(
            snapshot()["stages"][0]["inclusive_ns"].as_u64().unwrap(),
            before
        );
        {
            let _s = slice();
            marker("RTDiagEndRoutine");
        }
        let r = snapshot();
        assert!(r["errors"].as_array().unwrap().is_empty());
        assert!(r["open_stages"].as_array().unwrap().is_empty());
        let a = &r["stages"][0];
        let b = &r["stages"][1];
        assert_eq!(
            a["inclusive_ns"].as_u64().unwrap(),
            a["exclusive_ns"].as_u64().unwrap() + b["inclusive_ns"].as_u64().unwrap()
        );
        configure("off");
    }
}

pub fn count(name: &str, n: usize) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if !s.enabled || s.frozen || s.slice_start.is_none() {
            return;
        }
        if let Some(&id) = s.stack.last() {
            *s.stages[id].counts.entry(name.into()).or_default() += n as u64;
        }
    });
}
pub fn trigger(name: String) {
    STATE.with(|s| s.borrow_mut().trigger = name);
}
pub fn current_trigger() -> String {
    STATE.with(|s| s.borrow().trigger.clone())
}

#[allow(dead_code)] // Wasm-only publication bridge.
pub fn publication() {
    STATE.with(|s| s.borrow_mut().published = true);
}
#[allow(dead_code)] // Wasm-only publication bridge.
pub fn complete() -> bool {
    STATE.with(|s| s.borrow().frozen)
}

pub fn hook(args: &[crate::language::tokens::ArgumentTokens]) {
    if !enabled() {
        return;
    }
    let name = args
        .first()
        .map(|a| {
            a.tokens
                .iter()
                .filter_map(|&t| {
                    crate::language::character_tokens::token_character(t).and_then(char::from_u32)
                })
                .collect::<String>()
        })
        .unwrap_or_default();
    STATE.with(|s| s.borrow_mut().hook = name);
}

pub fn active() -> bool {
    STATE.with(|s| {
        let s = s.borrow();
        s.enabled && !s.frozen && s.slice_start.is_some() && !s.stack.is_empty()
    })
}
pub fn operand(
    value: &crate::language::records::CommandOperand,
    env: &crate::language::state::Environment,
) -> String {
    if let crate::language::records::CommandOperand::Macro(m) = value {
        let parameters = m.parameters.as_ref().map(|p| {
            (
                tokens(&p.prefix, env),
                p.delimiters
                    .iter()
                    .map(|d| tokens(d, env))
                    .collect::<Vec<_>>(),
            )
        });
        format!(
            "macro:body={:016x}|parameters={parameters:x?}|long={}|outer={}|protected={}",
            tokens(&m.body, env),
            m.long,
            m.outer,
            m.protected
        )
    } else {
        format!("{value:?}")
    }
}
