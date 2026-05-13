use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::watch;

/// Tracks the number of completed jobs (success or failure) for drain-mode shutdown.
///
/// Uses an atomic counter for the fast path and a `watch` channel so the drain
/// signal can react to each new completion without polling.
pub struct CompletionTracker {
    count: AtomicU64,
    tx: watch::Sender<u64>,
}

impl CompletionTracker {
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(0u64);
        Self {
            count: AtomicU64::new(0),
            tx,
        }
    }

    /// Record one job completion (regardless of success/failure).
    pub fn record_completion(&self) {
        let new = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        // Ignore send errors — receiver may have been dropped if drain already exited.
        let _ = self.tx.send(new);
    }

    /// Current number of completed jobs.
    pub fn completed(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Subscribe to completion count updates for the drain signal.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.tx.subscribe()
    }
}

impl Default for CompletionTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_completions() {
        let tracker = CompletionTracker::new();
        assert_eq!(tracker.completed(), 0);
        tracker.record_completion();
        assert_eq!(tracker.completed(), 1);
        tracker.record_completion();
        assert_eq!(tracker.completed(), 2);
    }

    #[tokio::test]
    async fn watch_receives_updates() {
        let tracker = CompletionTracker::new();
        let mut rx = tracker.subscribe();

        tracker.record_completion();
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), 1);

        tracker.record_completion();
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), 2);
    }
}
