//! Per-job human diagnostics and passive versioned execution evidence.
pub mod classic;
pub mod events;
pub mod inspector;
pub mod messages;
pub mod snapshot;
pub use events::TraceRecorder;
pub use messages::Diagnostics;
