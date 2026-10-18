//! Initialized state and external-resource provenance.
pub mod formats;
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) mod os_string;
pub mod provenance;
