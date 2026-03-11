use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::clock::Clock;
use crate::storage::memory::MemoryStorage;

/// Handle to the background garbage collection task.
///
/// The GC task is aborted when this handle is dropped.
pub struct GcHandle {
    handle: JoinHandle<()>,
}

impl GcHandle {
    /// Spawn a background task that periodically cleans expired entries.
    pub fn spawn(
        storage: Arc<MemoryStorage>,
        clock: Arc<dyn Clock>,
        interval: Duration,
    ) -> Self {
        let handle = tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            loop {
                tick.tick().await;
                let now = clock.now();
                storage.retain_active(now);
            }
        });
        Self { handle }
    }
}

impl Drop for GcHandle {
    fn drop(&mut self) {
        self.handle.abort();
    }
}
