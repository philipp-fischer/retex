//! Explicit continuation stack with error propagation and reverse cleanup.
use super::errors::Result;

/// Values returned by a particular execution protocol are a typed sum chosen
/// by that protocol. Scanner and execution tasks share that protocol's `V`.
pub enum Step<V> {
    Yield(Box<dyn Continuation<V>>),
    Complete(V),
}

pub trait Continuation<V> {
    fn resume(&mut self, child: Result<Option<V>>) -> Result<Step<V>>;
    fn close(&mut self);
}

pub fn run<V: 'static>(operation: Box<dyn Continuation<V>>) -> Result<V> {
    struct Pending<V>(Vec<Box<dyn Continuation<V>>>);
    impl<V> Drop for Pending<V> {
        fn drop(&mut self) {
            for operation in self.0.iter_mut().rev() {
                operation.close();
            }
        }
    }
    let mut pending = Pending(vec![operation]);
    let mut result = Ok(None);
    while let Some(parent) = pending.0.last_mut() {
        match parent.resume(result) {
            Ok(Step::Yield(child)) => {
                pending.0.push(child);
                result = Ok(None);
            }
            Ok(Step::Complete(value)) => {
                pending.0.pop();
                if pending.0.is_empty() {
                    return Ok(value);
                }
                result = Ok(Some(value));
            }
            Err(error) => {
                pending.0.pop();
                result = Err(error);
            }
        }
    }
    match result {
        Err(error) => Err(error),
        Ok(_) => unreachable!("outer continuation completed without a value"),
    }
}
