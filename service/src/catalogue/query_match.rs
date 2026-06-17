//! Shared catalogue-query EVALUATION: "would this entry appear in this query?"
//!
//! This is the convergence point for catalog triggers and catalogue
//! subscriptions (and any future surface): instead of a hand-rolled, eq-only
//! in-memory matcher over a `HashMap<String, HashMap<String,String>>`, the
//! filter IS a catalogue query (the same DSL the data browser uses, compiled by
//! [`crate::catalogue::query_dsl`]), evaluated by running the existing list
//! query pinned to the just-ingested entry.
//!
//! Two entry points:
//! - [`entry_satisfies`] — the live membership test: compile the DSL, AND-pin
//!   `id` + `execution_id` onto the filter, run the list query with `page_size
//!   = 1`, and report `total >= 1`. The catalogue UNIQUE INDEX
//!   `uq_cat_exec_id (execution_id, id)` makes this O(1).
//! - [`backfill_matches`] — replay: compile the DSL (no pin), run the list query
//!   ordered oldest-first, and return the page of matching entries.
//!
//! The single compiler means there is no divergence between "what the browser
//! shows" and "what fires the trigger / subscription".

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::catalogue::model::CatalogueEntry;
use crate::catalogue::query_dsl::{self, DatatypeRegistry};
use crate::query::builder::QueryError;
use crate::query::filter::{Filter, FilterCondition, FilterOperator, FilterValue};
use crate::query::pagination::{Sort, SortDirection};

/// Backfill page-size cap. Matches the historical
/// `subscriptions::filters_to_query_params` bound — a single page; >1000
/// historical matches wants an explicit pagination decision, not a silent
/// partial backfill.
pub const BACKFILL_PAGE_SIZE: i64 = 1000;

/// Build a concrete datatype resolver registry for a DSL string by resolving
/// every `datatype:<name>` reference against the data-types registry. Callers
/// hand `registry.as_resolver()` to [`query_dsl::compile_query`].
///
/// A thin convenience wrapper over [`DatatypeRegistry::resolve`] so the
/// trigger / subscription call sites don't reach into `query_dsl` internals.
pub async fn datatype_registry(pool: &PgPool, dsl: &str) -> Result<DatatypeRegistry, sqlx::Error> {
    DatatypeRegistry::resolve(pool, dsl).await
}

/// Membership test: does `entry` (already persisted) appear in the catalogue
/// query expressed by `dsl`, for `workspace_id`?
///
/// Compiles `dsl` to [`QueryParams`](crate::query::extractor::QueryParams),
/// then AND-pins the entry's `id` + `execution_id` onto the filter so the query
/// can match at most this one row, runs the existing `list_entries` with
/// `page_size = 1`, and returns `total >= 1`.
///
/// Resolving the datatype registry is done internally (one tiny query, only
/// when the DSL carries a `datatype:` term).
pub async fn entry_satisfies(
    pool: &PgPool,
    workspace_id: Uuid,
    dsl: &str,
    now: DateTime<Utc>,
    execution_id: &str,
    id: &str,
) -> Result<bool, QueryError> {
    let registry = datatype_registry(pool, dsl)
        .await
        .map_err(QueryError::Database)?;
    let mut params = query_dsl::compile_query(dsl, now, &registry.as_resolver());
    pin_to_entry(&mut params, execution_id, id);

    let paginated = crate::catalogue::queries::list_entries(pool, workspace_id, &params).await?;
    Ok(paginated.total >= 1)
}

/// AND-pin a compiled query to exactly one catalogue entry: append
/// `id = <id>` and `execution_id = <execution_id>` (both native columns in
/// `CATALOGUE_FIELD_SPECS`) onto the filter, and clamp pagination to a single
/// row — the membership test only needs `total >= 1`. The catalogue UNIQUE
/// INDEX `uq_cat_exec_id (execution_id, id)` makes this an O(1) probe.
fn pin_to_entry(params: &mut crate::query::extractor::QueryParams, execution_id: &str, id: &str) {
    let pins = [
        FilterCondition {
            field: "id".to_string(),
            operator: FilterOperator::Eq,
            value: FilterValue::String(id.to_string()),
        },
        FilterCondition {
            field: "execution_id".to_string(),
            operator: FilterOperator::Eq,
            value: FilterValue::String(execution_id.to_string()),
        },
    ];
    params.filter = Some(match params.filter.take() {
        Some(mut f) => {
            f.conditions.extend(pins);
            f
        }
        None => Filter::new(pins.to_vec()),
    });
    params.page.page = 0;
    params.page.page_size = 1;
}

/// Replay query: compile `dsl` and return the page of catalogue entries that
/// match, ordered oldest-first (`catalogued_at ASC`) so a workflow replaying a
/// backfill sees them in arrival order.
///
/// Bounded by [`BACKFILL_PAGE_SIZE`] — a single page; callers log when the
/// total exceeds what was delivered.
pub async fn backfill_matches(
    pool: &PgPool,
    workspace_id: Uuid,
    dsl: &str,
    now: DateTime<Utc>,
) -> Result<crate::query::pagination::Paginated<CatalogueEntry>, QueryError> {
    let registry = datatype_registry(pool, dsl)
        .await
        .map_err(QueryError::Database)?;
    let mut params = query_dsl::compile_query(dsl, now, &registry.as_resolver());
    params.sort = Some(Sort::new("catalogued_at", SortDirection::Asc));
    params.page.page = 0;
    params.page.page_size = BACKFILL_PAGE_SIZE;

    crate::catalogue::queries::list_entries(pool, workspace_id, &params).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_resolver(_: &str) -> Option<Vec<String>> {
        None
    }

    fn now() -> DateTime<Utc> {
        "2026-06-10T12:00:00.000Z".parse().unwrap()
    }

    /// Pinning onto a query with an existing filter APPENDS the two pins
    /// (keeps the DSL conditions) and clamps to a single-row page.
    #[test]
    fn pin_appends_to_compiled_filter() {
        let mut params = query_dsl::compile_query("category:model", now(), &no_resolver);
        assert_eq!(params.filter.as_ref().unwrap().conditions.len(), 1);
        pin_to_entry(&mut params, "exec-1", "art-9");

        let conds = &params.filter.as_ref().unwrap().conditions;
        assert_eq!(conds.len(), 3, "category + id + execution_id");
        assert!(conds.iter().any(|c| c.field == "category"));
        assert!(conds
            .iter()
            .any(|c| c.field == "id" && matches!(&c.value, FilterValue::String(s) if s == "art-9")));
        assert!(conds.iter().any(
            |c| c.field == "execution_id"
                && matches!(&c.value, FilterValue::String(s) if s == "exec-1")
        ));
        assert_eq!(params.page.page_size, 1);
        assert_eq!(params.page.page, 0);
    }

    /// Pinning an empty query (no DSL filter) creates a filter holding only the
    /// two pins — the membership probe still scopes to exactly one entry.
    #[test]
    fn pin_creates_filter_when_query_empty() {
        let mut params = query_dsl::compile_query("", now(), &no_resolver);
        assert!(params.filter.is_none());
        pin_to_entry(&mut params, "exec-2", "art-2");

        let conds = &params.filter.as_ref().unwrap().conditions;
        assert_eq!(conds.len(), 2);
        let fields: Vec<&str> = conds.iter().map(|c| c.field.as_str()).collect();
        assert!(fields.contains(&"id"));
        assert!(fields.contains(&"execution_id"));
        assert_eq!(params.page.page_size, 1);
    }
}
