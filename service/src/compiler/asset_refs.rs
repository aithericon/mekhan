//! Node-level asset binding manifest handed to the compiler at publish time
//! (docs/20 §5/§6). The asset analog of [`crate::compiler::resource_refs`].
//!
//! An asset binding is **opaque** (docs/20 §4.1): the borrow-checker never
//! looks *inside* an asset. So unlike resources there is no `<head>.<field>`
//! Python-source discrimination — a binding is a node-data selection
//! (`AssetBinding { alias, ref_key }`) the author makes, scope-resolved by the
//! publish handler to a stable `(asset_id, version)` pin and threaded here.
//!
//! The map is keyed by the binding **alias** (the staged-input stem the node
//! code reads, `<alias>.json`). Each entry carries the pinned asset id +
//! version (rename-safe; baked into the AIR) plus the type id for downstream
//! consumers.

use std::collections::BTreeMap;

use uuid::Uuid;

use crate::models::asset::Cardinality;

/// One asset the publish handler resolved + pinned for a node binding. The
/// pin (`asset_id` + `version`) is baked into the AIR so post-publish record
/// edits don't retroactively change an already-published workflow — symmetric
/// with [`crate::compiler::resource_refs::KnownResource`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownAsset {
    /// Stable scope-resolved asset id. Persisted in the AIR so ref-key renames
    /// don't break already-published workflows; deletes do (intentionally).
    pub asset_id: Uuid,
    /// Asset type id. Carried for downstream consumers (telemetry / picker)
    /// that want the pin without re-querying.
    pub type_id: Uuid,
    /// The asset's flat ref-key — the author-facing identity that was
    /// scope-resolved to `asset_id`.
    pub ref_key: String,
    /// Version pinned at publish time. The asset resolver reads exactly these
    /// records at publish so post-publish edits don't bleed into running
    /// instances.
    pub version: i32,
    /// Cardinality of the pinned asset. Selects the staging shape: an `Object`
    /// stages its single record as a dict (`<key>.json` ⇒ an attribute-accessible
    /// Python global), a `Collection` stages the full row list.
    pub cardinality: Cardinality,
}

/// Per-publish asset-binding manifest. Keyed by the binding **alias** (the
/// staged-input stem). `BTreeMap` so iteration / serialization order is stable,
/// keeping the AIR diff-friendly — same rationale as
/// [`crate::compiler::resource_refs::KnownResources`].
pub type KnownAssets = BTreeMap<String, KnownAsset>;
