//! Reactive catalogue subscription subsystem (ADR-17, Phase 3A).
//!
//! When a new artifact is registered in the catalogue, the subscription manager
//! evaluates all active subscriptions. If the artifact matches a subscription's
//! filters, an `ExternalSignal` is published to the subscribing Petri net via
//! JetStream so it lands in the `PETRI_GLOBAL` stream.
//!
//! Subscriptions are persisted in a NATS KV bucket (`CATALOGUE_SUBSCRIPTIONS`)
//! and cached in-memory via `DashMap` for fast evaluation on the hot path.

use std::sync::Arc;

use async_nats::jetstream;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use super::model::{CatalogueEntry, CatalogueRegisterCommand};
use super::protocol::SubscribeRequest;
use super::query_match;
use crate::observability::record_silent_drop_with;

/// A persisted catalogue subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogueSubscription {
    pub subscription_id: String,
    pub net_id: String,
    /// Owning tenant. Server-enforced scope for the backfill query. Defaults to
    /// the nil workspace for legacy KV entries that predate the field.
    #[serde(default)]
    pub workspace_id: uuid::Uuid,
    pub signal_place: String,
    /// Catalogue query DSL string (the same grammar the data browser and catalog
    /// triggers submit, e.g. `category:model filename~report created_at>-7d`).
    /// Compiled server-side at evaluation time so relative dates re-resolve per
    /// fire. An empty string matches every entry.
    #[serde(default)]
    pub query: String,
    pub backfill: bool,
    pub created_at: DateTime<Utc>,
}

/// Information about a subscription that matched a newly ingested artifact.
/// Returned from `evaluate_new_artifact` so the caller can record the
/// egress-side cross-link for provenance tracking.
#[derive(Debug, Clone)]
pub struct MatchedSubscription {
    pub subscription_id: String,
    pub target_net_id: String,
    /// The `signal_key` embedded in the published signal payload. Used as
    /// the primary key when inserting into `causality_cross_links`.
    pub signal_key: String,
}

/// Manages catalogue subscriptions with an in-memory cache backed by NATS KV.
pub struct SubscriptionManager {
    cache: DashMap<String, CatalogueSubscription>,
    kv: jetstream::kv::Store,
    jetstream: jetstream::Context,
    /// Postgres pool — the live membership test and backfill run the same SQL
    /// list query the data browser uses (via [`query_match`]), pinned to the
    /// just-ingested entry.
    db: PgPool,
}

impl SubscriptionManager {
    pub fn new(kv: jetstream::kv::Store, jetstream: jetstream::Context, db: PgPool) -> Self {
        Self {
            cache: DashMap::new(),
            kv,
            jetstream,
            db,
        }
    }

    /// Load all existing subscriptions from KV into the in-memory cache.
    pub async fn hydrate(&self) -> Result<(), async_nats::Error> {
        let mut keys = match self.kv.keys().await {
            Ok(k) => k,
            Err(e) => {
                // If the bucket is empty, keys() may return an error on some
                // NATS versions. Treat as empty.
                tracing::debug!("catalogue subscriptions KV keys() returned: {e}");
                return Ok(());
            }
        };

        use futures::StreamExt;
        while let Some(key_result) = keys.next().await {
            let key = match key_result {
                Ok(k) => k,
                Err(e) => {
                    tracing::warn!("catalogue subscriptions KV key error: {e}");
                    continue;
                }
            };
            match self.kv.get(&key).await {
                Ok(Some(value)) => {
                    match serde_json::from_slice::<CatalogueSubscription>(&value) {
                        Ok(sub) => {
                            self.cache.insert(sub.subscription_id.clone(), sub);
                        }
                        Err(e) => {
                            // KV row is now poisoned — won't be recoverable
                            // until something rewrites the key. Loud so an
                            // operator sees they've got a stranded
                            // subscription, not a quietly-missing trigger.
                            record_silent_drop_with(
                                "catalogue_subscription_hydrate",
                                &e,
                                serde_json::json!({
                                    "kv_bucket": "CATALOGUE_SUBSCRIPTIONS",
                                    "key": key.as_str(),
                                }),
                                Some(value.as_ref()),
                            );
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(key = %key, "failed to get subscription: {e}");
                }
            }
        }

        tracing::info!(count = self.cache.len(), "hydrated catalogue subscriptions");
        Ok(())
    }

    /// Spawn a background task that watches the KV bucket for changes and
    /// keeps the in-memory cache in sync.
    pub async fn start_watcher(self: Arc<Self>) -> Result<(), async_nats::Error> {
        let mut watcher = self.kv.watch_all().await?;

        tokio::spawn(async move {
            use futures::StreamExt;
            while let Some(entry) = watcher.next().await {
                match entry {
                    Ok(entry) => {
                        let key = entry.key.clone();
                        match entry.operation {
                            jetstream::kv::Operation::Put => {
                                match serde_json::from_slice::<CatalogueSubscription>(&entry.value)
                                {
                                    Ok(sub) => {
                                        tracing::debug!(
                                            subscription_id = %sub.subscription_id,
                                            "subscription cache: put"
                                        );
                                        self.cache.insert(sub.subscription_id.clone(), sub);
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            key = %key,
                                            "subscription watcher: bad value: {e}"
                                        );
                                    }
                                }
                            }
                            jetstream::kv::Operation::Delete | jetstream::kv::Operation::Purge => {
                                // Key format: sub.{subscription_id}
                                let sub_id = key.strip_prefix("sub.").unwrap_or(&key);
                                if self.cache.remove(sub_id).is_some() {
                                    tracing::debug!(
                                        subscription_id = %sub_id,
                                        "subscription cache: removed"
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("subscription watcher error: {e}");
                    }
                }
            }

            tracing::warn!("subscription KV watcher ended");
        });

        Ok(())
    }

    /// Create a new subscription. If `backfill` is true, query existing
    /// catalogue entries that match the query and publish a signal for each.
    pub async fn subscribe(
        &self,
        request: SubscribeRequest,
    ) -> Result<String, SubscriptionError> {
        let subscription_id = uuid::Uuid::new_v4().to_string();

        let workspace_id = request
            .workspace_id
            .as_deref()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .unwrap_or_else(uuid::Uuid::nil);

        let sub = CatalogueSubscription {
            subscription_id: subscription_id.clone(),
            net_id: request.net_id.clone(),
            workspace_id,
            signal_place: request.signal_place.clone(),
            query: request.query.clone(),
            backfill: request.backfill,
            created_at: Utc::now(),
        };

        let value =
            serde_json::to_vec(&sub).map_err(|e| SubscriptionError::Internal(e.to_string()))?;

        let kv_key = format!("sub.{subscription_id}");
        self.kv
            .put(&kv_key, value.into())
            .await
            .map_err(|e| SubscriptionError::Internal(format!("KV put failed: {e}")))?;

        tracing::info!(
            subscription_id = %subscription_id,
            net_id = %request.net_id,
            signal_place = %request.signal_place,
            backfill = request.backfill,
            "catalogue subscription created"
        );

        // Backfill: query existing entries matching the query and publish signals
        if request.backfill {
            self.run_backfill(&sub).await;
        }

        Ok(subscription_id)
    }

    /// Remove a subscription by ID. Returns `true` if it existed.
    pub async fn unsubscribe(&self, subscription_id: &str) -> Result<bool, SubscriptionError> {
        let kv_key = format!("sub.{subscription_id}");

        // Check if it exists in cache (fast path)
        let existed = self.cache.contains_key(subscription_id);

        // Delete from KV — the watcher will remove it from cache
        match self.kv.purge(&kv_key).await {
            Ok(_) => {
                tracing::info!(subscription_id = %subscription_id, "subscription removed");
                Ok(existed)
            }
            Err(e) => {
                tracing::warn!(
                    subscription_id = %subscription_id,
                    "failed to delete subscription from KV: {e}"
                );
                // If the key didn't exist, purge may error — treat as not-found
                Ok(false)
            }
        }
    }

    /// Evaluate a newly ingested artifact against all cached subscriptions.
    /// On match, publish an `ExternalSignal` to the subscribing net.
    ///
    /// Returns the list of matched subscriptions so the caller can record
    /// egress-side cross-links in `causality_cross_links` for provenance
    /// tracking. Each entry contains the `signal_key` that was embedded in
    /// the published signal — the caller must use this same key when
    /// inserting the cross-link row so that the ingress-side UPDATE (in the
    /// TokenCreated handler) can find it.
    pub async fn evaluate_new_artifact(&self, entry: &CatalogueEntry) -> Vec<MatchedSubscription> {
        // Snapshot the cache so we don't hold a DashMap read guard across the
        // `.await` membership probes (each is a pinned SQL list query).
        let subs: Vec<CatalogueSubscription> =
            self.cache.iter().map(|r| r.value().clone()).collect();

        let now = Utc::now();
        let mut matched = Vec::new();
        for sub in &subs {
            match query_match::entry_satisfies(
                &self.db,
                sub.workspace_id,
                &sub.query,
                now,
                &entry.execution_id,
                &entry.id,
            )
            .await
            {
                Ok(true) => {
                    let signal_key = build_subscription_signal_key(
                        &sub.subscription_id,
                        &entry.execution_id,
                        &entry.id,
                    );
                    self.publish_signal(sub, entry, &signal_key).await;
                    matched.push(MatchedSubscription {
                        subscription_id: sub.subscription_id.clone(),
                        target_net_id: sub.net_id.clone(),
                        signal_key,
                    });
                }
                Ok(false) => {}
                Err(e) => {
                    // Fail closed: a malformed query / DB hiccup must not fire a
                    // spurious signal, but it should be loud so the subscription
                    // owner can see their filter is broken.
                    tracing::warn!(
                        subscription_id = %sub.subscription_id,
                        query = %sub.query,
                        "subscription membership query failed; not firing: {e}"
                    );
                }
            }
        }
        matched
    }

    /// Delete all subscriptions belonging to a given net_id.
    pub async fn cleanup_net_subscriptions(&self, net_id: &str) {
        let to_remove: Vec<String> = self
            .cache
            .iter()
            .filter(|r| r.value().net_id == net_id)
            .map(|r| r.value().subscription_id.clone())
            .collect();

        if to_remove.is_empty() {
            return;
        }

        tracing::info!(
            net_id = %net_id,
            count = to_remove.len(),
            "cleaning up catalogue subscriptions for net"
        );

        for sub_id in &to_remove {
            let kv_key = format!("sub.{sub_id}");
            if let Err(e) = self.kv.purge(&kv_key).await {
                tracing::warn!(
                    subscription_id = %sub_id,
                    "failed to purge subscription during cleanup: {e}"
                );
            }
        }
    }

    /// Publish a signal to the PETRI_GLOBAL stream for a matching subscription.
    ///
    /// `signal_key` must be unique per (subscription, artifact) pair — use
    /// `build_subscription_signal_key` to construct it. It is embedded in
    /// the signal payload and becomes the PK of the corresponding
    /// `causality_cross_links` row when the TokenCreated handler fires.
    async fn publish_signal(
        &self,
        sub: &CatalogueSubscription,
        entry: &CatalogueEntry,
        signal_key: &str,
    ) {
        // `dedup_id` is stable per (subscription, artifact) — identical to
        // `msg_id` below. Populating it lets the subscriber engine's DedupIndex
        // drop duplicate TokenCreated events even when the JetStream 120s
        // duplicate window on PETRI_GLOBAL has expired (e.g., after a Mekhan
        // consumer restart or replay). Without this, the engine signal listener
        // reads `signal.dedup_id = None` and the time-unbounded safety net is
        // effectively disabled for catalogue-driven subscription signals.
        let dedup_id = format!(
            "cat-sig-{}-{}-{}",
            sub.subscription_id, entry.execution_id, entry.id
        );

        let payload = serde_json::json!({
            "source": "catalogue",
            "signal_key": signal_key,
            "dedup_id": dedup_id,
            "payload": {
                "source": "catalogue",
                "subscription_id": sub.subscription_id,
                "artifact": entry,
            },
            "timestamp": Utc::now().to_rfc3339(),
            "traceparent": null,
        });

        // The subscriber net is a mekhan instance net whose engine signal
        // listener filters `petri.{ws}.{net}.signal.>`. The old
        // `petri.signal.{net}.{place}` shape lands in PETRI_GLOBAL but matches
        // no consumer → the subscription token is never delivered.
        let subject = crate::nats::subjects::Subjects::signal_transfer(
            &sub.workspace_id.to_string(),
            &sub.net_id,
            &sub.signal_place,
        );
        let msg_id = dedup_id.clone();

        let mut headers = async_nats::HeaderMap::new();
        headers.insert("Nats-Msg-Id", msg_id.as_str());

        let payload_bytes = match serde_json::to_vec(&payload) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(
                    subscription_id = %sub.subscription_id,
                    artifact_id = %entry.id,
                    "failed to serialize signal payload: {e}"
                );
                return;
            }
        };

        match self
            .jetstream
            .publish_with_headers(subject.clone(), headers, payload_bytes.into())
            .await
        {
            Ok(ack_future) => {
                // Await the publish ack to confirm it landed in the stream
                match ack_future.await {
                    Ok(_) => {
                        tracing::debug!(
                            subscription_id = %sub.subscription_id,
                            artifact_id = %entry.id,
                            subject = %subject,
                            "catalogue signal published"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            subscription_id = %sub.subscription_id,
                            artifact_id = %entry.id,
                            subject = %subject,
                            "catalogue signal publish ack failed (stream may not exist): {e}"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    subscription_id = %sub.subscription_id,
                    artifact_id = %entry.id,
                    subject = %subject,
                    "catalogue signal publish failed: {e}"
                );
            }
        }
    }

    /// Run backfill for a newly created subscription: query the catalogue for
    /// matching entries (via the SAME DSL list query the data browser uses) and
    /// publish a signal for each.
    async fn run_backfill(&self, sub: &CatalogueSubscription) {
        let now = Utc::now();
        match query_match::backfill_matches(&self.db, sub.workspace_id, &sub.query, now).await {
            Ok(paginated) => {
                tracing::info!(
                    subscription_id = %sub.subscription_id,
                    count = paginated.items.len(),
                    total = paginated.total,
                    "backfilling subscription"
                );

                for entry in &paginated.items {
                    let signal_key = build_subscription_signal_key(
                        &sub.subscription_id,
                        &entry.execution_id,
                        &entry.id,
                    );
                    self.publish_signal(sub, entry, &signal_key).await;
                }

                // backfill_matches is bounded by one page; if the total exceeds
                // what was delivered, log it (paginating through all results is
                // a deliberate follow-up, not a silent partial backfill).
                if paginated.total > paginated.items.len() as i64 {
                    tracing::warn!(
                        subscription_id = %sub.subscription_id,
                        total = paginated.total,
                        delivered = paginated.items.len(),
                        "backfill truncated — only first page delivered"
                    );
                }
            }
            Err(e) => {
                tracing::error!(
                    subscription_id = %sub.subscription_id,
                    query = %sub.query,
                    "backfill query failed: {e}"
                );
            }
        }
    }
}

/// Build the unique `signal_key` for a subscription-triggered signal.
///
/// The format is `cat-sub:{subscription_id}:{execution_id}:{artifact_id}`,
/// which is unique per (subscription, artifact) pair and enables the
/// causality ingest to record per-subscription cross-links. The backfill
/// fallback in the TokenCreated handler parses this format to resolve the
/// source artifact when no egress-side row exists yet.
pub fn build_subscription_signal_key(
    subscription_id: &str,
    execution_id: &str,
    artifact_id: &str,
) -> String {
    format!("cat-sub:{subscription_id}:{execution_id}:{artifact_id}")
}

/// Convert a `CatalogueRegisterCommand` to a `CatalogueEntry` for filter
/// evaluation. The `catalogued_at` field is set to `now()` since the entry
/// was just inserted.
pub fn command_to_entry(cmd: &CatalogueRegisterCommand) -> CatalogueEntry {
    CatalogueEntry {
        entry_id: None,
        content_hash: cmd.content_hash.clone(),
        id: cmd.artifact_id.clone(),
        execution_id: cmd.execution_id.clone(),
        job_id: Some(cmd.job_id.clone()),
        name: cmd.name.clone(),
        category: cmd.category.clone(),
        filename: cmd.filename.clone(),
        mime_type: cmd.mime_type.clone(),
        size_bytes: cmd.size_bytes.map(|s| s as i64),
        storage_path: cmd.storage_path.clone(),
        source_net: cmd.source_net.clone(),
        source_place: cmd.source_place.clone(),
        signal_key: cmd.signal_key.clone(),
        process_id: cmd.process_id.clone(),
        process_step: cmd.process_step.clone(),
        source_event_sequence: None,
        file_metadata: cmd.file_metadata.clone().unwrap_or_default(),
        user_metadata: serde_json::to_value(&cmd.user_metadata).unwrap_or_default(),
        created_at: cmd.created_at,
        catalogued_at: Utc::now(),
        // Filter-evaluation only; authorship is resolved on the projector path.
        created_by: None,
        // Filter-evaluation only; the display view is hydrated on the read path.
        metadata_view: None,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SubscriptionError {
    #[error("internal error: {0}")]
    Internal(String),
}
