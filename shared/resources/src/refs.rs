//! Reference shapes used by the resolver (B.5) and the launcher (B.7).
//!
//! These types are placeholders insofar as the *consumer* code lives in the
//! service and has not yet been written. The shapes themselves are stable —
//! deliberate, simple data structures the rest of the design hangs off of.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A workflow-author-typed reference to a resource by **path** (e.g.
/// `f/team/local_pg`). Paths are scoped to a workspace; resolution happens at
/// instance-launch time, not at workflow compile time.
///
/// `ResourceRef` is what survives in the saved workflow graph. It carries
/// only the human-meaningful identifier so workflows can move between
/// environments (dev → prod) by rebinding aliases at launch without editing
/// the workflow itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceRef {
    /// Workspace-scoped path string, mirrors Windmill's `f/<folder>/<name>`
    /// convention.
    pub path: String,
}

/// The frozen identity of a resource at the moment a workflow instance was
/// launched. Once stored on `workflow_instances.resource_pins` it is
/// immutable for the lifetime of that instance — rotation after launch does
/// not retroactively change a running instance's view of credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourcePin {
    /// FK into `resources.id`.
    pub resource_id: Uuid,
    /// FK into `resource_versions.version` for that resource.
    pub version: i32,
}

/// Output shape produced by the resolver (B.5). One subtree per alias the
/// workflow declared in its `resources:` map.
///
/// * `public_inline` holds the non-secret field values directly so steps can
///   read `db.host` without an extra round-trip.
/// * `secret_refs` holds `{ field_name -> "{{secret:resources/<id>/v<n>#<field>}}" }`.
///   The engine's existing wrap path picks these up because the
///   `extract_secret_keys` regex already matches paths with `/`, `-`, `#`
///   (verified by Risk #7 in the plan and asserted in tests).
///
/// The launcher splices a JSON envelope of the form
/// `{ <alias>: { ...public_inline..., ...secret_refs... } }` into the AIR so
/// downstream backends see a single flat object per alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedResource {
    /// Workflow-level alias the author wrote (e.g. `"db"`).
    pub alias: String,
    /// Wire name of the resource type (matches `ResourceTypeDescriptor.name`).
    pub resource_type: String,
    /// The pin this resolution corresponds to. Echoed for audit and
    /// observability.
    pub pin: ResourcePin,
    /// Inline non-secret values keyed by field name.
    pub public_inline: serde_json::Map<String, serde_json::Value>,
    /// Secret-template references keyed by field name.
    pub secret_refs: serde_json::Map<String, serde_json::Value>,
}
