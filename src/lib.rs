//! Fresh, typed translation of the standalone Python pdfTeX engine.
//!
//! Module ownership follows `pdftex_engine`; this crate does not depend on the
//! earlier `retex-rs` port or invoke a Python interpreter.

pub(crate) mod checked_material;
pub mod core;
pub mod fonts;
pub mod language;
pub(crate) mod local_investigation;
pub(crate) mod local_page;
pub mod paragraph_preview;
pub mod pdf;
pub mod resources;
pub mod typesetting;

pub mod cli;
pub mod diagnostics;
pub mod engine;
#[cfg(feature = "latency-diagnostics")]
pub(crate) mod latency_diagnostics;
#[cfg(feature = "latency-diagnostics")]
pub(crate) mod local_update_audit;
pub mod model;
#[cfg(feature = "latency-diagnostics")]
pub(crate) mod output_diagnostics;
pub mod serialization;
pub mod session;
pub mod source_map;
pub use engine::{Engine, PDFTeXEngine};
#[cfg(target_arch = "wasm32")]
pub mod wasm;

#[cfg(any(feature = "latency-diagnostics", feature = "output-replay-control"))]
pub(crate) mod investigation_checkpoint;
