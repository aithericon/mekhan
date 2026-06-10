//! Inventory fold batches — the data-plane envelope for batch crawl
//! registration (docs/32 batch-fold transport).
//!
//! A file-ops `crawl` running in sink mode publishes one `FoldBatch` per
//! filled batch to the `INVENTORY_FOLD` JetStream stream instead of emitting
//! per-file channel items. The control plane (mekhan-service) consumes the
//! stream with a durable consumer and folds each batch set-based into
//! `file_inventory` (and the catalogue, for hash-carrying items). Per-file
//! rows therefore never become engine tokens or causality-projector events —
//! the workflow's control plane carries only cursors and counts.
//!
//! Delivery contract: at-least-once. The publisher sets `Nats-Msg-Id` to
//! [`FoldBatch::msg_id`] (publish-side dedup inside the stream's duplicate
//! window) and the consumer's upserts are idempotent on
//! `(file_server_id, path)`, so redelivery is harmless.

use serde::{Deserialize, Serialize};

/// JetStream stream holding fold batches. Both the executor (publisher) and
/// mekhan-service (consumer) `get_or_create` this stream — their configs MUST
/// stay byte-identical or the second creator errors at boot.
pub const INVENTORY_FOLD_STREAM: &str = "INVENTORY_FOLD";

/// Subject ROOT fold batches are published under. The stream binds
/// `{root}.>` and publishers append a sanitized `file_server_id` leaf
/// (`inventory.fold.batch.<server>`), so a consumer can filter one server's
/// campaign without a new stream.
pub const INVENTORY_FOLD_SUBJECT: &str = "inventory.fold.batch";

/// How the consumer folds a batch into the inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FoldMode {
    /// Classify against the legacy baseline (inherit hash by
    /// `(file_server_id, path)`, compare sizes) — the docs/32 §4/§5 reconcile.
    Reconcile,
    /// Plain inventory upsert (status `indexed`); items that carry a `hash`
    /// also upsert the catalogue half in the same transaction.
    Index,
}

impl FoldMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reconcile => "reconcile",
            Self::Index => "index",
        }
    }
}

/// One observed file inside a fold batch (crawl is metadata-only; `hash` is
/// present only for hash-bearing publishers, e.g. a future probe-fed flow).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FoldItem {
    /// Server-relative file path (the inventory identity together with the
    /// batch-level `file_server_id`).
    pub path: String,
    /// Observed size in bytes.
    pub size: u64,
    /// Observed mtime (RFC 3339), when the lister/stat provided one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<String>,
    /// Content hash (`sha256:…`), when known. Triggers catalogue coupling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// Owning user id (`st_uid`), when the crawler could lstat locally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    /// Owning group id (`st_gid`), when the crawler could lstat locally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    /// File mode bits (`st_mode`), when the crawler could lstat locally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

/// One crawl batch on its way to the inventory fold consumer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FoldBatch {
    /// Execution that produced the batch (dedup-id component + tracing).
    pub execution_id: String,
    /// Episode uid shared by every batch of one crawl walk.
    pub episode_uid: String,
    /// 0-based batch index within the episode (dedup-id component).
    pub batch_idx: u64,
    /// Fold discipline the consumer applies.
    pub mode: FoldMode,
    /// Inventory server key the items belong to (`file_inventory.file_server_id`).
    pub file_server_id: String,
    /// Canonical endpoint root the item paths are anchored to (persisted into
    /// inventory provenance so file-server `adopt` can auto-stamp it).
    pub endpoint_root: String,
    /// Serve identity of the publishing runner (`runner_id` or routing
    /// partition) — persisted into provenance for adopt auto-stamping. Stamped
    /// by the NATS sink, not the backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serve_group: Option<String>,
    /// The observed files.
    pub items: Vec<FoldItem>,
}

impl FoldBatch {
    /// Deterministic `Nats-Msg-Id` so a republished batch (job retry, resume
    /// overlap) dedups inside the stream's duplicate window.
    pub fn msg_id(&self) -> String {
        format!("{}-{}-{}", self.execution_id, self.episode_uid, self.batch_idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_batch_roundtrip_and_msg_id() {
        let batch = FoldBatch {
            execution_id: "exec-1".into(),
            episode_uid: "ep-1".into(),
            batch_idx: 3,
            mode: FoldMode::Index,
            file_server_id: "demo-nas".into(),
            endpoint_root: "/tmp".into(),
            serve_group: Some("runner-a".into()),
            items: vec![FoldItem {
                path: "datasets/a.csv".into(),
                size: 42,
                mtime: Some("2026-06-10T00:00:00Z".into()),
                hash: None,
                uid: Some(501),
                gid: Some(20),
                mode: Some(0o100644),
            }],
        };
        assert_eq!(batch.msg_id(), "exec-1-ep-1-3");
        let json = serde_json::to_string(&batch).unwrap();
        assert!(json.contains("\"mode\":\"index\""));
        let back: FoldBatch = serde_json::from_str(&json).unwrap();
        assert_eq!(back.items[0].path, "datasets/a.csv");
        assert_eq!(back.mode, FoldMode::Index);
    }

    #[test]
    fn fold_item_optional_fields_skip() {
        let item = FoldItem {
            path: "x".into(),
            size: 0,
            mtime: None,
            hash: None,
            uid: None,
            gid: None,
            mode: None,
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(!json.contains("mtime"));
        assert!(!json.contains("hash"));
        assert!(!json.contains("uid"));
        assert!(!json.contains("gid"));
        assert!(!json.contains("mode"));
    }

    /// Wire backward compat: a pre-ownership publisher's item JSON (no
    /// uid/gid/mode keys) must still deserialize, defaulting to `None`.
    #[test]
    fn fold_item_pre_ownership_json_deserializes() {
        let json = r#"{"path":"datasets/a.csv","size":42,"mtime":"2026-06-10T00:00:00Z"}"#;
        let item: FoldItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.path, "datasets/a.csv");
        assert_eq!(item.size, 42);
        assert_eq!(item.uid, None);
        assert_eq!(item.gid, None);
        assert_eq!(item.mode, None);
    }
}
