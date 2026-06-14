//! Library-pack DTOs — the row projection + the self-contained bundle document
//! that `POST /library/packs/import` consumes and `GET /library/packs/export`
//! produces.
//!
//! A "pack" groups the `library_node` templates that ship together under one
//! `vendor/slug` coordinate (see `migrations/20240187000000_library_packs.sql`).
//! It is a control-plane parent: the pack's templates point back via
//! `workflow_templates.pack_id`; the pack carries no graph/AIR of its own.
//!
//! ## Casing
//!
//! Every type here is `#[serde(rename_all = "camelCase")]`. The response DTOs
//! must be camelCase (the SvelteKit client expects it). [`PackBundle`] is
//! *symmetric* — export emits exactly what import accepts — so it uses the same
//! camelCase wire form on both directions. A bundle round-trips through
//! `export → file → import` byte-for-byte on the field names.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// A `library_packs` row, as stored. Read model for the list/detail endpoints.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryPack {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub vendor: String,
    pub slug: String,
    pub version: String,
    pub name: String,
    pub description: String,
    /// Trust axis: `system` | `workspace` | `community`.
    pub origin: String,
    pub installed_by: Option<Uuid>,
    pub installed_at: chrono::DateTime<chrono::Utc>,
}

/// A pack list row: the stored pack plus the count of library nodes it owns and
/// the caller's effective role (so the management view can gate Import/Delete to
/// `admin`+).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryPackSummary {
    #[serde(flatten)]
    pub pack: LibraryPack,
    /// Number of `is_latest` library-node families belonging to this pack.
    pub node_count: i64,
    /// Caller's effective role label (`owner|admin|editor|viewer`) on the
    /// pack's workspace — drives the Import/Delete affordance gate. The backend
    /// still enforces the role on every mutate path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub my_effective_role: Option<String>,
}

/// `GET /library/packs/{id}` — the pack plus its library-node descriptors
/// (the same shape the palette consumes via `node_library`).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryPackDetail {
    #[serde(flatten)]
    pub pack: LibraryPack,
    /// The pack's library nodes (latest version of each family).
    pub nodes: Vec<crate::handlers::node_library::LibraryNodeDescriptor>,
}

/// A self-contained, portable pack document. Export emits it; import consumes
/// it. Symmetric: `export → import` is lossless on the carried fields. The
/// per-node AIR / interface JSON is **not** carried — import RECOMPILES each
/// node's graph through the same compile path the seeder uses, so a bundle can
/// never ship stale or hand-tampered AIR.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackBundle {
    pub manifest: PackManifest,
    pub nodes: Vec<PackNode>,
    /// Logo/icon bytes referenced by `presentation.icon` tokens of the form
    /// `asset:{uuid}`. Empty when no node carries an uploaded logo.
    #[serde(default)]
    pub assets: Vec<PackAsset>,
}

/// Pack-level identity in a [`PackBundle`].
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackManifest {
    pub vendor: String,
    pub slug: String,
    #[serde(default = "default_pack_version")]
    pub version: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

fn default_pack_version() -> String {
    "1".to_string()
}

/// One library node in a [`PackBundle`]. Carries the authored graph verbatim;
/// `air`/`interface` are RECOMPILED on import, never carried.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackNode {
    /// Stable `vendor/slug` coordinate.
    pub coordinate: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Branding blob (`Presentation` JSON: icon/color/vendor/category/badge).
    /// Kept as raw JSON so import can rewrite an `asset:{uuid}` icon token
    /// in-place before persisting, without round-tripping the typed shape.
    pub presentation: serde_json::Value,
    /// `WorkflowGraph` JSON — the authored graph, recompiled on import.
    pub graph: serde_json::Value,
    /// Per-node source files (`node_id` → `filename` → content), e.g. a Python
    /// step's `main.py`. The graph references these by name, so they are
    /// REQUIRED for the import-time recompile to succeed and to re-seed the
    /// editor doc — the graph JSON alone is not self-contained. `#[serde(default)]`
    /// keeps older graph-only bundles parseable (they simply carry no files).
    #[serde(default)]
    pub files: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
}

/// A logo/icon blob embedded in a [`PackBundle`]. `ref` is the
/// `asset:{uuid}` token a node's `presentation.icon` points at; import re-stores
/// the bytes (minting a fresh id) and rewrites every matching icon token.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackAsset {
    /// The `asset:{uuid}` token used as a `presentation.icon` value.
    #[serde(rename = "ref")]
    pub r#ref: String,
    /// MIME type (e.g. `image/svg+xml`, `image/png`).
    pub mime: String,
    /// Base64-encoded bytes.
    pub data_base64: String,
}

/// Response of a successful `POST /library/packs/import`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackImportResult {
    pub pack: LibraryPack,
    pub node_count: i64,
}
