use std::time::Duration;

use tokio::sync::watch;
use tracing::info;

/// Configuration for drain-mode shutdown behavior.
pub struct DrainConfig {
    pub min_jobs: Option<u64>,
    pub max_jobs: Option<u64>,
    pub idle_timeout: Duration,
}

/// Wait until the drain condition is met, then return.
///
/// **Phase 1** (if `min_jobs` is set): block until `completed >= min_jobs`.
/// **Phase 2**: wait for either `completed >= max_jobs` (immediate exit)
/// or an idle timeout with no new completions.
///
/// Returns `Ok(())` when the executor should shut down.
pub async fn drain_signal(mut rx: watch::Receiver<u64>, config: &DrainConfig) {
    // Phase 1: wait for min_jobs
    if let Some(min) = config.min_jobs {
        loop {
            let count = *rx.borrow();
            if count >= min {
                info!(count, min, "min_jobs reached, entering idle phase");
                break;
            }
            // Wait for next completion
            if rx.changed().await.is_err() {
                // Sender dropped — tracker is gone, exit.
                info!("completion tracker dropped, exiting drain");
                return;
            }
        }
    }

    // Phase 2: wait for max_jobs or idle timeout
    loop {
        // Check max_jobs immediately
        if let Some(max) = config.max_jobs {
            let count = *rx.borrow();
            if count >= max {
                info!(count, max, "max_jobs reached, shutting down");
                return;
            }
        }

        // Wait for next completion or idle timeout
        match tokio::time::timeout(config.idle_timeout, rx.changed()).await {
            Ok(Ok(())) => {
                // New completion arrived — loop to check max_jobs again.
                // The idle timer resets automatically on next iteration.
                continue;
            }
            Ok(Err(_)) => {
                // Sender dropped — tracker is gone, exit.
                info!("completion tracker dropped, exiting drain");
                return;
            }
            Err(_) => {
                // Idle timeout elapsed with no new completions.
                let count = *rx.borrow();
                info!(
                    count,
                    idle_timeout_secs = config.idle_timeout.as_secs(),
                    "idle timeout elapsed, shutting down"
                );
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::CompletionTracker;

    #[tokio::test]
    async fn max_jobs_triggers_exit() {
        let tracker = CompletionTracker::new();
        let rx = tracker.subscribe();
        let config = DrainConfig {
            min_jobs: None,
            max_jobs: Some(2),
            idle_timeout: Duration::from_secs(60),
        };

        // Record 2 completions before calling drain_signal
        tracker.record_completion();
        tracker.record_completion();

        // Should return immediately since max_jobs is already met
        tokio::time::timeout(Duration::from_secs(1), drain_signal(rx, &config))
            .await
            .expect("drain_signal should have returned immediately");
    }

    #[tokio::test]
    async fn idle_timeout_triggers_exit() {
        let tracker = CompletionTracker::new();
        let rx = tracker.subscribe();
        let config = DrainConfig {
            min_jobs: None,
            max_jobs: None,
            idle_timeout: Duration::from_millis(50),
        };

        // No completions — should exit after idle timeout
        tokio::time::timeout(Duration::from_secs(2), drain_signal(rx, &config))
            .await
            .expect("drain_signal should have returned after idle timeout");
    }

    #[tokio::test]
    async fn min_jobs_blocks_until_met() {
        let tracker = CompletionTracker::new();
        let rx = tracker.subscribe();
        let config = DrainConfig {
            min_jobs: Some(2),
            max_jobs: None,
            idle_timeout: Duration::from_millis(50),
        };

        // Spawn drain_signal
        let handle = tokio::spawn(async move {
            drain_signal(rx, &config).await;
        });

        // Record 1 completion — should NOT trigger exit yet (min=2)
        tracker.record_completion();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!handle.is_finished(), "should not exit before min_jobs");

        // Record 2nd completion — now min is met, idle timer starts
        tracker.record_completion();

        // Should exit after idle timeout
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("should complete within timeout")
            .expect("task should not panic");
    }

    #[tokio::test]
    async fn completion_resets_idle_timer() {
        let tracker = CompletionTracker::new();
        let rx = tracker.subscribe();
        let config = DrainConfig {
            min_jobs: None,
            max_jobs: None,
            idle_timeout: Duration::from_millis(200),
        };

        let handle = tokio::spawn(async move {
            drain_signal(rx, &config).await;
        });

        // Keep sending completions faster than idle timeout
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            tracker.record_completion();
        }

        // After we stop, idle timeout should trigger
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("should complete within timeout")
            .expect("task should not panic");

        assert_eq!(tracker.completed(), 3);
    }
}
