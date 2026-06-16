//! Legacy-migration pipeline DRIVER (docs/32 Phase 5).
//!
//! Runs the migration pipeline end-to-end against a **synthetic NAS** — a local
//! root directory standing in for an NFS mount:
//!
//! ```text
//! crawl (real op, in-process)
//!   └─▶ fold each emitted batch → reconcile_batch  (verified/mismatch/orphan_disk)
//!         └─▶ hash-pending: real probe op on orphan_disk/mismatch rows
//!               └─▶ set content_hash + UPSERT catalogue + advance status
//! ```
//!
//! ## Architecture note (transport)
//!
//! This driver invokes the `executor-file-ops` crawl/probe ops **IN-PROCESS**
//! against a `Local` [`StorageConfig`] as the dev/scaffold harness — no NATS, no
//! runner. In production these SAME ops run inside a co-located runner that
//! pulls jobs over NATS (already supported by the file-ops backend); the
//! NATS-dispatch + SSH-deployed-runner layer is the deferred "real operations"
//! step. **The driver's pipeline logic (fold + hash + register) is
//! transport-agnostic** — only the op-invocation seam changes when it moves
//! behind NATS.
//!
//! The whole module is behind the `migration-driver` cargo feature so the
//! default service build pulls in NONE of the file-ops / OpenDAL deps.

use std::path::Path;
use std::sync::Arc;

use aithericon_executor_backend::traits::EventStream;
use aithericon_executor_backend_configs::file_ops::{CrawlConfig, ProbeConfig};
use aithericon_executor_storage::build_operator;
use aithericon_executor_storage_types::{StorageBackend, StorageConfig, StorageCredentials};
use aithericon_file_metadata::ChecksumAlgorithm;
use async_trait::async_trait;
use opendal::Operator;
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::inventory::reconcile::{reconcile_batch, ObservedItem, ReconcileCounts};

pub mod migrate;
pub mod synthetic;

pub use migrate::{migrate, retire, MigrateCounts, MigrateSelector, RetireCounts};

/// Errors surfaced by the driver pipeline.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("storage/operator error: {0}")]
    Storage(String),
    #[error("crawl op error: {0}")]
    Crawl(String),
    #[error("probe op error: {0}")]
    Probe(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Build a `Local` [`StorageConfig`] rooted at `root` (the synthetic NAS / NFS
/// mount). `prefix` is left empty so crawl/probe paths are used verbatim
/// relative to the root.
pub fn local_storage(root: &str) -> StorageConfig {
    StorageConfig {
        backend: StorageBackend::Local,
        endpoint: root.to_string(),
        bucket: String::new(),
        region: None,
        prefix: String::new(),
        credentials: StorageCredentials::default(),
        retry: Default::default(),
        resource_alias: None,
    }
}

/// Build an OpenDAL [`Operator`] from a [`StorageConfig`] — the exact path the
/// file-ops backend uses (`aithericon_executor_storage::build_operator`).
pub(crate) fn operator_for(storage: &StorageConfig) -> Result<Operator, DriverError> {
    build_operator(storage).map_err(|e| DriverError::Storage(e.to_string()))
}

// ---------------------------------------------------------------------------
// index-reconcile: crawl (real op) → fold each batch through reconcile_batch
// ---------------------------------------------------------------------------

/// An [`EventStream`] that folds each crawl batch into `file_inventory` via
/// [`reconcile_batch`] as the batch is emitted.
///
/// The crawl op `await`s each `item()`, so folding is inline + back-pressured.
/// `item()`/`close()` can't return a `Result`, so a per-batch failure is
/// captured in `error` (and re-raised by [`index_reconcile`] after the op
/// returns) and running totals accumulate in `counts`.
struct ReconcileSink {
    pool: PgPool,
    file_server_id: String,
    counts: Mutex<ReconcileCounts>,
    batches: Mutex<u64>,
    error: Mutex<Option<sqlx::Error>>,
}

impl ReconcileSink {
    fn new(pool: PgPool, file_server_id: String) -> Self {
        Self {
            pool,
            file_server_id,
            counts: Mutex::new(ReconcileCounts::default()),
            batches: Mutex::new(0),
            error: Mutex::new(None),
        }
    }

    /// Parse the crawl op's `{"items":[{path,size,mtime}, …]}` batch payload into
    /// the reconcile [`ObservedItem`] shape.
    fn parse_batch(payload: &Value) -> Vec<ObservedItem> {
        payload
            .get("items")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| serde_json::from_value::<ObservedItem>(e.clone()).ok())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[async_trait]
impl EventStream for ReconcileSink {
    async fn log(
        &self,
        _level: aithericon_executor_domain::LogLevel,
        _msg: String,
        _f: std::collections::HashMap<String, String>,
    ) {
    }

    async fn item(&self, _channel: String, _episode_uid: String, idx: u64, payload: Value) {
        // Short-circuit once a prior batch errored.
        if self.error.lock().await.is_some() {
            return;
        }
        let items = Self::parse_batch(&payload);
        // Events-mode batch items carry `endpoint_root` per item; lift it into
        // the observation context so provenance keeps the adopt-autostamp keys.
        let ctx = crate::inventory::reconcile::ObservationContext {
            endpoint_root: payload
                .get("items")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|e| e.get("endpoint_root"))
                .and_then(Value::as_str)
                .map(str::to_string),
            serve_group: None,
        };
        match reconcile_batch(&self.pool, &self.file_server_id, &items, &ctx).await {
            Ok(c) => {
                let mut totals = self.counts.lock().await;
                totals.verified += c.verified;
                totals.mismatch += c.mismatch;
                totals.orphan_disk += c.orphan_disk;
                *self.batches.lock().await += 1;
                debug!(
                    batch = idx,
                    n = items.len(),
                    verified = c.verified,
                    mismatch = c.mismatch,
                    orphan_disk = c.orphan_disk,
                    "reconciled crawl batch"
                );
            }
            Err(e) => {
                warn!(batch = idx, error = %e, "reconcile_batch failed");
                *self.error.lock().await = Some(e);
            }
        }
    }

    async fn close(&self, _channel: String, _episode_uid: String, count: u64) {
        debug!(total = count, "crawl episode closed");
    }
}

/// Run the real `crawl` op over `root` and fold every emitted batch through
/// [`reconcile_batch`], classifying each observed file against the legacy
/// baseline (`legacy_file_index`) into `verified` / `mismatch` / `orphan_disk`.
///
/// Returns the aggregate [`ReconcileCounts`]. The crawl op is invoked in-process
/// (`ops::crawl::execute`) with an OpenDAL `Operator` built from a `Local`
/// `StorageConfig{ endpoint: root }` — the same operator the file-ops backend
/// constructs at runtime.
pub async fn index_reconcile(
    pool: &PgPool,
    file_server_id: &str,
    root: &str,
    batch_size: usize,
) -> Result<ReconcileCounts, DriverError> {
    let storage = local_storage(root);
    let operator = operator_for(&storage)?;

    let config = CrawlConfig {
        prefix: String::new(),
        storage: storage.clone(),
        batch_size: batch_size.max(1).into(),
        resume_from: None,
        stat: true,
        max_batches: None,
        sink: None,
        probe: None,
    };

    let sink = Arc::new(ReconcileSink::new(pool.clone(), file_server_id.to_string()));
    let cancel = CancellationToken::new();

    info!(
        file_server_id,
        root, batch_size, "index-reconcile: crawl + fold"
    );

    // The storage prefix ("") is what the file-ops dispatch passes to the op as
    // the path-resolution prefix; pass the same here. `endpoint_root` is the
    // canonical root stamped into each row's provenance — for the synthetic-NAS
    // driver that's the crawl `root` (an NFS mount stand-in).
    // probe is None and sink mode is off, so the probe temp dir and batch
    // sink are inert; the events-mode EventStream is the driver's fold sink.
    aithericon_executor_file_ops::ops::crawl::execute(
        &config,
        &operator,
        &storage.prefix,
        root,
        Some(sink.clone() as Arc<dyn EventStream>),
        None,
        "migration-driver",
        &std::env::temp_dir(),
        &cancel,
    )
    .await
    .map_err(|e| DriverError::Crawl(e.to_string()))?;

    // Re-raise any per-batch reconcile error captured inside the sink.
    if let Some(e) = sink.error.lock().await.take() {
        return Err(DriverError::Db(e));
    }

    let counts = sink.counts.lock().await.clone();
    let batches = *sink.batches.lock().await;
    info!(
        verified = counts.verified,
        mismatch = counts.mismatch,
        orphan_disk = counts.orphan_disk,
        batches,
        "index-reconcile complete"
    );
    Ok(counts)
}

// ---------------------------------------------------------------------------
// hash-pending: real probe op on orphan_disk/mismatch rows → register
// ---------------------------------------------------------------------------

/// Counts returned by [`hash_pending`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HashPendingCounts {
    /// `orphan_disk` rows hashed → `verified` + catalogue-registered.
    pub orphan_disk_registered: i64,
    /// `mismatch` rows re-hashed (status kept `mismatch`, freshly-computed hash
    /// recorded in provenance).
    pub mismatch_rehashed: i64,
    /// `verified` rows re-probed as an audit sample (no status change).
    pub verified_sampled: i64,
    /// Rows where the probe op failed (counted, logged, left untouched).
    pub probe_failed: i64,
}

/// One inventory row eligible for hashing.
#[derive(Debug, sqlx::FromRow)]
struct PendingRow {
    id: uuid::Uuid,
    path: String,
    status: String,
    provenance: Value,
}

/// Targeted-hash the pending inventory rows for `file_server_id`.
///
/// Selects `file_inventory` rows with `status IN ('orphan_disk','mismatch')`
/// (plus an optional `sample_verified` percentage of `verified` rows for audit),
/// runs the REAL `probe` op against each (computing a deterministic bare-hex
/// SHA-256), then:
///
/// * `orphan_disk` → set `content_hash`, UPSERT a `catalogue_entries` row by
///   that hash (`category='observed'`, size from probe), advance status to
///   `verified`, stamp `last_verified`.
/// * `mismatch`    → record the freshly-computed hash in `provenance.probed_hash`
///   and `content_hash`, but KEEP `status='mismatch'` (the size disagreement
///   with the legacy baseline is a curation decision, not auto-resolved here).
/// * sampled `verified` → record `provenance.probed_hash` (audit), no status
///   change.
///
/// The probe op is invoked in-process (`ops::probe::execute`) with an OpenDAL
/// `Operator` over a `Local` `StorageConfig{ endpoint: root }` + a tempdir
/// `run_dir` (probe downloads each file there before hashing).
pub async fn hash_pending(
    pool: &PgPool,
    file_server_id: &str,
    root: &str,
    sample_verified_pct: u8,
) -> Result<HashPendingCounts, DriverError> {
    let storage = local_storage(root);
    let operator = operator_for(&storage)?;

    // A single tempdir backs every probe's `run_dir` (probe writes a temp copy
    // there, hashes it, then removes it). Dropped at end of the run.
    let run_dir = tempfile::tempdir()?;

    // orphan_disk + mismatch always; verified only if sampling is requested.
    let rows: Vec<PendingRow> = if sample_verified_pct > 0 {
        let pct = sample_verified_pct.min(100) as f64 / 100.0;
        sqlx::query_as(
            "SELECT id, path, status, provenance FROM file_inventory \
             WHERE file_server_id = $1 \
               AND ( status IN ('orphan_disk','mismatch') \
                     OR (status = 'verified' AND random() < $2) ) \
             ORDER BY path",
        )
        .bind(file_server_id)
        .bind(pct)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as(
            "SELECT id, path, status, provenance FROM file_inventory \
             WHERE file_server_id = $1 AND status IN ('orphan_disk','mismatch') \
             ORDER BY path",
        )
        .bind(file_server_id)
        .fetch_all(pool)
        .await?
    };

    info!(
        file_server_id,
        candidates = rows.len(),
        sample_verified_pct,
        "hash-pending: probing pending rows"
    );

    let mut counts = HashPendingCounts::default();

    for row in rows {
        let (digest, size) =
            match probe_one(&operator, &storage.prefix, &row.path, run_dir.path()).await {
                Ok(v) => v,
                Err(e) => {
                    warn!(path = %row.path, error = %e, "probe failed; skipping row");
                    counts.probe_failed += 1;
                    continue;
                }
            };

        match row.status.as_str() {
            "orphan_disk" => {
                register_hashed(pool, &row, &digest, size, "verified", "observed").await?;
                counts.orphan_disk_registered += 1;
            }
            "mismatch" => {
                // Record the freshly-computed hash but keep the mismatch flag.
                record_probed_hash(pool, &row, &digest, /*advance=*/ false).await?;
                counts.mismatch_rehashed += 1;
            }
            "verified" => {
                // Audit sample: stamp the probed hash in provenance, no change.
                record_probed_hash(pool, &row, &digest, /*advance=*/ false).await?;
                counts.verified_sampled += 1;
            }
            other => {
                debug!(status = other, "unexpected status in pending set; skipping");
            }
        }
    }

    info!(
        orphan_disk_registered = counts.orphan_disk_registered,
        mismatch_rehashed = counts.mismatch_rehashed,
        verified_sampled = counts.verified_sampled,
        probe_failed = counts.probe_failed,
        "hash-pending complete"
    );
    Ok(counts)
}

/// Run the real `probe` op against one file, returning `(bare_hex_sha256, size)`.
async fn probe_one(
    operator: &Operator,
    prefix: &str,
    path: &str,
    run_dir: &Path,
) -> Result<(String, i64), DriverError> {
    let config = ProbeConfig {
        path: path.to_string(),
        include_statistics: false,
        storage: None, // operator already built from the Local storage
        checksum_algo: Some(ChecksumAlgorithm::Sha256),
    };

    let outputs =
        aithericon_executor_file_ops::ops::probe::execute(&config, operator, prefix, run_dir)
            .await
            .map_err(|e| DriverError::Probe(e.to_string()))?;

    // `checksum_digest` is the bare lowercase-hex SHA-256 — the exact reconcile
    // join-key shape (matches `legacy_file_index.hash` with `"SHA256:"` stripped
    // and the catalogue `content_hash`).
    let digest = outputs
        .get("checksum_digest")
        .and_then(Value::as_str)
        .ok_or_else(|| DriverError::Probe("probe returned no checksum_digest".into()))?
        .to_string();

    let size = outputs
        .get("file_size_bytes")
        .and_then(Value::as_i64)
        .unwrap_or_default();

    Ok((digest, size))
}

/// `orphan_disk` → set content_hash, UPSERT catalogue by hash, advance status.
async fn register_hashed(
    pool: &PgPool,
    row: &PendingRow,
    digest: &str,
    size: i64,
    new_status: &str,
    category: &str,
) -> Result<(), DriverError> {
    let mut tx = pool.begin().await?;

    // Catalogue row keyed on (workspace_id, content_hash) (dedup across copies).
    // This legacy disk-migration driver has no per-tenant context, so rows land
    // in the default workspace (workspace_id defaults to Uuid::nil()).
    sqlx::query(
        "INSERT INTO catalogue_entries (content_hash, category, size_bytes) \
         VALUES ($1, $2, $3) ON CONFLICT (workspace_id, content_hash) DO NOTHING",
    )
    .bind(digest)
    .bind(category)
    .bind(size)
    .execute(&mut *tx)
    .await?;

    // Merge the probed hash into provenance + advance the inventory row.
    let mut provenance = row.provenance.clone();
    if let Some(obj) = provenance.as_object_mut() {
        obj.insert("probed_hash".into(), Value::String(digest.to_string()));
        obj.insert("probed_size".into(), Value::Number(size.into()));
    }

    sqlx::query(
        "UPDATE file_inventory SET \
            content_hash  = $1, \
            status        = $2, \
            provenance    = $3, \
            last_verified = NOW(), \
            updated_at    = NOW() \
         WHERE id = $4",
    )
    .bind(digest)
    .bind(new_status)
    .bind(&provenance)
    .bind(row.id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// Record the freshly-computed probe hash on an inventory row WITHOUT changing
/// status (mismatch stays mismatch; verified-sample stays verified). Sets
/// `content_hash` to the probed digest and stamps `provenance.probed_hash`.
async fn record_probed_hash(
    pool: &PgPool,
    row: &PendingRow,
    digest: &str,
    _advance: bool,
) -> Result<(), DriverError> {
    let mut provenance = row.provenance.clone();
    if let Some(obj) = provenance.as_object_mut() {
        obj.insert("probed_hash".into(), Value::String(digest.to_string()));
    }

    sqlx::query(
        "UPDATE file_inventory SET \
            content_hash  = $1, \
            provenance    = $2, \
            last_verified = NOW(), \
            updated_at    = NOW() \
         WHERE id = $3",
    )
    .bind(digest)
    .bind(&provenance)
    .bind(row.id)
    .execute(pool)
    .await?;

    Ok(())
}
