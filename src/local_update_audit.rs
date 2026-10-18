//! Bounded feasibility probe, compiled only with latency-diagnostics.
//!
//! This is deliberately not a publication certificate. The attempted narrow
//! recipe recognizes only native direct output; arbitrary TeX output programs need
//! a proof of their observations of the replaceable material. Recording their
//! execution, or finding equal dimensions, cannot supply that proof.
use crate::{
    core::{constants::Constants as C, enums::NodeKind},
    language::{
        records::{Quantity, ValueKey},
        state::Environment,
    },
    typesetting::{
        model::NodePayload,
        nodes::NodeBuilder,
        pages::{OutputHandler, PageBuilder},
    },
};
use serde::Serialize;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};

const PARAGRAPHS: usize = 32;
const NODES: usize = 8192;
const CONSUMERS: usize = 64;

#[derive(Default, Serialize)]
struct Audit {
    paragraphs: Vec<Paragraph>,
    consumers: Vec<Consumer>,
    publication_active_ns: Option<u64>,
    dropped: usize,
    target: Option<(String, usize)>,
    watched_ordinal: Option<usize>,
    #[serde(skip)]
    watched: BTreeSet<i64>,
    accesses: BTreeMap<String, (usize, usize)>,
    copied_watched_nodes: usize,
}
#[derive(Serialize)]
struct Paragraph {
    ordinal: usize,
    ready_active_ns: u64,
    capture_wall_ns: u64,
    main_cursor_bytes: usize,
    edited_offset: Option<usize>,
    // Root geometry is an admission precondition, never equivalence evidence.
    geometry: Vec<(i64, i64, i64, i64)>,
    penalties: Vec<i64>,
    source_lines: BTreeMap<String, (usize, usize)>,
    truncated_origins: bool,
    kinds: BTreeMap<String, usize>,
    fonts: BTreeSet<usize>,
    nodes: usize,
    material_miss: Option<&'static str>,
    construction_miss: &'static str,
    output_tokens: usize,
    page_nodes: usize,
    queued_nodes: usize,
    active_output: bool,
}
#[derive(Serialize)]
struct Consumer {
    active_ns: u64,
    operation: &'static str,
}
thread_local! { static AUDIT: RefCell<Audit> = RefCell::new(Audit::default()); }

pub(crate) fn reset() {
    AUDIT.with(|s| *s.borrow_mut() = Audit::default());
}
pub(crate) fn target(path: String, line: usize) {
    AUDIT.with(|s| s.borrow_mut().target = Some((path, line)));
}
// Deliberately limited to NodeBuilder accessors and explicit copy propagation.
// Direct NodeTable access/iteration is not a complete observation journal.
// Consequently these observations must NEVER authorize publication.
#[track_caller]
pub(crate) fn access(pointer: i64, write: bool) {
    if !crate::latency_diagnostics::local_enabled() {
        return;
    }
    let location = std::panic::Location::caller();
    AUDIT.with(|s| {
        let mut s = s.borrow_mut();
        if s.publication_active_ns.is_some() || !s.watched.contains(&pointer) {
            return;
        }
        let key = format!("{}:{}", location.file(), location.line());
        if s.accesses.len() == 64 && !s.accesses.contains_key(&key) {
            s.dropped += 1;
            return;
        }
        let counts = s.accesses.entry(key).or_default();
        if write {
            counts.1 += 1;
        } else {
            counts.0 += 1;
        }
    });
}
pub(crate) fn copied(source: i64, destination: i64) {
    if !crate::latency_diagnostics::local_enabled() {
        return;
    }
    AUDIT.with(|s| {
        let mut s = s.borrow_mut();
        if s.publication_active_ns.is_none() && s.watched.contains(&source) {
            if s.watched.len() == 65536 {
                s.dropped += 1;
                return;
            }
            s.watched.insert(destination);
            s.copied_watched_nodes += 1;
        }
    });
}
pub(crate) fn retired(pointer: i64) {
    if !crate::latency_diagnostics::local_enabled() {
        return;
    }
    AUDIT.with(|s| {
        s.borrow_mut().watched.remove(&pointer);
    });
}
fn active() -> Option<u64> {
    if !crate::latency_diagnostics::local_enabled() {
        return None;
    }
    let clock = crate::latency_diagnostics::execution_clock()?;
    AUDIT.with(|s| s.borrow().publication_active_ns.is_none().then_some(clock))
}
pub(crate) fn consumer(operation: &'static str) {
    let Some(active_ns) = active() else { return };
    AUDIT.with(|s| {
        let mut s = s.borrow_mut();
        if s.consumers.len() == CONSUMERS {
            s.dropped += 1;
        } else {
            s.consumers.push(Consumer {
                active_ns,
                operation,
            });
        }
    });
}
pub(crate) fn publication() {
    let Some(clock) = crate::latency_diagnostics::execution_clock() else {
        return;
    };
    AUDIT.with(|s| {
        s.borrow_mut().publication_active_ns.get_or_insert(clock);
    });
}
pub(crate) fn snapshot() -> serde_json::Value {
    AUDIT.with(|s| serde_json::json!({
        "record": &*s.borrow(),
        "publication_authorized": false,
        "limits": "32 paragraphs, 8192 nodes per paragraph, 64 selected consumers/access sites, 65536 watched identities, first page only; NodeBuilder access/copy observations omit direct NodeTable access/iteration and are not a complete read journal; clocks exclude slice waits; no state, graph or resource ownership is reused",
    }))
}

pub(crate) fn paragraph(
    builder: &NodeBuilder,
    env: &Environment,
    pages: &PageBuilder,
    roots: &[i64],
    penalties: &[i64],
    ordinal: usize,
    main_cursor_bytes: usize,
    edited_offset: Option<usize>,
) {
    let Some(ready_active_ns) = active() else {
        return;
    };
    if AUDIT.with(|s| s.borrow().paragraphs.len() == PARAGRAPHS) {
        AUDIT.with(|s| s.borrow_mut().dropped += 1);
        return;
    }
    let capture_start = crate::latency_diagnostics::now();
    let output_tokens = match env.values.raw().get(&ValueKey::parameter("output")) {
        Some(Quantity::Tokens(tokens)) => tokens.len(),
        _ => 0,
    };
    let mut p = Paragraph {
        ordinal,
        ready_active_ns,
        capture_wall_ns: 0,
        main_cursor_bytes,
        edited_offset,
        geometry: Vec::new(),
        penalties: penalties.iter().take(NODES).copied().collect(),
        source_lines: BTreeMap::new(),
        truncated_origins: false,
        kinds: BTreeMap::new(),
        fonts: BTreeSet::new(),
        nodes: 0,
        material_miss: (roots.len() > NODES || penalties.len() > NODES)
            .then_some("material_root_bound"),
        construction_miss: if matches!(pages.output_handler, OutputHandler::External) {
            "external_output_handler"
        } else if output_tokens != 0 {
            "tex_output_program_has_no_checked_recipe"
        } else {
            // Even native direct shipment does not prove the intervening input
            // cannot unpack a line, observe it, assign state or add effects.
            "native_direct_output_still_needs_suffix_and_placement_evidence"
        },
        output_tokens,
        page_nodes: pages.page.len(),
        queued_nodes: pages.queue.len(),
        active_output: pages.active,
    };
    for &root in roots.iter().take(NODES) {
        match builder.node(root) {
            Ok(n) => p.geometry.push((n.width, n.height, n.depth, n.shift)),
            Err(_) => p.material_miss = Some("invalid_root"),
        }
    }
    let mut seen = BTreeSet::new();
    // Do not follow root sibling links into surrounding contributions.
    let mut pending: Vec<_> = roots.iter().take(NODES).map(|&r| (r, false)).collect();
    while let Some((pointer, sibling)) = pending.pop() {
        if pointer == C::NULL {
            continue;
        }
        if seen.len() == NODES || !seen.insert(pointer) {
            p.material_miss = Some("material_bound_or_alias");
            break;
        }
        let Ok(n) = builder.node(pointer) else {
            p.material_miss = Some("invalid_node");
            break;
        };
        *p.kinds.entry(format!("{:?}", n.node_type)).or_default() += 1;
        if n.glyph() {
            p.fonts.insert(n.font);
        }
        // Pure glyph/spacing material only. This is a necessary condition for
        // the proposed opaque-line recipe, not permission to reuse its users.
        if !matches!(
            n.node_type,
            NodeKind::Char
                | NodeKind::Ligature
                | NodeKind::Hlist
                | NodeKind::Glue
                | NodeKind::Kern
                | NodeKind::Penalty
        ) || !matches!(n.payload, NodePayload::None)
            || n.shaped.is_some()
            || n.leader_ptr != C::NULL
        {
            p.material_miss = Some("unsupported_material_or_effect");
        }
        for origin in n.origins.iter().flatten() {
            let mut source = origin;
            for _ in 0..16 {
                p.truncated_origins |= source.call_site_truncated;
                let lines = p
                    .source_lines
                    .entry(source.file.to_string())
                    .or_insert((source.line, source.line));
                lines.0 = lines.0.min(source.line);
                lines.1 = lines.1.max(source.line);
                let Some(parent) = &source.call_site else {
                    break;
                };
                source = parent;
            }
        }
        if sibling {
            pending.push((n.link, true));
        }
        for child in [n.list_ptr, n.pre_break, n.post_break, n.leader_ptr] {
            if child != C::NULL {
                pending.push((child, true));
            }
        }
    }
    p.nodes = seen.len();
    p.capture_wall_ns = crate::latency_diagnostics::now().saturating_sub(capture_start);
    AUDIT.with(|s| {
        let mut s = s.borrow_mut();
        if s.watched_ordinal.is_none()
            && s.target.as_ref().is_some_and(|(path, line)| {
                p.source_lines
                    .get(path)
                    .is_some_and(|(first, last)| first <= line && line <= last)
            })
        {
            s.watched_ordinal = Some(p.ordinal);
            s.watched = seen;
        }
        s.paragraphs.push(p);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn watched_access_keeps_its_caller_and_retires_recycled_identities() {
        crate::latency_diagnostics::configure("local");
        AUDIT.with(|s| {
            s.borrow_mut().watched.insert(41);
        });
        let caller_line = line!() + 1;
        access(41, false);
        copied(41, 42);
        retired(42);
        access(42, true);
        AUDIT.with(|s| {
            let s = s.borrow();
            assert_eq!(s.accesses.len(), 1);
            assert_eq!(s.accesses[&format!("{}:{caller_line}", file!())], (1, 0));
            assert!(!s.watched.contains(&42));
        });
        crate::latency_diagnostics::configure("off");
    }
    #[test]
    fn paragraph_probe_does_not_enter_live_dependency_journals() {
        crate::latency_diagnostics::configure("local");
        let env = Environment::default();
        env.values.begin_reads();
        {
            let _slice = crate::latency_diagnostics::execution_slice();
            paragraph(
                &NodeBuilder::new(Default::default()),
                &env,
                &PageBuilder::default(),
                &[],
                &[],
                0,
                0,
                None,
            );
        }
        assert!(env.values.finish_reads().unwrap().is_empty());
        crate::latency_diagnostics::configure("off");
    }
    #[test]
    fn diagnostic_recording_stops_at_publication_and_resets_between_revisions() {
        crate::latency_diagnostics::configure("local");
        reset();
        {
            let _slice = crate::latency_diagnostics::execution_slice();
            consumer("unpack");
            publication();
            consumer("must_not_be_recorded_after_first_publication");
        }
        let s = snapshot();
        assert_eq!(s["publication_authorized"], false);
        assert_eq!(s["record"]["consumers"].as_array().unwrap().len(), 1);
        reset();
        assert!(snapshot()["record"]["publication_active_ns"].is_null());
        crate::latency_diagnostics::configure("off");
    }
}
