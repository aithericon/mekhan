//! Shared types for the trigger dispatcher. Kept separate from
//! `models::template` because these are runtime-only — they don't round-trip
//! through `graph_json` and aren't part of the editor's data model.

use chrono::{DateTime, Utc};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::models::template::{TriggerSource, WorkflowEdge, WorkflowNodeData};

/// Discriminator for the *kind of effect* a trigger has. Computed from its
/// outgoing edge target: spawn (target is a Start port → create instance)
/// vs signal (any other port → publish to running instances of the template).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    /// Target is a Start block's input port. Firing the trigger calls the
    /// instance creation handler with a `start_tokens` entry seeding the Start.
    Spawn,
    /// Target is any non-Start input port. Firing publishes an `ExternalSignal`
    /// to every running instance of the template, on the corresponding
    /// `petri.signal.{net_id}.{place}` subject.
    Signal,
}

/// A trigger registered in the dispatcher's in-memory index. Built by
/// scanning every published template's `graph_json`.
#[derive(Debug, Clone)]
pub struct TriggerRecord {
    pub template_id: Uuid,
    pub template_version: i32,
    pub node_id: String,
    pub kind: TriggerKind,
    /// Resolved target node id — either a Start block (Spawn) or a workflow
    /// node carrying the signal port (Signal).
    pub target_node_id: String,
    /// The handle on the target node, e.g. `"in"` for the canonical input port.
    pub target_handle: String,
    pub source: TriggerSource,
    pub enabled: bool,
    pub registered_at: DateTime<Utc>,
}

/// Used in handler responses and history records. Distinguishes a trigger by
/// its (template, node) identity since `node_id` is only unique within a graph.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ToSchema)]
pub struct TriggerLocator {
    pub template_id: Uuid,
    pub template_version: i32,
    pub node_id: String,
}

/// Result of a single fire attempt. Kept structurally similar across sources
/// so the history endpoint can render a uniform table.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ToSchema)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum FireOutcome {
    /// Spawn: an instance was created. `instance_id` is the new instance.
    Spawned { instance_id: Uuid },
    /// Signal: at least one running instance received the signal.
    Signaled { delivered_to: usize },
    /// Signal had no running instances to send to. Not an error — just
    /// reported so the caller knows.
    NoTargets,
    /// Concurrency policy dropped the fire (`Skip`/`DedupKey`).
    Dropped { reason: String },
}

/// Wrap an outcome with metadata the history endpoint records on every fire.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ToSchema)]
pub struct FireResult {
    pub locator: TriggerLocator,
    pub fired_at: DateTime<Utc>,
    pub source_kind: String,
    pub outcome: FireOutcome,
}

#[derive(Debug, thiserror::Error)]
pub enum TriggerError {
    #[error("trigger '{0}' not found in any published template")]
    NotFound(String),
    #[error("trigger '{0}' is disabled")]
    Disabled(String),
    #[error("trigger '{node_id}' resolves to a target node '{target}' which is missing or invalid")]
    TargetMissing { node_id: String, target: String },
    #[error("payload mapping for field '{field}' failed: {message}")]
    PayloadMappingFailed { field: String, message: String },
    #[error("instance creation failed: {0}")]
    InstanceFailed(String),
    #[error("signal publish failed: {0}")]
    SignalFailed(String),
    #[error("database error: {0}")]
    Database(String),
}

/// Helper: find a trigger node in a graph by id, returning the trigger node
/// data and its single outgoing edge. Used during registration and ad-hoc
/// fires alike — keeps the "trigger has exactly one outgoing edge" invariant
/// in one place.
pub fn locate_trigger<'a>(
    nodes: &'a [crate::models::template::WorkflowNode],
    edges: &'a [WorkflowEdge],
    node_id: &str,
) -> Option<(&'a WorkflowNodeData, &'a WorkflowEdge)> {
    let node = nodes.iter().find(|n| n.id == node_id)?;
    if !matches!(node.data, WorkflowNodeData::Trigger { .. }) {
        return None;
    }
    let edge = edges.iter().find(|e| e.source == node_id)?;
    Some((&node.data, edge))
}
