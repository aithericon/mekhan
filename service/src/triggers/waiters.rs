//! WaitForResult waiter registry.
//!
//! A `POST /api/triggers/{id}/fire?reply=wait` caller blocks until the spawned
//! instance reaches a terminal state. The fire path registers a one-shot keyed
//! by `instance_id`; the lifecycle consumer resolves it when
//! `net.completed`/`net.cancelled`/`net.failed` lands. The fire path *also*
//! re-checks the row right after registering and resolves synchronously if the
//! net was already terminal by then — closing the create→deploy→terminal race
//! (the consumer's resolve was a no-op because no waiter existed yet). Resolve
//! is idempotent: first writer wins, later calls are no-ops.

use std::sync::Arc;

use dashmap::DashMap;
use serde::Serialize;
use tokio::sync::oneshot;
use utoipa::ToSchema;
use uuid::Uuid;

/// The terminal disposition handed back to a WaitForResult caller.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TerminalOutcome {
    /// Net terminal status: `completed` | `cancelled` | `failed`.
    pub status: String,
    /// The structured result envelope (the same value persisted to
    /// `workflow_instances.result`), or absent when none was produced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

/// Process-wide registry of in-flight WaitForResult waiters. Held in
/// `AppState` and shared (by `Arc`) with the lifecycle consumer.
#[derive(Default)]
pub struct ResultWaiters {
    inner: DashMap<Uuid, oneshot::Sender<TerminalOutcome>>,
}

impl ResultWaiters {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Register interest in `instance_id`'s terminal outcome. The returned
    /// receiver resolves once, or errors if the sender is dropped without a
    /// send (the handler treats that as "degrade to polling").
    pub fn register(&self, instance_id: Uuid) -> oneshot::Receiver<TerminalOutcome> {
        let (tx, rx) = oneshot::channel();
        self.inner.insert(instance_id, tx);
        rx
    }

    /// Deliver the terminal outcome to a waiter if one is registered. No-op
    /// when absent (every terminal net calls this; only waited ones have an
    /// entry) or already resolved (first writer wins).
    pub fn resolve(&self, instance_id: &Uuid, outcome: TerminalOutcome) {
        if let Some((_, tx)) = self.inner.remove(instance_id) {
            let _ = tx.send(outcome);
        }
    }

    /// Drop a waiter without resolving it (handler timeout / client
    /// disconnect) so the sender doesn't leak.
    pub fn deregister(&self, instance_id: &Uuid) {
        self.inner.remove(instance_id);
    }

    /// True when no waiters are registered — lets the lifecycle consumer skip
    /// its `net_id`→`id` lookup on the common (no-waiter) terminal path.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}
