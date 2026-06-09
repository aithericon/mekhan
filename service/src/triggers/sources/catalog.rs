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

/// Source-of-truth predicate: does this catalogue entry match every filter
/// the trigger declares? Walked twice — once on every live ingest, and once
/// per entry during backfill. Has to match the DB-side
/// `filters_to_query_params` semantics; when the DB returns a superset (e.g.
/// because we can't translate an operator to SQL) this post-filter is what
/// makes the actual decision.
///
/// ## Grammar
///
/// Each filter is `{field: {operator: value}}`. Top-level multiple fields
/// are ANDed; multiple operators on the same field are also ANDed.
///
/// **Fields:**
/// - Bare names map to direct `CatalogueEntry` columns: `category`,
///   `source_net`, `source_place`, `process_id`, `process_step`, `name`,
///   `filename`.
/// - Dotted paths `user_metadata.<key>` and `file_metadata.<key>` read one
///   level into the respective JSONB column. Deeper paths are not supported
///   (keep the surface flat; if you need richer queries, encode them in a
///   sentinel category).
///
/// **Operators:**
/// - `eq`, `ne` — string equality / inequality (compares the JSON value's
///   rendered string for jsonb fields)
/// - `lt`, `lte`, `gt`, `gte` — numeric ordering; both sides parse to f64.
///   If either side isn't a parseable number the filter fails closed (no
///   match), which is the safer default.
///
/// Unknown operators or unknown bare fields fail closed.
fn matches_filters(
    filters: &HashMap<String, HashMap<String, String>>,
    entry: &CatalogueEntry,
) -> bool {
    for (field, ops) in filters {
        let actual = resolve_field(field, entry);
        for (operator, expected) in ops {
            if !apply_op(operator, actual.as_ref(), expected) {
                return false;
            }
        }
    }
    true
}

/// Resolve a filter field to an opaque JSON value (or None if the field is
/// unknown / absent on this entry). Operators interpret the value
/// themselves; equality / numeric semantics depend on the operator.
fn resolve_field(field: &str, entry: &CatalogueEntry) -> Option<Value> {
    // JSONB metadata access: `user_metadata.<key>` / `file_metadata.<key>`
    // — one level deep, matching the catalogue's flat-metadata convention.
    if let Some(key) = field.strip_prefix("user_metadata.") {
        return entry.user_metadata.get(key).cloned();
    }
    if let Some(key) = field.strip_prefix("file_metadata.") {
        return entry.file_metadata.get(key).cloned();
    }
    let s = match field {
        "category" => Some(entry.category.clone()),
        "source_net" => entry.source_net.clone(),
        "source_place" => entry.source_place.clone(),
        "process_id" => entry.process_id.clone(),
        "process_step" => entry.process_step.clone(),
        "name" => Some(entry.name.clone()),
        "filename" => Some(entry.filename.clone()),
        _ => None, // unknown bare field — fail closed
    };
    s.map(Value::String)
}

/// Apply a single operator. `actual` is the resolved field value (None if
/// the entry doesn't carry that field — equality/ordering always fail
/// closed in that case; `ne` is the natural exception and is the only op
/// that flips that default).
fn apply_op(operator: &str, actual: Option<&Value>, expected: &str) -> bool {
    match operator {
        "eq" => actual
            .and_then(value_as_compare_string)
            .map(|s| s == expected)
            .unwrap_or(false),
        "ne" => actual
            .and_then(value_as_compare_string)
            .map(|s| s != expected)
            // Field absent → "not equal to anything" — treat as match so
            // filters like `{user_metadata.failed: {ne: "yes"}}` accept
            // entries that don't carry the metadata key at all.
            .unwrap_or(true),
        "lt" | "lte" | "gt" | "gte" => {
            let lhs = actual.and_then(value_as_number);
            let rhs = expected.parse::<f64>().ok();
            match (lhs, rhs) {
                (Some(a), Some(e)) => match operator {
                    "lt" => a < e,
                    "lte" => a <= e,
                    "gt" => a > e,
                    "gte" => a >= e,
                    _ => unreachable!(),
                },
                _ => false, // unparseable on either side → fail closed
            }
        }
        unknown => {
            tracing::debug!(operator = unknown, "unsupported filter operator");
            false
        }
    }
}

/// Render a JSON value as a string for `eq`/`ne`. Numbers / booleans
/// stringify so a user can author `{user_metadata.step: {eq: "0"}}`
/// against a JSON `0` and still match — predictability beats strict typing
/// for an authoring surface.
fn value_as_compare_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
        // Composite values (Object/Array) aren't comparable as strings
        // here; users should target a leaf via a dotted path.
        _ => None,
    }
}

fn value_as_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> CatalogueEntry {
        CatalogueEntry {
            entry_id: None,
            content_hash: None,
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
            metadata_view: None,
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
    fn unknown_operator_fails_closed() {
        // `foo` isn't in our grammar — fail closed so a typo can't silently
        // turn into "match everything".
        let mut filters = HashMap::new();
        let mut ops = HashMap::new();
        ops.insert("foo".to_string(), "invoice".to_string());
        filters.insert("category".to_string(), ops);
        assert!(!matches_filters(&filters, &entry()));
    }

    #[test]
    fn empty_filters_match_everything() {
        let filters = HashMap::new();
        assert!(matches_filters(&filters, &entry()));
    }

    // ── Extended grammar (BO-driven additions) ────────────────────────

    fn entry_with_metadata(user_meta: Value, file_meta: Value) -> CatalogueEntry {
        CatalogueEntry {
            user_metadata: user_meta,
            file_metadata: file_meta,
            ..entry()
        }
    }

    #[test]
    fn ne_matches_when_field_differs() {
        let mut filters = HashMap::new();
        let mut ops = HashMap::new();
        ops.insert("ne".to_string(), "invoice".to_string());
        filters.insert("category".to_string(), ops);
        assert!(matches_filters(&filters, &entry()));
    }

    #[test]
    fn ne_matches_when_jsonb_field_absent() {
        // Filtering "exclude failed observations" — entries without the
        // metadata key at all should still match (ne against absent).
        let mut filters = HashMap::new();
        let mut ops = HashMap::new();
        ops.insert("ne".to_string(), "yes".to_string());
        filters.insert("user_metadata.failed".to_string(), ops);
        assert!(matches_filters(&filters, &entry()));
    }

    #[test]
    fn user_metadata_eq_matches_jsonb_string_leaf() {
        let user_meta = serde_json::json!({ "campaign": "alpha", "step": 7 });
        let mut filters = HashMap::new();
        let mut ops = HashMap::new();
        ops.insert("eq".to_string(), "alpha".to_string());
        filters.insert("user_metadata.campaign".to_string(), ops);
        assert!(matches_filters(
            &filters,
            &entry_with_metadata(user_meta, Value::Null)
        ));
    }

    #[test]
    fn user_metadata_eq_matches_jsonb_number_coerced_to_string() {
        // Numbers stringify so authors don't have to remember the underlying
        // JSON type when writing eq filters.
        let user_meta = serde_json::json!({ "step": 7 });
        let mut filters = HashMap::new();
        let mut ops = HashMap::new();
        ops.insert("eq".to_string(), "7".to_string());
        filters.insert("user_metadata.step".to_string(), ops);
        assert!(matches_filters(
            &filters,
            &entry_with_metadata(user_meta, Value::Null)
        ));
    }

    #[test]
    fn user_metadata_gt_compares_numbers() {
        // BO's "skip observations before bootstrap is complete":
        //   user_metadata.step > 5
        let user_meta = serde_json::json!({ "step": 10 });
        let mut filters = HashMap::new();
        let mut ops = HashMap::new();
        ops.insert("gt".to_string(), "5".to_string());
        filters.insert("user_metadata.step".to_string(), ops);
        assert!(matches_filters(
            &filters,
            &entry_with_metadata(user_meta.clone(), Value::Null)
        ));

        // Boundary cases for the four ordering ops.
        let mut at_threshold = HashMap::new();
        at_threshold.insert("gt".to_string(), "10".to_string());
        let mut at = HashMap::new();
        at.insert("user_metadata.step".to_string(), at_threshold);
        assert!(
            !matches_filters(&at, &entry_with_metadata(user_meta.clone(), Value::Null)),
            "gt should be strict (10 > 10 is false)"
        );

        let mut gte_at = HashMap::new();
        gte_at.insert("gte".to_string(), "10".to_string());
        let mut gte = HashMap::new();
        gte.insert("user_metadata.step".to_string(), gte_at);
        assert!(matches_filters(
            &gte,
            &entry_with_metadata(user_meta, Value::Null)
        ));
    }

    #[test]
    fn lt_lte_fail_closed_when_field_missing() {
        // No `step` → ordering ops can't decide → fail closed (safer than
        // accepting all entries that happen to lack the field).
        let user_meta = serde_json::json!({});
        let mut filters = HashMap::new();
        let mut ops = HashMap::new();
        ops.insert("lt".to_string(), "10".to_string());
        filters.insert("user_metadata.step".to_string(), ops);
        assert!(!matches_filters(
            &filters,
            &entry_with_metadata(user_meta, Value::Null)
        ));
    }

    #[test]
    fn multiple_operators_on_same_field_are_anded() {
        // `5 < step <= 10`
        let user_meta = serde_json::json!({ "step": 10 });
        let mut ops = HashMap::new();
        ops.insert("gt".to_string(), "5".to_string());
        ops.insert("lte".to_string(), "10".to_string());
        let mut filters = HashMap::new();
        filters.insert("user_metadata.step".to_string(), ops);
        assert!(matches_filters(
            &filters,
            &entry_with_metadata(user_meta, Value::Null)
        ));
    }

    #[test]
    fn unparseable_number_fails_ordering_op_closed() {
        let user_meta = serde_json::json!({ "step": "ten" });
        let mut filters = HashMap::new();
        let mut ops = HashMap::new();
        ops.insert("gt".to_string(), "5".to_string());
        filters.insert("user_metadata.step".to_string(), ops);
        assert!(!matches_filters(
            &filters,
            &entry_with_metadata(user_meta, Value::Null)
        ));
    }

    #[test]
    fn file_metadata_path_works_too() {
        // Parity with user_metadata — same dotted grammar.
        let file_meta = serde_json::json!({ "mime": "application/json" });
        let mut filters = HashMap::new();
        let mut ops = HashMap::new();
        ops.insert("eq".to_string(), "application/json".to_string());
        filters.insert("file_metadata.mime".to_string(), ops);
        assert!(matches_filters(
            &filters,
            &entry_with_metadata(Value::Null, file_meta)
        ));
    }
}
