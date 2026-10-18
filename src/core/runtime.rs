//! Ownership roots and exception-safe temporary state (`core/runtime.py`).
use std::cell::RefCell;
use std::rc::Rc;

pub type NodeId = i64;
pub type RootProvider = Box<dyn Fn() -> Vec<NodeId>>;

struct RootSet {
    roots: Rc<RefCell<Vec<NodeId>>>,
    component: bool,
}

#[derive(Default)]
pub struct OwnershipRoots {
    providers: Vec<RootProvider>,
    transfers: Rc<RefCell<Vec<RootSet>>>,
}
impl OwnershipRoots {
    pub(crate) fn checkpoint_ready(&self) -> bool {
        self.providers.is_empty() && self.transfers.borrow().len() == 1
    }
    pub(crate) fn checkpoint_ready_for(&self, transfers: usize) -> bool {
        self.providers.is_empty() && self.transfers.borrow().len() == transfers
    }

    pub fn register(&mut self, provider: RootProvider) {
        self.providers.push(provider);
    }
    pub fn collect(&self) -> Vec<NodeId> {
        self.collect_live(|_| true)
    }
    pub(crate) fn collect_live(&self, exists: impl Fn(NodeId) -> bool) -> Vec<NodeId> {
        let mut roots: Vec<_> = self
            .providers
            .iter()
            .flat_map(|provider| provider())
            .collect();
        for transfer in self.transfers.borrow().iter() {
            roots.extend(
                transfer
                    .roots
                    .borrow()
                    .iter()
                    .copied()
                    .filter(|&root| !transfer.component || exists(root)),
            );
        }
        roots
    }
    pub fn hold(&self, roots: impl IntoIterator<Item = NodeId>) -> RootHold {
        self.hold_roots(roots, false)
    }
    /// Mirror Python's math/alignment providers while their component is moved
    /// into an owned continuation. Those providers expose only surviving nodes;
    /// ordinary transfer holds remain strict and report invalid identities.
    pub(crate) fn hold_component(&self, roots: impl IntoIterator<Item = NodeId>) -> RootHold {
        self.hold_roots(roots, true)
    }
    fn hold_roots(&self, roots: impl IntoIterator<Item = NodeId>, component: bool) -> RootHold {
        let roots = Rc::new(RefCell::new(roots.into_iter().collect()));
        self.transfers.borrow_mut().push(RootSet {
            roots: roots.clone(),
            component,
        });
        RootHold {
            roots,
            transfers: self.transfers.clone(),
        }
    }
}

pub struct RootHold {
    pub roots: Rc<RefCell<Vec<NodeId>>>,
    transfers: Rc<RefCell<Vec<RootSet>>>,
}
impl Drop for RootHold {
    fn drop(&mut self) {
        self.transfers
            .borrow_mut()
            .retain(|set| !Rc::ptr_eq(&set.roots, &self.roots));
    }
}

/// A scoped replacement of a typed component state. `Drop` restores it during
/// either success or panic; ordinary failures use Rust's `Result` unwinding.
pub struct TemporaryState<'a, T> {
    slot: &'a mut T,
    previous: Option<T>,
}
impl<'a, T> TemporaryState<'a, T> {
    pub fn new(slot: &'a mut T, replacement: T) -> Self {
        let previous = std::mem::replace(slot, replacement);
        Self {
            slot,
            previous: Some(previous),
        }
    }
}
impl<T> std::ops::Deref for TemporaryState<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.slot
    }
}
impl<T> std::ops::DerefMut for TemporaryState<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.slot
    }
}
impl<T> Drop for TemporaryState<'_, T> {
    fn drop(&mut self) {
        *self.slot = self
            .previous
            .take()
            .expect("temporary state restored twice");
    }
}
