//! Cooperative cancellation scoped to one explicit update operation.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[derive(Clone, Default)]
pub(crate) struct Token(Arc<AtomicBool>);
impl Token {
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub(crate) fn cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        if self.cancelled() {
            Err("update operation cancelled".into())
        } else {
            Ok(())
        }
    }
    #[cfg(test)]
    pub(crate) fn lifetime(&self) -> std::sync::Weak<AtomicBool> {
        Arc::downgrade(&self.0)
    }
}
