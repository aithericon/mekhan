//! Pages — free-form collaborative rich-text documents.
//!
//! A page either rides a host entity 1:1 (a "Notes" tab on a template, a
//! "Report" tab on an instance — `attached_kind` + `attached_id`) or lives
//! free-standing inside a folder (`folder_id`). Exactly one placement is set
//! (DB `pages_placement_xor` CHECK). The rich content lives entirely in the
//! generalized Yjs stack (`yjs_documents`/`yjs_snapshots` WHERE
//! `doc_kind = 'page'`, keyed on `pages.id`) — this row is metadata + placement
//! only; rich content never travels through a REST payload.
//!
//! Permissions inherit from the host: a page's effective role IS its host's
//! effective role (resolved via `page_host_ref` in `handlers/pages.rs`). There
//! is no `ObjectKind::Page` and no per-page grant row.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// A page row. `attached_kind`/`attached_id` (singleton tab on a template or
/// instance) and `folder_id` (free page) are mutually exclusive.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct Page {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub title: String,
    /// `template` or `instance` for an attached page; `None` for a free page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached_kind: Option<String>,
    /// The host id: a template chain-root id (D5) or an instance id. Polymorphic,
    /// no FK. `None` for a free page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached_id: Option<Uuid>,
    /// Home folder for a free page; `None` for an attached page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<Uuid>,
    pub created_by: Uuid,
    pub updated_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The caller's effective object role on this page's HOST
    /// (`owner|admin|editor|viewer`), stamped by the list/get handlers. NOT a
    /// database column — `#[sqlx(default)]` lets the `SELECT *` row map satisfy
    /// `FromRow`; the handler fills it in. Lets the SPA gate edit affordances.
    /// `skip_serializing_if` (NOT `skip_deserializing` — utoipa drops the
    /// latter) keeps it out of the wire shape when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[sqlx(default)]
    pub my_effective_role: Option<String>,
}

impl crate::auth::AclAnnotated for Page {
    fn acl_id(&self) -> Uuid {
        self.id
    }
    fn set_my_effective_role(&mut self, role: Option<String>) {
        self.my_effective_role = role;
    }
}

/// Create a page. Supply EITHER `folder_id` (free page) OR
/// `attached_kind` + `attached_id` (singleton tab); the handler XOR-validates
/// before the DB CHECK backstop.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreatePageRequest {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub folder_id: Option<Uuid>,
    #[serde(default)]
    pub attached_kind: Option<String>,
    #[serde(default)]
    pub attached_id: Option<Uuid>,
}

/// Partial update for a page. `title` applies to both kinds; `folder_id` moves
/// a FREE page between folders (rejected on an attached page).
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdatePageRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub folder_id: Option<Uuid>,
}
