//! NATS KV-backed [`SnapshotStore`] for net hibernation snapshots.
//!
//! Mirrors the per-workspace KV pattern of [`crate::net_metadata`]: a base
//! bucket name [`SNAPSHOT_KV_BUCKET`] namespaced per workspace via
//! [`crate::kv_bucket_for`] (`KV_NET_SNAPSHOT_{ws}`), keyed by `net_id`, with
//! `history: 1` (only the latest snapshot per net matters). The snapshot is
//! captured by the registry's hibernate hook and consumed by its wake path; see
//! [`petri_application::net_snapshot`] for the wake protocol and correctness
//! invariants.
//!
//! ## Best-effort, never a correctness dependency
//!
//! Every operation degrades cleanly to today's full-replay behavior:
//! - a KV that cannot be opened → `put`/`delete` no-op, `get` returns `None`;
//! - a snapshot whose serialized size exceeds [`max_snapshot_bytes`] (the
//!   marking can carry fat parked tokens) → `put` SKIPS the write (and logs),
//!   so the next wake full-replays rather than risking a KV `put` failure / OOM;
//! - a deserialize failure or unknown `version` on `get` → `None` (full replay).

use async_nats::jetstream::kv::Store;
use petri_application::net_snapshot::{NetSnapshot, SnapshotStore, SNAPSHOT_VERSION};

/// Base NATS KV bucket name for net snapshots. The live bucket is
/// per-workspace: `KV_NET_SNAPSHOT_{ws}` (via [`crate::kv_bucket_for`]).
pub const SNAPSHOT_KV_BUCKET: &str = "KV_NET_SNAPSHOT";

/// Default cap on a serialized snapshot's size. A snapshot's `marking` can hold
/// fat parked data tokens, so an unbounded write could exceed the NATS KV value
/// limit (bounded by the stream `max_payload`) or itself become an OOM/payload
/// vector. 8 MiB stays well under typical NATS limits while covering the common
/// case. Override with `PETRI_MAX_SNAPSHOT_BYTES`.
pub const DEFAULT_MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;

/// The snapshot byte cap, read once from `PETRI_MAX_SNAPSHOT_BYTES`
/// (default [`DEFAULT_MAX_SNAPSHOT_BYTES`]).
pub fn max_snapshot_bytes() -> usize {
    std::env::var("PETRI_MAX_SNAPSHOT_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_SNAPSHOT_BYTES)
}

/// KV-backed snapshot store. Per-workspace buckets are opened (get-or-create)
/// lazily and cached, mirroring [`crate::net_metadata`]'s `per_ws_store`.
pub struct NetSnapshotStore {
    jetstream: async_nats::jetstream::Context,
    /// Lazily-opened per-workspace `KV_NET_SNAPSHOT_{ws}` stores.
    per_ws: tokio::sync::Mutex<std::collections::HashMap<String, Store>>,
}

impl NetSnapshotStore {
    pub fn new(jetstream: async_nats::jetstream::Context) -> Self {
        Self {
            jetstream,
            per_ws: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Open (and cache) the per-workspace bucket. Returns `None` if it cannot be
    /// created — callers then degrade to full replay.
    async fn bucket(&self, ws: &str) -> Option<Store> {
        {
            let cache = self.per_ws.lock().await;
            if let Some(s) = cache.get(ws) {
                return Some(s.clone());
            }
        }
        let name = crate::kv_bucket_for(SNAPSHOT_KV_BUCKET, ws);
        let store = match self.jetstream.get_key_value(&name).await {
            Ok(s) => Some(s),
            Err(_) => match self
                .jetstream
                .create_key_value(async_nats::jetstream::kv::Config {
                    bucket: name.clone(),
                    history: 1,
                    ..Default::default()
                })
                .await
            {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!(
                        bucket = %name, error = %e,
                        "Failed to open snapshot KV bucket — wake will full-replay"
                    );
                    None
                }
            },
        };
        if let Some(ref s) = store {
            self.per_ws.lock().await.insert(ws.to_string(), s.clone());
        }
        store
    }
}

#[async_trait::async_trait]
impl SnapshotStore for NetSnapshotStore {
    async fn put(&self, ws: &str, net_id: &str, snapshot: &NetSnapshot) {
        let Some(kv) = self.bucket(ws).await else {
            return;
        };
        let bytes = match serde_json::to_vec(snapshot) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(net_id, error = %e, "Failed to serialize snapshot — skipping");
                return;
            }
        };
        let cap = max_snapshot_bytes();
        if bytes.len() > cap {
            tracing::warn!(
                net_id,
                bytes = bytes.len(),
                cap,
                "Snapshot exceeds cap — skipping (wake will full-replay)"
            );
            return;
        }
        if let Err(e) = kv.put(net_id, bytes.into()).await {
            tracing::warn!(net_id, error = %e, "Snapshot put failed — wake will full-replay");
        } else {
            tracing::debug!(
                net_id,
                ws,
                last_stream_seq = snapshot.last_stream_seq,
                event_count = snapshot.event_count,
                "Wrote net snapshot"
            );
        }
    }

    async fn get(&self, ws: &str, net_id: &str) -> Option<NetSnapshot> {
        let kv = self.bucket(ws).await?;
        let bytes = match kv.get(net_id).await {
            Ok(Some(b)) => b,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(net_id, error = %e, "Snapshot get failed — wake will full-replay");
                return None;
            }
        };
        match serde_json::from_slice::<NetSnapshot>(&bytes) {
            Ok(snap) if snap.version <= SNAPSHOT_VERSION => Some(snap),
            Ok(snap) => {
                tracing::warn!(
                    net_id,
                    version = snap.version,
                    supported = SNAPSHOT_VERSION,
                    "Snapshot version newer than supported — ignoring (full replay)"
                );
                None
            }
            Err(e) => {
                tracing::warn!(net_id, error = %e, "Snapshot deserialize failed — full replay");
                None
            }
        }
    }

    async fn delete(&self, ws: &str, net_id: &str) {
        if let Some(kv) = self.bucket(ws).await {
            if let Err(e) = kv.purge(net_id).await {
                tracing::debug!(net_id, error = %e, "Snapshot purge failed (best-effort)");
            }
        }
    }
}
