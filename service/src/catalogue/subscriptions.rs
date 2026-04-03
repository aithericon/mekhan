//! Reactive catalogue subscription subsystem (ADR-17, Phase 3A).
//!
//! When a new artifact is registered in the catalogue, the subscription manager
//! evaluates all active subscriptions. If the artifact matches a subscription's
//! filters, an `ExternalSignal` is published to the subscribing Petri net via
//! JetStream so it lands in the `PETRI_GLOBAL` stream.
//!
//! Subscriptions are persisted in a NATS KV bucket (`CATALOGUE_SUBSCRIPTIONS`)
//! and cached in-memory via `DashMap` for fast evaluation on the hot path.

use std::collections::HashMap;
use std::sync::Arc;

use async_nats::jetstream;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use super::model::{CatalogueEntry, CatalogueRegisterCommand};
use super::protocol::SubscribeRequest;
use super::repository::CatalogueRepository;

/// A persisted catalogue subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogueSubscription {
    pub subscription_id: String,
    pub net_id: String,
    pub signal_place: String,
    /// Outer key = field name, inner = `{ operator: value }`.
    /// Currently only `eq` is supported.
    pub filters: HashMap<String, HashMap<String, String>>,
    pub backfill: bool,
    pub created_at: DateTime<Utc>,
}

/// Manages catalogue subscriptions with an in-memory cache backed by NATS KV.
pub struct SubscriptionManager {
    cache: DashMap<String, CatalogueSubscription>,
    kv: jetstream::kv::Store,
    jetstream: jetstream::Context,
}

impl SubscriptionManager {
    pub fn new(kv: jetstream::kv::Store, jetstream: jetstream::Context) -> Self {
        Self {
            cache: DashMap::new(),
            kv,
            jetstream,
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
                            tracing::warn!(key = %key, "failed to deserialize subscription: {e}");
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
                                match serde_json::from_slice::<CatalogueSubscription>(
                                    &entry.value,
                                ) {
                                    Ok(sub) => {
                                        tracing::debug!(
                                            subscription_id = %sub.subscription_id,
                                            "subscription cache: put"
                                        );
                                        self.cache
                                            .insert(sub.subscription_id.clone(), sub);
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            key = %key,
                                            "subscription watcher: bad value: {e}"
                                        );
                                    }
                                }
                            }
                            jetstream::kv::Operation::Delete
                            | jetstream::kv::Operation::Purge => {
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
    /// catalogue entries that match the filters and publish a signal for each.
    pub async fn subscribe(
        &self,
        request: SubscribeRequest,
        repo: &dyn CatalogueRepository,
    ) -> Result<String, SubscriptionError> {
        let subscription_id = uuid::Uuid::new_v4().to_string();

        let sub = CatalogueSubscription {
            subscription_id: subscription_id.clone(),
            net_id: request.net_id.clone(),
            signal_place: request.signal_place.clone(),
            filters: request.filters.clone(),
            backfill: request.backfill,
            created_at: Utc::now(),
        };

        let value = serde_json::to_vec(&sub)
            .map_err(|e| SubscriptionError::Internal(e.to_string()))?;

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

        // Backfill: query existing entries matching filters and publish signals
        if request.backfill {
            self.run_backfill(&sub, repo).await;
        }

        Ok(subscription_id)
    }

    /// Remove a subscription by ID. Returns `true` if it existed.
    pub async fn unsubscribe(
        &self,
        subscription_id: &str,
    ) -> Result<bool, SubscriptionError> {
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
    pub async fn evaluate_new_artifact(&self, entry: &CatalogueEntry) {
        for sub_ref in self.cache.iter() {
            let sub = sub_ref.value();
            if matches_filters(sub, entry) {
                self.publish_signal(sub, entry).await;
            }
        }
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
    async fn publish_signal(&self, sub: &CatalogueSubscription, entry: &CatalogueEntry) {
        let payload = serde_json::json!({
            "source": "catalogue",
            "signal_key": format!("catalogue-{}", entry.id),
            "payload": {
                "source": "catalogue",
                "subscription_id": sub.subscription_id,
                "artifact": entry,
            },
            "timestamp": Utc::now().to_rfc3339(),
            "traceparent": null,
        });

        let subject = format!("petri.signal.{}.{}", sub.net_id, sub.signal_place);
        let msg_id = format!("cat-sig-{}-{}", sub.subscription_id, entry.id);

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
    /// matching entries and publish a signal for each.
    async fn run_backfill(&self, sub: &CatalogueSubscription, repo: &dyn CatalogueRepository) {
        // Build query params from subscription filters
        let query_params = filters_to_query_params(&sub.filters);

        match repo.list_entries(&query_params).await {
            Ok(paginated) => {
                tracing::info!(
                    subscription_id = %sub.subscription_id,
                    count = paginated.items.len(),
                    total = paginated.total,
                    "backfilling subscription"
                );

                for entry in &paginated.items {
                    if matches_filters(sub, entry) {
                        self.publish_signal(sub, entry).await;
                    }
                }

                // If there are more pages, we could iterate, but for the
                // initial implementation we limit backfill to the first page.
                // Production improvement: paginate through all results.
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
                    "backfill query failed: {e}"
                );
            }
        }
    }
}

/// Evaluate whether a catalogue entry matches a subscription's filters.
///
/// All filters must match (AND semantics). Each filter specifies a field name
/// and a map of `{ operator: value }`. Currently only `eq` is supported.
/// If a filter references a field that is `None` on the entry, the filter
/// does not match.
fn matches_filters(sub: &CatalogueSubscription, entry: &CatalogueEntry) -> bool {
    for (field, ops) in &sub.filters {
        for (operator, expected) in ops {
            if operator != "eq" {
                // Unsupported operator — treat as non-matching to be safe
                tracing::debug!(
                    subscription_id = %sub.subscription_id,
                    field = %field,
                    operator = %operator,
                    "unsupported filter operator, skipping"
                );
                return false;
            }

            let actual: Option<&str> = match field.as_str() {
                "category" => Some(&entry.category),
                "source_net" => entry.source_net.as_deref(),
                "source_place" => entry.source_place.as_deref(),
                "process_id" => entry.process_id.as_deref(),
                "process_step" => entry.process_step.as_deref(),
                "trace_id" => entry.trace_id.as_deref(),
                unknown => {
                    tracing::debug!(
                        subscription_id = %sub.subscription_id,
                        field = %unknown,
                        "unknown filter field"
                    );
                    return false;
                }
            };

            match actual {
                Some(val) if val == expected.as_str() => {}
                _ => return false,
            }
        }
    }

    true
}

/// Convert subscription filters to `QueryParams` for backfill queries.
fn filters_to_query_params(
    filters: &HashMap<String, HashMap<String, String>>,
) -> crate::query::extractor::QueryParams {
    use crate::query::filter::{Filter, FilterCondition, FilterOperator, FilterValue};
    use crate::query::pagination::PageQuery;

    let conditions: Vec<FilterCondition> = filters
        .iter()
        .flat_map(|(field, ops)| {
            ops.iter().filter_map(move |(op, value)| {
                let operator = match op.as_str() {
                    "eq" => FilterOperator::Eq,
                    _ => return None,
                };
                Some(FilterCondition {
                    field: field.clone(),
                    operator,
                    value: FilterValue::String(value.clone()),
                })
            })
        })
        .collect();

    let filter = if conditions.is_empty() {
        None
    } else {
        Some(Filter::new(conditions))
    };

    crate::query::extractor::QueryParams {
        page: PageQuery {
            page: 0,
            page_size: 1000, // Reasonable backfill batch size
        },
        filter,
        sort: None,
        metadata: None,
        file_metadata: None,
        search: None,
    }
}

/// Convert a `CatalogueRegisterCommand` to a `CatalogueEntry` for filter
/// evaluation. The `catalogued_at` field is set to `now()` since the entry
/// was just inserted.
pub fn command_to_entry(cmd: &CatalogueRegisterCommand) -> CatalogueEntry {
    CatalogueEntry {
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
        correlation_id: cmd.correlation_id.clone(),
        process_id: cmd.process_id.clone(),
        process_step: cmd.process_step.clone(),
        trace_id: cmd.trace_id.clone(),
        file_metadata: cmd.file_metadata.clone().unwrap_or_default(),
        user_metadata: serde_json::to_value(&cmd.user_metadata).unwrap_or_default(),
        created_at: cmd.created_at,
        catalogued_at: Utc::now(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SubscriptionError {
    #[error("internal error: {0}")]
    Internal(String),
}
