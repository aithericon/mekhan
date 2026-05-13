use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Shared registry mapping execution_id → CancellationToken.
///
/// Thread-safe via `Mutex<HashMap>`. Contention is minimal (register on job start,
/// deregister on job end, cancel is rare and point-lookup only), so a std Mutex
/// with trivially short critical sections is sufficient.
#[derive(Clone, Default)]
pub struct CancellationRegistry {
    inner: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl CancellationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new token for an execution. Returns the token to pass to the backend.
    ///
    /// If a token already existed for this execution_id, it is replaced.
    pub fn register(&self, execution_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        let mut map = self.inner.lock().unwrap();
        map.insert(execution_id.to_string(), token.clone());
        token
    }

    /// Deregister a token (called when execution finishes, regardless of outcome).
    pub fn deregister(&self, execution_id: &str) {
        let mut map = self.inner.lock().unwrap();
        map.remove(execution_id);
    }

    /// Cancel an execution by ID. Returns `true` if the execution was found and
    /// cancelled, `false` if not found (already finished or never existed).
    pub fn cancel(&self, execution_id: &str) -> bool {
        let map = self.inner.lock().unwrap();
        if let Some(token) = map.get(execution_id) {
            token.cancel();
            true
        } else {
            false
        }
    }

    /// Number of currently active (registered) executions.
    pub fn active_count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

/// Listens on core NATS `executor.cancel.{execution_id}` subjects and
/// triggers cancellation via the registry.
///
/// Uses core NATS subscribe (not JetStream) — cancellation is ephemeral.
/// If the executor is down when a cancel message is sent, the message is lost.
/// This is correct: the execution either already finished or will be retried.
pub struct NatsCancelListener;

impl NatsCancelListener {
    /// Start listening for cancel messages. Returns a `JoinHandle` for the listener task.
    ///
    /// `prefix` follows the same convention as `StatusReporter.subject_prefix`:
    ///   - `None`  → subscribes to `executor.cancel.*`
    ///   - `Some("pfx")` → subscribes to `pfx.executor.cancel.*`
    pub async fn start(
        client: async_nats::Client,
        registry: CancellationRegistry,
        prefix: Option<&str>,
        shutdown: CancellationToken,
    ) -> Result<JoinHandle<()>, async_nats::SubscribeError> {
        let subject = match prefix {
            Some(pfx) => format!("{pfx}.executor.cancel.*"),
            None => "executor.cancel.*".into(),
        };

        let mut subscription = client.subscribe(subject.clone()).await?;
        info!(%subject, "NATS cancel listener started");

        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => {
                        info!("NATS cancel listener shutting down");
                        break;
                    }
                    msg = subscription.next() => {
                        match msg {
                            Some(msg) => {
                                if let Some(execution_id) = msg.subject.as_str().split('.').next_back() {
                                    let found = registry.cancel(execution_id);
                                    if found {
                                        info!(%execution_id, "cancellation triggered via NATS");
                                    } else {
                                        debug!(
                                            %execution_id,
                                            "cancel request for unknown execution (already finished?)"
                                        );
                                    }
                                }
                            }
                            None => {
                                warn!("NATS cancel subscription closed");
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_cancel() {
        let registry = CancellationRegistry::new();
        let token = registry.register("exec-1");
        assert!(!token.is_cancelled());
        assert_eq!(registry.active_count(), 1);

        let found = registry.cancel("exec-1");
        assert!(found);
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancel_unknown_is_noop() {
        let registry = CancellationRegistry::new();
        let found = registry.cancel("nonexistent");
        assert!(!found);
    }

    #[test]
    fn deregister_removes_token() {
        let registry = CancellationRegistry::new();
        let _token = registry.register("exec-1");
        assert_eq!(registry.active_count(), 1);

        registry.deregister("exec-1");
        assert_eq!(registry.active_count(), 0);

        let found = registry.cancel("exec-1");
        assert!(!found);
    }

    #[test]
    fn register_replaces_existing() {
        let registry = CancellationRegistry::new();
        let token1 = registry.register("exec-1");
        let token2 = registry.register("exec-1");
        assert_eq!(registry.active_count(), 1);

        // Cancelling should affect the new token
        registry.cancel("exec-1");
        assert!(token2.is_cancelled());
        // Old token is no longer tracked, but was not cancelled via registry
        assert!(!token1.is_cancelled());
    }
}
