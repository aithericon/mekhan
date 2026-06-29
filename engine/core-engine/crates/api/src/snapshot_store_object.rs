//! Object-store-backed [`SnapshotStore`] for net hibernation snapshots.
//!
//! Replaces the former NATS-KV snapshot adapter: snapshots now live in an
//! OpenDAL-backed object store (S3, GCS, Azure Blob, local fs, …), keyed
//! per-workspace by `net_id`. The snapshot is captured by the registry's
//! hibernate hook and consumed by its wake path; see
//! [`petri_application::net_snapshot`] for the wake protocol and correctness
//! invariants.
//!
//! Requires the `artifact-store` feature flag and a DEDICATED env namespace
//! (independent of `ARTIFACT_STORE_*`):
//!
//! ```text
//! PETRI_SNAPSHOT_STORE_BACKEND=s3            # s3|local|gcs|azblob|sftp (default s3)
//! PETRI_SNAPSHOT_STORE_ENDPOINT=http://localhost:9005   # REQUIRED — gates from_env()
//! PETRI_SNAPSHOT_STORE_BUCKET=net-snapshots
//! PETRI_SNAPSHOT_STORE_REGION=us-east-1      # optional
//! PETRI_SNAPSHOT_STORE_PREFIX=               # optional path prefix
//! PETRI_SNAPSHOT_STORE_ACCESS_KEY=rustfsadmin
//! PETRI_SNAPSHOT_STORE_SECRET_KEY=rustfsadmin
//! PETRI_SNAPSHOT_MAX_BYTES=268435456         # sanity cap, default 256 MiB
//! ```
//!
//! ## Best-effort, never a correctness dependency
//!
//! Every operation degrades cleanly to today's full-replay behaviour: a missing
//! snapshot, an oversized one, a deserialize error, an unknown `version`, or an
//! object-store outage all surface as `()`/`None`, never an error.

#[cfg(feature = "artifact-store")]
mod inner {
    use aithericon_executor_storage::build_operator;
    use aithericon_executor_storage_types::{StorageBackend, StorageConfig, StorageCredentials};
    use petri_application::net_snapshot::{NetSnapshot, SnapshotStore, SNAPSHOT_VERSION};

    /// Default sanity cap on a serialized snapshot's size. A snapshot's
    /// `marking` can hold fat parked data tokens; a write past this cap is
    /// skipped so the next wake full-replays rather than persisting an
    /// unbounded blob. Override with `PETRI_SNAPSHOT_MAX_BYTES`.
    pub const DEFAULT_MAX_SNAPSHOT_BYTES: usize = 256 * 1024 * 1024;

    /// Object-store-backed snapshot store wrapping an OpenDAL operator.
    pub struct ObjectSnapshotStore {
        operator: opendal::Operator,
        prefix: String,
        max_bytes: usize,
    }

    impl ObjectSnapshotStore {
        /// Create from `PETRI_SNAPSHOT_STORE_*` environment variables.
        ///
        /// Returns `None` if `PETRI_SNAPSHOT_STORE_ENDPOINT` is not set (the
        /// store is then disabled → wakes full-replay).
        pub fn from_env() -> Option<Self> {
            let endpoint = std::env::var("PETRI_SNAPSHOT_STORE_ENDPOINT").ok()?;
            let backend = parse_backend(
                std::env::var("PETRI_SNAPSHOT_STORE_BACKEND")
                    .ok()
                    .as_deref(),
            );
            let bucket = std::env::var("PETRI_SNAPSHOT_STORE_BUCKET").unwrap_or_default();
            let region = std::env::var("PETRI_SNAPSHOT_STORE_REGION").ok();
            let prefix = std::env::var("PETRI_SNAPSHOT_STORE_PREFIX").unwrap_or_default();
            let access_key = std::env::var("PETRI_SNAPSHOT_STORE_ACCESS_KEY").unwrap_or_default();
            let secret_key = std::env::var("PETRI_SNAPSHOT_STORE_SECRET_KEY").unwrap_or_default();
            let max_bytes = std::env::var("PETRI_SNAPSHOT_MAX_BYTES")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(DEFAULT_MAX_SNAPSHOT_BYTES);

            let config = StorageConfig {
                backend,
                endpoint,
                bucket,
                region,
                prefix: prefix.clone(),
                credentials: StorageCredentials {
                    access_key,
                    secret_key,
                },
                retry: Default::default(),
                resource_alias: None,
            };

            match build_operator(&config) {
                Ok(operator) => Some(Self {
                    operator,
                    prefix,
                    max_bytes,
                }),
                Err(e) => {
                    tracing::error!(error = %e, "Failed to build snapshot store operator");
                    None
                }
            }
        }

        /// Object key for a `(ws, net_id)` snapshot. Mirrors the artifact
        /// store's prefix-join: if `prefix` is non-empty it already ends with
        /// the desired separator, so it is concatenated verbatim (no
        /// double-insertion).
        fn key(&self, ws: &str, net_id: &str) -> String {
            format!("{}{}/{}.json", self.prefix, ws, net_id)
        }
    }

    #[async_trait::async_trait]
    impl SnapshotStore for ObjectSnapshotStore {
        async fn put(&self, ws: &str, net_id: &str, snapshot: &NetSnapshot) {
            let bytes = match serde_json::to_vec(snapshot) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(net_id, error = %e, "Failed to serialize snapshot — skipping");
                    return;
                }
            };
            if bytes.len() > self.max_bytes {
                tracing::warn!(
                    net_id,
                    bytes = bytes.len(),
                    cap = self.max_bytes,
                    "snapshot exceeds sanity cap — skipping (wake will full-replay)"
                );
                return;
            }
            let key = self.key(ws, net_id);
            if let Err(e) = self.operator.write(&key, bytes).await {
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
            let key = self.key(ws, net_id);
            let buf = match self.operator.read(&key).await {
                Ok(buf) => buf,
                Err(e) if e.kind() == opendal::ErrorKind::NotFound => return None,
                Err(e) => {
                    tracing::warn!(net_id, error = %e, "Snapshot get failed — wake will full-replay");
                    return None;
                }
            };
            match serde_json::from_slice::<NetSnapshot>(&buf.to_vec()) {
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
            let key = self.key(ws, net_id);
            // opendal `delete` treats a missing object as success.
            if let Err(e) = self.operator.delete(&key).await {
                tracing::debug!(net_id, error = %e, "Snapshot delete failed (best-effort)");
            }
        }
    }

    /// Map a backend string to a [`StorageBackend`], case-insensitively.
    /// Unset or unrecognized → `S3` (the cloud default).
    fn parse_backend(s: Option<&str>) -> StorageBackend {
        match s.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("local") => StorageBackend::Local,
            Some("gcs") => StorageBackend::Gcs,
            Some("azblob") | Some("azure") => StorageBackend::AzBlob,
            Some("sftp") => StorageBackend::Sftp,
            _ => StorageBackend::S3,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use petri_domain::Marking;

        /// A fresh, unique temp directory for a hermetic Local-backend operator.
        fn temp_root() -> String {
            let dir = std::env::temp_dir().join(format!(
                "petri-snapshot-test-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            dir.to_string_lossy().into_owned()
        }

        /// Build an `ObjectSnapshotStore` over a Local (filesystem) operator
        /// rooted at `root`, with no network access.
        fn local_store(root: &str, max_bytes: usize) -> ObjectSnapshotStore {
            let config = StorageConfig {
                backend: StorageBackend::Local,
                endpoint: root.to_string(),
                bucket: String::new(),
                region: None,
                prefix: String::new(),
                credentials: StorageCredentials::default(),
                retry: Default::default(),
                resource_alias: None,
            };
            let operator = build_operator(&config).expect("local operator builds");
            ObjectSnapshotStore {
                operator,
                prefix: String::new(),
                max_bytes,
            }
        }

        fn sample_snapshot(version: u32) -> NetSnapshot {
            NetSnapshot {
                marking: Marking::new(),
                dedup: vec![],
                last_hash: Some("deadbeef".to_string()),
                event_count: 7,
                next_sequence: 7,
                last_stream_seq: 42,
                topology: None,
                version,
            }
        }

        #[tokio::test]
        async fn snapshot_put_then_get_round_trips() {
            let root = temp_root();
            let store = local_store(&root, DEFAULT_MAX_SNAPSHOT_BYTES);

            assert!(
                store.get("ws", "net-a").await.is_none(),
                "no snapshot should exist initially"
            );

            let snap = sample_snapshot(SNAPSHOT_VERSION);
            store.put("ws", "net-a", &snap).await;

            let got = store
                .get("ws", "net-a")
                .await
                .expect("snapshot must round-trip");
            assert_eq!(got.last_hash, Some("deadbeef".to_string()));
            assert_eq!(got.event_count, 7);
            assert_eq!(got.next_sequence, 7);
            assert_eq!(got.last_stream_seq, 42);
            assert_eq!(got.version, SNAPSHOT_VERSION);
        }

        #[tokio::test]
        async fn snapshot_get_absent_returns_none() {
            let root = temp_root();
            let store = local_store(&root, DEFAULT_MAX_SNAPSHOT_BYTES);
            assert!(store.get("ws", "missing").await.is_none());
        }

        #[tokio::test]
        async fn snapshot_delete_then_get_returns_none() {
            let root = temp_root();
            let store = local_store(&root, DEFAULT_MAX_SNAPSHOT_BYTES);

            store
                .put("ws", "net-d", &sample_snapshot(SNAPSHOT_VERSION))
                .await;
            assert!(store.get("ws", "net-d").await.is_some());

            store.delete("ws", "net-d").await;
            assert!(
                store.get("ws", "net-d").await.is_none(),
                "snapshot must be gone after delete"
            );

            // Deleting an already-absent key is a no-op success.
            store.delete("ws", "net-d").await;
        }

        #[tokio::test]
        async fn snapshot_over_cap_is_skipped() {
            let root = temp_root();
            // Cap of 1 byte → any real snapshot exceeds it and the write is
            // skipped, so the read finds nothing.
            let store = local_store(&root, 1);
            store
                .put("ws", "net-big", &sample_snapshot(SNAPSHOT_VERSION))
                .await;
            assert!(
                store.get("ws", "net-big").await.is_none(),
                "oversized snapshot must be skipped (wake full-replays)"
            );
        }

        #[tokio::test]
        async fn snapshot_future_version_is_ignored() {
            let root = temp_root();
            let store = local_store(&root, DEFAULT_MAX_SNAPSHOT_BYTES);

            // A snapshot written by a NEWER engine (version > supported) must be
            // ignored on read → wake falls back to full replay.
            store
                .put("ws", "net-v", &sample_snapshot(SNAPSHOT_VERSION + 1))
                .await;
            assert!(
                store.get("ws", "net-v").await.is_none(),
                "a newer-versioned snapshot must be ignored (→ full replay)"
            );
        }
    }
}

#[cfg(feature = "artifact-store")]
pub use inner::*;

// Stub when the feature is disabled — ObjectSnapshotStore::from_env() always
// returns None, so the engine boots with the snapshot fast-path disabled.
#[cfg(not(feature = "artifact-store"))]
mod stub {
    use petri_application::net_snapshot::{NetSnapshot, SnapshotStore};

    pub struct ObjectSnapshotStore;

    impl ObjectSnapshotStore {
        pub fn from_env() -> Option<Self> {
            None
        }
    }

    // `from_env` always returns `None`, so this impl is never exercised — it
    // exists only so the `Some(store)` selection arm in `main.rs` typechecks
    // when the feature is off.
    #[async_trait::async_trait]
    impl SnapshotStore for ObjectSnapshotStore {
        async fn put(&self, _ws: &str, _net_id: &str, _snapshot: &NetSnapshot) {}
        async fn get(&self, _ws: &str, _net_id: &str) -> Option<NetSnapshot> {
            None
        }
        async fn delete(&self, _ws: &str, _net_id: &str) {}
    }
}

#[cfg(not(feature = "artifact-store"))]
pub use stub::*;
