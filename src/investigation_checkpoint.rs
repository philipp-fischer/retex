//! Diagnostic checkpoint selection control; absent from normal builds.
use std::cell::Cell;
thread_local! {static LIMIT:Cell<Option<usize>>=const {Cell::new(None)};}
#[allow(dead_code)] // Used by the Wasm-only investigation interface.
pub fn set(offset: usize) {
    LIMIT.with(|v| v.set(Some(offset)));
}
pub fn allowed(offset: usize) -> bool {
    LIMIT.with(|v| v.get().is_none_or(|limit| offset <= limit))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_limit_is_inclusive_and_optional() {
        LIMIT.with(|v| v.set(None));
        assert!(allowed(usize::MAX));
        set(3917);
        assert!(allowed(3917));
        assert!(!allowed(3918));
        LIMIT.with(|v| v.set(None));
    }
}
