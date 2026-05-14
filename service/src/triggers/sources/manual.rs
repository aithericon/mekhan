//! Manual trigger source (Phase 5a).
//!
//! The manual source has no background listener — it fires synchronously from
//! the `POST /api/triggers/{node_id}/fire` handler. The handler hands the
//! request body verbatim to `TriggerDispatcher::fire`, which evaluates the
//! trigger's `payload_mapping` against it as the `payload` scope.
//!
//! This module is intentionally tiny in 5a — its purpose is to give the other
//! source modules a stable shape to follow.

use serde_json::Value;

use crate::triggers::dispatcher::TriggerDispatcher;
use crate::triggers::model::{FireResult, TriggerError};

/// Fire a manual trigger. Equivalent to calling `dispatcher.fire(node_id,
/// payload)` directly — kept as a free function so the per-source modules all
/// look uniform.
pub async fn fire(
    dispatcher: &TriggerDispatcher,
    node_id: &str,
    payload: Value,
) -> Result<FireResult, TriggerError> {
    dispatcher.fire(node_id, payload).await
}
