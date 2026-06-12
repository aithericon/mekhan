//! HTTP request/response DTOs for the template endpoints.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::graph::{FieldMapping, WorkflowGraph, WorkflowTemplate};
use super::triggers::TriggerSource;

// --- API request/response types ---

/// Request body for stateless compilation. Used by `POST /api/v1/compile` and
/// `POST /api/v1/templates/{id}/compile`. `files` is a per-node, per-filename map
/// of inline contents; the preview compile emits `InputSource::Raw` entries so
/// the AIR matches the StoragePath-keyed shape produced by publish.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CompileRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub graph: WorkflowGraph,
    #[serde(default)]
    pub files: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    /// Workspace the draft belongs to. When present, `POST /api/v1/analyze`
    /// resolves workspace-scoped **resources** referenced by the graph so the
    /// editor picker / diagnostics see resource public fields (`<resource>.<field>`)
    /// as a known "Globals" scope instead of a false unresolved. Absent on the
    /// stateless `/api/v1/compile` path (which has no DB context).
    #[serde(default)]
    pub workspace_id: Option<uuid::Uuid>,
    /// Template the draft belongs to. When present, `/api/v1/analyze` resolves
    /// template-visible **assets** referenced by the graph (`<asset>.<field>`)
    /// into the same "Globals" scope.
    #[serde(default)]
    pub template_id: Option<uuid::Uuid>,
}

/// Git provenance recorded on a version published via `mekhan apply`.
/// Serialized into the `workflow_templates.source_ref` JSONB column.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SourceRef {
    /// Git remote URL (`git remote get-url origin`).
    pub remote: String,
    /// Commit SHA the artifact was applied from (`git rev-parse HEAD`).
    pub sha: String,
    /// Working tree had uncommitted changes at apply time
    /// (`git status --porcelain` non-empty).
    pub dirty: bool,
    /// Branch / ref name, when resolvable (`git rev-parse --abbrev-ref HEAD`).
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
}

/// Request body for `POST /api/v1/templates/{id}/apply` — the GitOps path.
/// The `graph` REPLACES the chain head wholesale (no CRDT merge); binary
/// assets are uploaded out-of-band via the files endpoint before this call.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ApplyTemplateRequest {
    pub graph: WorkflowGraph,
    #[serde(default)]
    pub files: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    #[serde(default)]
    pub source_ref: Option<SourceRef>,
}

/// Trigger spec embedded in a `POST /api/templates/apply-air` request.
/// The endpoint synthesizes a `WorkflowGraph` stub containing only this
/// Trigger node so that `register_triggers` (which walks `template.graph`)
/// finds it post-commit. Direct AIR-place binding via
/// `air_target_place_id` — no graph edge.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PreAirTriggerSpec {
    /// Stable, globally-unique node id used in `POST /api/triggers/{node_id}/fire`
    /// URLs. Author-controlled (e.g. `"trg_di_extraction_v1"`).
    pub node_id: String,
    pub label: String,
    /// Trigger source. Clinic's initial use case is `Manual`; other sources
    /// are valid here too (Webhook, Cron, ...) — the dispatcher resolves
    /// them identically once the trigger is registered.
    pub source: TriggerSource,
    #[serde(default)]
    pub payload_mapping: Vec<FieldMapping>,
    /// The AIR place id whose `initial_tokens` will be seeded with the
    /// fire payload + system fields. Must exist in the supplied AIR's
    /// `places[]` — validated at fire time by `parameterize_for_place`.
    pub air_target_place_id: String,
    /// Whether the trigger is live post-apply. Explicit (no default) so
    /// the deploy recipe must state intent — a disabled trigger never
    /// fires even if registered.
    pub enabled: bool,
}

/// Request body for `POST /api/templates/apply-air` — clinic-style
/// headless template upload.
///
/// Accepts pre-compiled AIR directly: no `WorkflowGraph` compile pass,
/// no Y.Doc init, no S3 file upload. The supplied `air_json` is stored
/// verbatim into the `air_json` column; a synthetic stub graph (one
/// Trigger node, no edges) is stored into the `graph` column so the
/// trigger dispatcher's `register_triggers` finds it.
///
/// Idempotency: name-based. Re-apply with the same `name` Bumps the
/// chain (new version row, prior version's triggers forgotten); first
/// apply Seeds (fresh chain at v1).
#[derive(Debug, Deserialize, ToSchema)]
pub struct ApplyAirTemplateRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Pre-compiled AIR. Stored verbatim. The endpoint runs no compile
    /// pass; the AIR is consumed by the engine at trigger-fire time.
    pub air_json: serde_json::Value,
    pub trigger: PreAirTriggerSpec,
    /// Optional git provenance, recorded into `source_ref` exactly like
    /// the existing GitOps `apply` endpoint.
    #[serde(default)]
    pub source_ref: Option<SourceRef>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateTemplateRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub graph: Option<WorkflowGraph>,
    /// Optional per-node file map (filename → inline contents). Files are
    /// seeded as Y.Text entries inside each node's `files` Y.Map so that the
    /// new template lands ready-to-publish for backends that require
    /// attached scripts (e.g. Python's entrypoint).
    #[serde(default)]
    pub files: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateTemplateRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub graph: Option<WorkflowGraph>,
}

/// Template-specific list parameters layered on top of the generic
/// `crate::query::QueryParams` extractor (which owns `page`/`page_size`/`sort`/
/// `search`/`filter[field][op]`). These are the relational & security filters
/// that don't reduce to a plain column predicate: `folder_id`/`tag` are
/// relational joins, `base_template_id` switches the listing into version-chain
/// mode, and `owner_template_id` toggles private sub-workflow visibility.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct TemplateListExtras {
    /// Version-chain mode: list every version of this base family (ignoring
    /// `is_latest`) instead of the default latest-only catalogue listing.
    pub base_template_id: Option<Uuid>,
    /// Restrict to templates homed in this folder (via
    /// `template_folders.base_template_id`). With `recursive=true` the filter
    /// covers the whole subtree rooted at the folder; otherwise only direct
    /// members. The live `is_latest` row wins.
    pub folder_id: Option<Uuid>,
    /// When a `folder_id` is supplied, include templates homed anywhere in the
    /// folder's subtree (matched by materialized-path prefix) rather than only
    /// its direct members.
    #[serde(default)]
    pub recursive: bool,
    /// Restrict to templates carrying this tag in the user's workspace.
    pub tag: Option<String>,
    /// Enumerate the private sub-workflow children owned by this parent
    /// family (`COALESCE(base_template_id, id)`). When supplied, the listing
    /// returns *only* those private children (they're otherwise hidden from
    /// the catalogue). When absent, private templates are excluded entirely.
    pub owner_template_id: Option<Uuid>,
}

/// Response of `DELETE /api/v1/templates/{id}/draft`. Distinguishes the two
/// discard outcomes so the editor knows where to navigate next.
#[derive(Debug, Serialize, ToSchema)]
pub struct DiscardDraftResponse {
    /// True when the discarded draft was the only version in its chain — the
    /// whole template was deleted (there was no parent to fall back to).
    pub template_deleted: bool,
    /// The parent version restored as the chain head (`is_latest = TRUE`).
    /// `None` exactly when `template_deleted` is true.
    pub restored_head: Option<WorkflowTemplate>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PaginatedResponse<T: ToSchema> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_ref_jsonb_roundtrip() {
        // What `apply` serializes into the `source_ref` JSONB column. `ref`
        // is renamed and omitted when None; `dirty` is always present.
        let sr = SourceRef {
            remote: "git@forge.aithericon.eu:Milan/wf.git".to_string(),
            sha: "a1b2c3d4".to_string(),
            dirty: true,
            git_ref: Some("main".to_string()),
        };
        let v = serde_json::to_value(&sr).unwrap();
        assert_eq!(v["remote"], "git@forge.aithericon.eu:Milan/wf.git");
        assert_eq!(v["sha"], "a1b2c3d4");
        assert_eq!(v["dirty"], true);
        assert_eq!(v["ref"], "main");
        let back: SourceRef = serde_json::from_value(v).unwrap();
        assert_eq!(back.sha, "a1b2c3d4");
        assert_eq!(back.git_ref.as_deref(), Some("main"));

        let none = SourceRef {
            remote: "r".to_string(),
            sha: "s".to_string(),
            dirty: false,
            git_ref: None,
        };
        let v = serde_json::to_value(&none).unwrap();
        assert!(v.get("ref").is_none(), "ref must be omitted when None");
        let back: SourceRef = serde_json::from_value(v).unwrap();
        assert_eq!(back.git_ref, None);
    }
}
