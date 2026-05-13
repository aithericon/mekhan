//! Reconnect loop with exponential backoff and graceful shutdown.
//!
//! Provides a generic reconnect pattern for scheduler watchers that need
//! to maintain a long-lived connection to an external system.

use std::future::Future;
use std::time::Duration;

use tokio::sync::broadcast;

/// Run an async operation in a reconnect loop with exponential backoff.
///
/// Calls `connect_and_stream` repeatedly. On success (Ok), backoff resets to 1s.
/// On error, the error is logged and the loop waits with exponential backoff
/// (1s -> 2s -> 4s -> ... -> 20s max) before retrying.
///
/// The loop exits when the `shutdown` receiver fires.
///
/// # Arguments
/// * `shutdown` - Broadcast receiver for graceful shutdown signaling
/// * `label` - Human-readable name for log messages (e.g., "Nomad", "Slurm")
/// * `connect_and_stream` - Async closure that connects and streams events
pub async fn run_with_reconnect<F, Fut, E>(
    mut shutdown: broadcast::Receiver<()>,
    label: &str,
    mut connect_and_stream: F,
) where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(20);

    loop {
        tracing::info!("{} watcher connecting", label);

        tokio::select! {
            result = connect_and_stream() => {
                match result {
                    Ok(()) => {
                        tracing::info!("{} event stream ended normally", label);
                        backoff = Duration::from_secs(1);
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            backoff_secs = backoff.as_secs(),
                            "{} event stream error, reconnecting", label
                        );
                    }
                }
            }
            _ = shutdown.recv() => {
                tracing::info!("{} watcher shutting down", label);
                return;
            }
        }

        // Wait before reconnecting
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = shutdown.recv() => {
                tracing::info!("{} watcher shutting down during backoff", label);
                return;
            }
        }

        backoff = (backoff * 2).min(max_backoff);
    }
}
