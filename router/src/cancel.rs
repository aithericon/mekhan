//! Cancellation registry + NATS listener.
//!
//! Structurally a copy of `executor-worker/src/cancel.rs`, retargeted to the
//! inference subjects: a `request_id → CancellationToken` registry, a
//! **core-NATS** (not JetStream — cancellation is ephemeral) subscriber on
//! `inference.cancel.{request_id}` that point-looks-up and cancels, and a
//! `inference.cancelled.{request_id}` confirmation publish (doc 11 §5.5).
//!
//! HTTP-disconnect is the *second* cancellation trigger and is handled in the
//! proxy by the response body future being dropped (which releases the
//! admission permit); the [`DeregisterGuard`] makes registry cleanup automatic
//! on every terminal path — completion, NATS-cancel, disconnect, or error.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// `request_id → CancellationToken`. Cheap point-lookups under a std `Mutex`.
#[derive(Clone, Default)]
pub struct CancellationRegistry {
    inner: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl CancellationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, request_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        self.inner
            .lock()
            .unwrap()
            .insert(request_id.to_string(), token.clone());
        token
    }

    pub fn deregister(&self, request_id: &str) {
        self.inner.lock().unwrap().remove(request_id);
    }

    /// Cancel a request by id. Returns `true` if it was live.
    pub fn cancel(&self, request_id: &str) -> bool {
        let map = self.inner.lock().unwrap();
        if let Some(token) = map.get(request_id) {
            token.cancel();
            true
        } else {
            false
        }
    }

    pub fn active_count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

/// RAII guard that deregisters a request on drop — so completion, cancel,
/// disconnect, and error paths all clean up without explicit calls.
pub struct DeregisterGuard {
    registry: CancellationRegistry,
    request_id: String,
}

impl DeregisterGuard {
    pub fn new(registry: CancellationRegistry, request_id: String) -> Self {
        Self {
            registry,
            request_id,
        }
    }
}

impl Drop for DeregisterGuard {
    fn drop(&mut self) {
        self.registry.deregister(&self.request_id);
    }
}

pub const CANCEL_SUBJECT_FILTER: &str = "inference.cancel.*";

/// Subscribe to `inference.cancel.*` and trigger the matching token.
pub async fn spawn_cancel_listener(
    client: async_nats::Client,
    registry: CancellationRegistry,
    shutdown: CancellationToken,
) -> Result<JoinHandle<()>, async_nats::SubscribeError> {
    let mut subscription = client.subscribe(CANCEL_SUBJECT_FILTER).await?;
    info!(
        subject = CANCEL_SUBJECT_FILTER,
        "inference cancel listener started"
    );
    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    info!("inference cancel listener shutting down");
                    break;
                }
                msg = subscription.next() => match msg {
                    Some(msg) => {
                        if let Some(request_id) = msg.subject.as_str().split('.').next_back() {
                            if registry.cancel(request_id) {
                                info!(%request_id, "inference cancellation triggered via NATS");
                            } else {
                                debug!(%request_id, "cancel for unknown request (already finished?)");
                            }
                        }
                    }
                    None => {
                        warn!("inference cancel subscription closed");
                        break;
                    }
                }
            }
        }
    });
    Ok(handle)
}

/// Publish the `inference.cancelled.{request_id}` confirmation (doc 11 §5.5).
pub async fn publish_cancelled(client: &async_nats::Client, request_id: &str) {
    let subject = format!("inference.cancelled.{request_id}");
    if let Err(e) = client.publish(subject, Vec::<u8>::new().into()).await {
        warn!(%request_id, error = %e, "failed to publish inference.cancelled");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_cancel_deregister() {
        let reg = CancellationRegistry::new();
        let token = reg.register("req-1");
        assert!(!token.is_cancelled());
        assert!(reg.cancel("req-1"));
        assert!(token.is_cancelled());
        reg.deregister("req-1");
        assert_eq!(reg.active_count(), 0);
        assert!(!reg.cancel("req-1"));
    }

    #[test]
    fn guard_deregisters_on_drop() {
        let reg = CancellationRegistry::new();
        let _token = reg.register("req-1");
        {
            let _guard = DeregisterGuard::new(reg.clone(), "req-1".to_string());
            assert_eq!(reg.active_count(), 1);
        }
        assert_eq!(reg.active_count(), 0);
    }
}
