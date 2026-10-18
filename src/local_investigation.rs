//! Optional observations only. None of these records participates in reuse.
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

pub(crate) fn material(
    builder: &crate::typesetting::nodes::NodeBuilder,
    roots: &[i64],
) -> serde_json::Value {
    use crate::core::constants::Constants as C;
    // Stay inside the same native Preparation/Validation observation contract
    // as the caller. Only record a diagnostic projection; no admission changes.
    let mut seen = std::collections::BTreeSet::new();
    let mut sources = std::collections::BTreeMap::<String, (usize, usize)>::new();
    let mut pending: Vec<_> = roots.iter().map(|&r| (r, false)).collect();
    let shapes: Vec<_> = roots
        .iter()
        .filter_map(|&r| {
            builder
                .node(r)
                .ok()
                .map(crate::checked_material::Shape::new)
        })
        .collect();
    while let Some((id, siblings)) = pending.pop() {
        if id == C::NULL || !seen.insert(id) || seen.len() > 16384 {
            continue;
        }
        let Ok(n) = builder.node(id) else {
            continue;
        };
        for s in n.origins.iter().flatten() {
            let lines = sources.entry(s.file.to_string()).or_insert((usize::MAX, 0));
            lines.0 = lines.0.min(s.line);
            lines.1 = lines.1.max(s.line);
        }
        if siblings {
            pending.push((n.link, true));
        }
        pending.push((n.list_ptr, true));
    }
    serde_json::json!({"sources":sources,"shapes":shapes,"visited_nodes":seen.len()})
}
