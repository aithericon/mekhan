//! Inventory fold ingest (docs/32 batch-fold) — the consumer side of the
//! sink-mode crawl transport.
//!
//! Sink-mode `crawl` runners publish one [`FoldBatch`] per filled batch to
//! `inventory.fold.batch.<server>`; this loop folds each batch set-based into
//! `file_inventory` — per-file rows never become engine tokens or causality
//! events. Two fold disciplines:
//!
//! * `reconcile` — classify against the legacy baseline via
//!   [`reconcile::reconcile_batch`] (inherit hash by `(server, path)`,
//!   compare sizes).
//! * `index` — plain hashless observation upsert (status `indexed`); items
//!   that DO carry a hash also upsert the catalogue half in the same tx
//!   ("register fills both, never half").
//!
//! Delivery is at-least-once: every write is an upsert keyed on
//! `(file_server_id, path)` / `content_hash`, so a redelivered batch is
//! harmless. Failures NAK with a short delay and rely on redelivery.

use chrono::{DateTime, Utc};
use futures::StreamExt;
use sqlx::PgPool;

use aithericon_executor_domain::{FoldBatch, FoldMode};

use crate::nats::MekhanNats;

use super::reconcile::{self, ObservationContext, ObservedItem};

/// Start the fold ingest loop. Spawned once at service startup, next to the
/// causality ingest; runs until the message stream ends.
pub async fn start_inventory_fold_ingest(nats: MekhanNats, db: PgPool) {
    let consumer = match nats.inventory_fold_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create inventory-fold consumer: {e}");
            return;
        }
    };

    // Cap the pull batch (see step_executions_consumer's incident rationale):
    // one 5000-item fold batch is thousands of statements with today's
    // per-item loop, and anything prefetched but un-acked within `ack_wait`
    // redelivers. 8 × worst-case seconds stays inside the 120s ack_wait.
    let mut messages = match consumer.stream().max_messages_per_batch(8).messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start inventory-fold message stream: {e}");
            return;
        }
    };

    tracing::info!("inventory-fold ingest started on inventory.fold.batch.>");

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("inventory-fold ingest message error: {e}");
                continue;
            }
        };

        match process_batch(&db, &msg.payload).await {
            Ok(()) => {
                let _ = msg.ack().await;
            }
            Err(e) => {
                tracing::error!(subject = %msg.subject, "inventory-fold processing failed: {e}");
                let _ = msg
                    .ack_with(async_nats::jetstream::AckKind::Nak(Some(
                        std::time::Duration::from_secs(2),
                    )))
                    .await;
            }
        }
    }
}

/// Deserialize and fold one batch. Errors propagate to the NAK path.
async fn process_batch(
    db: &PgPool,
    payload: &[u8],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let batch: FoldBatch = serde_json::from_slice(payload)?;
    let ctx = ObservationContext {
        endpoint_root: Some(batch.endpoint_root.clone()).filter(|s| !s.is_empty()),
        serve_group: batch.serve_group.clone(),
    };

    let n_items = batch.items.len();
    match batch.mode {
        FoldMode::Reconcile => {
            let items: Vec<ObservedItem> = batch
                .items
                .iter()
                .map(|i| ObservedItem {
                    path: i.path.clone(),
                    size: i.size as i64,
                    mtime: parse_mtime(i.mtime.as_deref()),
                    hash: i.hash.clone(),
                    uid: i.uid.map(|v| v as i32),
                    gid: i.gid.map(|v| v as i32),
                    mode: i.mode,
                })
                .collect();
            let counts = reconcile::reconcile_batch(db, &batch.file_server_id, &items, &ctx).await?;
            tracing::debug!(
                server = %batch.file_server_id,
                batch_idx = batch.batch_idx,
                items = n_items,
                verified = counts.verified,
                mismatch = counts.mismatch,
                orphan_disk = counts.orphan_disk,
                "fold batch reconciled"
            );
        }
        FoldMode::Index => {
            let mut tx = db.begin().await?;
            for item in &batch.items {
                let mut provenance = serde_json::json!({
                    "source": "crawl_sink",
                    "observed_size": item.size,
                    "mtime": item.mtime,
                });
                if let Some(root) = ctx.endpoint_root.as_deref() {
                    provenance["endpoint_root"] = serde_json::json!(root);
                }
                if let Some(group) = ctx.serve_group.as_deref() {
                    provenance["serve_group"] = serde_json::json!(group);
                }
                // st_mode is provenance-only (no promoted column for it).
                if let Some(mode) = item.mode {
                    provenance["mode"] = serde_json::json!(mode);
                }

                // A hash-carrying item couples the catalogue half in the same
                // tx; hashless items are plain observations (inventory only).
                let hash = item.hash.as_deref().filter(|h| !h.trim().is_empty());
                if let Some(hash) = hash {
                    let name = item.path.rsplit('/').next().filter(|n| !n.is_empty());
                    super::queries::upsert_catalogue_by_hash(
                        &mut tx,
                        hash,
                        "legacy",
                        name,
                        Some(item.size as i64),
                        None,
                    )
                    .await?;
                }
                let facts = super::model::ObservedFacts {
                    size_bytes: Some(item.size as i64),
                    mtime: parse_mtime(item.mtime.as_deref()),
                    uid: item.uid.map(|v| v as i32),
                    gid: item.gid.map(|v| v as i32),
                };
                super::queries::upsert_inventory_copy(
                    &mut tx,
                    hash,
                    &batch.file_server_id,
                    &item.path,
                    "indexed",
                    false,
                    &provenance,
                    &facts,
                )
                .await?;
            }
            tx.commit().await?;
            tracing::debug!(
                server = %batch.file_server_id,
                batch_idx = batch.batch_idx,
                items = n_items,
                "fold batch indexed"
            );
        }
    }
    Ok(())
}

/// Parse the crawl op's RFC 3339 mtime rendering; unparseable values drop to
/// `None` (mtime is provenance-only, never classification input).
fn parse_mtime(s: Option<&str>) -> Option<DateTime<Utc>> {
    s.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}
