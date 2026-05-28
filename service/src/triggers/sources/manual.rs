//! Manual trigger source (Phase 5a).
//!
//! The manual source has no background listener — it fires synchronously from
//! the `POST /api/v1/triggers/{node_id}/fire` handler. The handler hands the
//! request body verbatim to `TriggerDispatcher::fire`, which evaluates the
//! trigger's `payload_mapping` against it as the `payload` scope.
//!
//! This module is intentionally tiny in 5a — its purpose is to give the other
//! source modules a stable shape to follow.

use serde_json::Value;
use tokio::sync::oneshot;

use crate::triggers::dispatcher::TriggerDispatcher;
use crate::triggers::model::{FireResult, TriggerError};
use crate::triggers::waiters::{ResultWaiters, TerminalOutcome};

/// Fire a manual trigger. Equivalent to calling `dispatcher.fire(node_id,
/// payload, dispatch_options)` directly — kept as a free function so the
/// per-source modules all look uniform. `dispatch_options` carries γ.mekhan
/// ablation (#126.2) for research-harness-driven fires; ordinary HTTP fires
/// pass `DispatchOptions::default()`.
pub async fn fire(
    dispatcher: &TriggerDispatcher,
    node_id: &str,
    payload: Value,
    dispatch_options: petri_api_types::DispatchOptions,
) -> Result<FireResult, TriggerError> {
    dispatcher.fire(node_id, payload, dispatch_options).await
}

/// Fire a manual trigger in WaitForResult mode: a Spawn additionally
/// registers a terminal-outcome waiter whose receiver is returned alongside
/// the `FireResult` (always `None` for Signal-kind fires).
pub async fn fire_waiting(
    dispatcher: &TriggerDispatcher,
    node_id: &str,
    payload: Value,
    dispatch_options: petri_api_types::DispatchOptions,
    waiters: &ResultWaiters,
) -> Result<(FireResult, Option<oneshot::Receiver<TerminalOutcome>>), TriggerError> {
    dispatcher
        .fire_waiting(node_id, payload, dispatch_options, waiters)
        .await
}
