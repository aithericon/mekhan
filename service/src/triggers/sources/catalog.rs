//! Catalog trigger source (Phase 5c).
//!
//! Catalog triggers are an *additional* authoring surface that lives alongside
//! the engine's runtime `catalogue_subscribe` effect handler (ADR 17). Both
//! surfaces produce signals through the same `SubscriptionManager` /
//! `CatalogueEntry` source of truth; this module just adds a static,
//! template-authored entry point that does not require modeling a startup
//! transition in the workflow itself.
//!
//! Static catalog triggers earn their place for the common case where the
//! filter values are known at authoring time — "fire whenever a `category =
//! receipt` artifact appears". The dynamic `catalogue_subscribe` effect
//! remains the right answer when filters depend on runtime token data.
//!
//! Phase 5c scope: filter evaluation, fan-out to spawn (Start target) and
//! in-flight signal (any other target). The dispatcher's `fire` path already
//! handles both cases via the trigger's resolved `kind` (Spawn vs Signal), so
//! this module is mostly a filter walker that calls `fire`.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};
use sqlx::PgPool;

use crate::catalogue::model::CatalogueEntry;
use crate::catalogue::repository::{CatalogueRepository, PgCatalogueRepository};
use crate::catalogue::subscriptions::filters_to_query_params;
use crate::models::template::TriggerSource;
use crate::query::pagination::{Sort, SortDirection};
use crate::triggers::dispatcher::TriggerDispatcher;

/// Build the fire payload from a `CatalogueEntry`. Flat scope map keyed by
/// `triggers::scope::source_scope(Catalog)`: every column hoisted to the top
/// level (so authors write `category`, `filename`, … directly), plus
/// `catalogue_entry` as a `Json` escape hatch and the dispatch `fire_time`.
/// Shared between live evaluation and backfill so guards/payload mappings
/// see the identical shape on either path.
fn entry_to_payload(entry: &CatalogueEntry) -> Value {
    let mut scope = match serde_json::to_value(entry) {
        Ok(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    scope.insert("catalogue_entry".to_string(), json!(entry));
    scope.insert(
        "fire_time".to_string(),
        json!(chrono::Utc::now().to_rfc3339()),
    );
    Value::Object(scope)
}

/// Walk every registered Catalog trigger and fire those whose filters match
/// `entry`. Called from the causality ingest pipeline on each new artifact.
pub async fn evaluate(dispatcher: &TriggerDispatcher, entry: &CatalogueEntry) {
    for rec in dispatcher.list_all() {
        if !rec.enabled {
            continue;
        }
        let TriggerSource::Catalog(cat) = &rec.source else {
            continue;
        };
        if !matches_filters(&cat.filters, entry) {
            continue;
        }

        let payload = entry_to_payload(entry);
        match dispatcher.fire(&rec.node_id, payload).await {
            Ok(result) => {
                tracing::info!(
                    node_id = %rec.node_id,
                    artifact_id = %entry.id,
                    outcome = ?result.outcome,
                    "catalog trigger fired"
                );
            }
            Err(e) => {
                tracing::warn!(
                    node_id = %rec.node_id,
                    artifact_id = %entry.id,
                    "catalog trigger fire failed: {e}"
                );
            }
        }
    }
}

/// Backfill a single Catalog trigger by walking existing catalogue entries
/// that match its filters and firing the trigger for each, in chronological
/// (`catalogued_at ASC`) order so the workflow sees them in the order they
/// arrived — important for BO-style retrain workflows where each replay
/// approximates a fresh observation.
///
/// Spawned from `TriggerDispatcher::register_template` when the caller asks
/// for backfill (`do_backfill=true`) AND the trigger node is newly added
/// (wasn't in the prior in-memory record set for this template+version) AND
/// `cat.backfill == true` AND the trigger is `enabled`. The "newly added"
/// check is what stops backfill from re-running on a trigger toggle.
///
/// Boundaries:
/// - Hard-bounded by `filters_to_query_params`'s `page_size = 1000`; this is
///   a single page. If a deployment ever has >1000 historical matches we
///   want explicit pagination decisions, not a silent partial backfill.
/// - Post-filters with `matches_filters` for parity with the live-evaluate
///   path; the DB query can in principle accept fields/operators the
///   in-memory matcher would reject.
/// - Single-active coalescing (when added) applies on top: if N entries
///   match but the workflow is already running, only one follow-up fire is
///   needed because the retrain re-reads all observations anyway.
pub async fn backfill_one(
    dispatcher: Arc<TriggerDispatcher>,
    node_id: String,
    filters: HashMap<String, HashMap<String, String>>,
    db: PgPool,
) {
    let mut params = filters_to_query_params(&filters);
    // Replay oldest-first.
    params.sort = Some(Sort::new("catalogued_at", SortDirection::Asc));

    let repo = PgCatalogueRepository::new(db);
    let paginated = match repo.list_entries(&params).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(node_id = %node_id, "catalog trigger backfill query failed: {e}");
            return;
        }
    };

    let total = paginated.items.len();
    tracing::info!(
        node_id = %node_id,
        count = total,
        total_matching = paginated.total,
        "starting catalog trigger backfill"
    );

    let mut fired = 0usize;
    for entry in &paginated.items {
        if !matches_filters(&filters, entry) {
            continue;
        }
        let payload = entry_to_payload(entry);
        match dispatcher.fire(&node_id, payload).await {
            Ok(_) => fired += 1,
            Err(e) => {
                tracing::warn!(
                    node_id = %node_id,
                    artifact_id = %entry.id,
                    "catalog trigger backfill fire failed: {e}"
                );
            }
        }
    }
    tracing::info!(
        node_id = %node_id,
        fired,
        total_matching = paginated.total,
        "catalog trigger backfill complete"
    );
}

/// Re-implementation of `subscriptions::matches_filters` over the same wire
/// shape but accepting the dispatcher-side `HashMap` directly. Currently only
/// `eq` is supported, matching the existing subscription contract.
fn matches_filters(
    filters: &HashMap<String, HashMap<String, String>>,
    entry: &CatalogueEntry,
) -> bool {
    for (field, ops) in filters {
        for (operator, expected) in ops {
            if operator != "eq" {
                tracing::debug!(field, operator, "unsupported filter operator");
                return false;
            }
            let actual: Option<&str> = match field.as_str() {
                "category" => Some(&entry.category),
                "source_net" => entry.source_net.as_deref(),
                "source_place" => entry.source_place.as_deref(),
                "process_id" => entry.process_id.as_deref(),
                "process_step" => entry.process_step.as_deref(),
                "name" => Some(&entry.name),
                "filename" => Some(&entry.filename),
                _ => return false,
            };
            match actual {
                Some(v) if v == expected.as_str() => {}
                _ => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> CatalogueEntry {
        CatalogueEntry {
            id: "art-1".to_string(),
            execution_id: "exec-1".to_string(),
            job_id: None,
            name: "Receipt 1".to_string(),
            category: "receipt".to_string(),
            filename: "r.pdf".to_string(),
            mime_type: Some("application/pdf".to_string()),
            size_bytes: Some(1234),
            storage_path: Some("bucket/key".to_string()),
            source_net: Some("mekhan-1".to_string()),
            source_place: Some("p_out".to_string()),
            signal_key: None,
            process_id: None,
            process_step: None,
            source_event_sequence: None,
            file_metadata: serde_json::Value::Null,
            user_metadata: serde_json::Value::Null,
            created_at: chrono::Utc::now(),
            catalogued_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn matches_when_filter_equals_field() {
        let mut filters = HashMap::new();
        let mut eq = HashMap::new();
        eq.insert("eq".to_string(), "receipt".to_string());
        filters.insert("category".to_string(), eq);
        assert!(matches_filters(&filters, &entry()));
    }

    #[test]
    fn no_match_when_value_differs() {
        let mut filters = HashMap::new();
        let mut eq = HashMap::new();
        eq.insert("eq".to_string(), "invoice".to_string());
        filters.insert("category".to_string(), eq);
        assert!(!matches_filters(&filters, &entry()));
    }

    #[test]
    fn no_match_on_unsupported_operator() {
        let mut filters = HashMap::new();
        let mut ne = HashMap::new();
        ne.insert("ne".to_string(), "invoice".to_string());
        filters.insert("category".to_string(), ne);
        assert!(!matches_filters(&filters, &entry()));
    }

    #[test]
    fn empty_filters_match_everything() {
        let filters = HashMap::new();
        assert!(matches_filters(&filters, &entry()));
    }
}
