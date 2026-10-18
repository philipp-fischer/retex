//! Deterministic ligature-loop detection (`ligature_cycles.py`).
use crate::core::errors::{Error, Result};
use std::collections::HashSet;
use std::hash::Hash;
pub struct PumpingGuard<S> {
    pub label: String,
    seen: HashSet<S>,
    levels: Vec<(usize, Vec<S>)>,
}
impl<S: Clone + Eq + Hash> PumpingGuard<S> {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            seen: HashSet::new(),
            levels: Vec::new(),
        }
    }
    pub fn visit(&mut self, signature: S, depth: usize) -> Result<()> {
        while self.levels.last().is_some_and(|&(level, _)| level > depth) {
            let (_, signatures) = self.levels.pop().expect("retained baseline");
            for signature in signatures {
                self.seen.remove(&signature);
            }
        }
        if !self.seen.insert(signature.clone()) {
            return Err(Error::value(format!("Nonterminating {}", self.label)));
        }
        if !self.levels.last().is_some_and(|&(level, _)| level == depth) {
            self.levels.push((depth, Vec::new()));
        }
        self.levels
            .last_mut()
            .expect("current baseline")
            .1
            .push(signature);
        Ok(())
    }
}
