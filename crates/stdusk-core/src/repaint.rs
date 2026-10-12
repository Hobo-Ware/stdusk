//! How a background thread asks the UI for another frame without knowing which UI toolkit runs.

use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct Repaint(Arc<dyn Fn(Duration) + Send + Sync>);

impl Repaint {
    pub fn new(request: impl Fn(Duration) + Send + Sync + 'static) -> Self {
        Self(Arc::new(request))
    }

    pub fn none() -> Self {
        Self::new(|_| {})
    }

    pub fn now(&self) {
        (self.0)(Duration::ZERO);
    }

    pub fn after(&self, delay: Duration) {
        (self.0)(delay);
    }
}
