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
//! query is known at authoring time — "fire whenever a `category:receipt`
//! artifact appears". The dynamic `catalogue_subscribe` effect remains the
//! right answer when the query depends on runtime token data.
//!
//! ## Filter = catalogue query (convergence)
//!
//! The trigger carries a catalogue **query DSL** string — the exact same text
//! the data browser submits. Both the live-ingest membership test and the
//! backfill replay delegate to [`crate::catalogue::query_match`], which
//! compiles the DSL (server-side, at eval time, so relative dates re-resolve
//! per fire) and runs the existing catalogue list query. There is no separate
//! in-memory matcher: "would this entry appear in this query?" is answered by
//! the SAME SQL path the browser uses, pinned to the just-ingested entry.

use std::sync::Arc;

use serde_json::{json, Value};
use sqlx::PgPool;

use crate::catalogue::model::CatalogueEntry;
use crate::catalogue::query_match;
use crate::models::template::TriggerSource;
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

/// Walk every registered Catalog trigger and fire those whose query matches
/// `entry`. Called from the causality ingest pipeline on each new artifact.
///
/// The entry has already been persisted (the catalogue_entries row is inserted
/// and committed by the ingest pipeline before this runs), so the membership
/// test is a pinned catalogue list query — `entry_satisfies` resolves "would
/// this entry appear in this query?" against live SQL, not an in-memory object.
pub async fn evaluate(dispatcher: &TriggerDispatcher, entry: &CatalogueEntry) {
    let now = chrono::Utc::now();
    for rec in dispatcher.list_all() {
        if !rec.enabled {
            continue;
        }
        let TriggerSource::Catalog(cat) = &rec.source else {
            continue;
        };

        let matches = match query_match::entry_satisfies(
            dispatcher.db(),
            rec.workspace_id,
            &cat.query,
            now,
            &entry.execution_id,
            &entry.id,
        )
        .await
        {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    node_id = %rec.node_id,
                    artifact_id = %entry.id,
                    query = %cat.query,
                    "catalog trigger match query failed: {e}"
                );
                continue;
            }
        };
        if !matches {
            continue;
        }

        let payload = entry_to_payload(entry);
        match dispatcher
            .fire(
                &rec.node_id,
                payload,
                petri_api_types::DispatchOptions::default(),
                None,
            )
            .await
        {
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
/// that match its query and firing the trigger for each, in chronological
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
/// - Hard-bounded by [`query_match::BACKFILL_PAGE_SIZE`]; this is a single
///   page. If a deployment ever has >1000 historical matches we want explicit
///   pagination decisions, not a silent partial backfill.
/// - No post-filter: the compiled query IS the membership predicate, so the
///   returned page is exactly the set that would fire live.
/// - Single-active coalescing (when added) applies on top: if N entries
///   match but the workflow is already running, only one follow-up fire is
///   needed because the retrain re-reads all observations anyway.
pub async fn backfill_one(
    dispatcher: Arc<TriggerDispatcher>,
    node_id: String,
    workspace_id: uuid::Uuid,
    query: String,
    db: PgPool,
) {
    let now = chrono::Utc::now();
    let paginated = match query_match::backfill_matches(&db, workspace_id, &query, now).await {
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
        let payload = entry_to_payload(entry);
        match dispatcher
            .fire(
                &node_id,
                payload,
                petri_api_types::DispatchOptions::default(),
                None,
            )
            .await
        {
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
