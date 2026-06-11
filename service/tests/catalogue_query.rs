//! Catalogue query layer — live-DB integration suite.
//!
//! Exercises the virtual `meta.*` filter/sort fields (compiled through
//! `CATALOGUE_FIELD_SPECS`), the facets aggregation (incl. the lateral
//! `column` / `classification` dimensions), and the saved-queries CRUD against
//! a real Postgres with handcrafted fmeta-shaped `file_metadata` JSON.
//!
//! Gated on `MEKHAN__DATABASE_URL` (skips with a clear message if unset, like
//! `service/tests/analytics.rs`). Uses a per-run unique `test-catq-` namespace
//! and cleans up everything it created at start AND end.
//!
//! Run: MEKHAN__DATABASE_URL=postgres://mekhan:mekhan@localhost:20210/mekhan \
//!      cargo test -p mekhan-service --test catalogue_query

use sqlx::PgPool;
use uuid::Uuid;

use mekhan_service::catalogue::facets::{clamp_limit, facets, CatalogueDimension, FacetsResponse};
use mekhan_service::catalogue::queries::list_entries;
use mekhan_service::catalogue::saved_queries::{
    self, is_unique_violation, SavedQueryCreate, SavedQueryUpdate,
};
use mekhan_service::query::extractor::QueryParams;

/// Resolve the live DB URL, or `None` (→ skip) if the gate env is unset.
fn db_url() -> Option<String> {
    std::env::var("MEKHAN__DATABASE_URL").ok()
}

async fn connect() -> PgPool {
    let url = db_url().expect("db_url checked before connect");
    PgPool::connect(&url)
        .await
        .expect("connect to dev Postgres")
}

/// Tear down everything in the `test-catq-` namespace (start AND end — a
/// crashed prior run must not poison bucket math).
async fn cleanup(pool: &PgPool) {
    let _ = sqlx::query("DELETE FROM catalogue_entries WHERE execution_id LIKE 'test-catq-%'")
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM catalogue_saved_queries WHERE name LIKE 'test-catq-%'")
        .execute(pool)
        .await;
}

async fn insert_entry(
    pool: &PgPool,
    exec: &str,
    id: &str,
    category: &str,
    size_bytes: i64,
    file_metadata: serde_json::Value,
) {
    sqlx::query(
        "INSERT INTO catalogue_entries \
         (id, execution_id, name, category, filename, size_bytes, source_net, process_step, file_metadata) \
         VALUES ($1, $2, $1, $3, $1, $4, 'test-catq-net', 'probe', $5)",
    )
    .bind(id)
    .bind(exec)
    .bind(category)
    .bind(size_bytes)
    .bind(file_metadata)
    .execute(pool)
    .await
    .expect("insert catalogue entry");
}

/// Seed 5 entries under a unique execution_id namespace; returns the exec id.
///
/// fmeta-shaped JSON per the serializer (fields with `skip_serializing_if`
/// ABSENT when empty): a csv with column_names + email classification +
/// data_quality, a second csv, a png with `format_specific.Image`, a netcdf
/// with dimensions, and one with NO probe data (`{}`).
async fn seed(pool: &PgPool) -> String {
    let run = Uuid::new_v4().simple().to_string();
    let exec = format!("test-catq-{run}");

    // e1: big csv — 1000 rows, columns email+name, email classified.
    insert_entry(
        pool,
        &exec,
        "e1",
        "dataset",
        5000,
        serde_json::json!({
            "format": "csv",
            "mime_type": "text/csv",
            "file_size_bytes": 5000,
            "num_rows": 1000,
            "num_columns": 2,
            "column_names": ["email", "name"],
            "columns": [
                {"name": "email", "data_type": "string", "nullable": false,
                 "classifications": [{"category": "email", "confidence": 0.95}]},
                {"name": "name", "data_type": "string", "nullable": true}
            ],
            "data_quality": {"row_count": 1000, "completeness": 0.98},
            "extracted_at": "2026-06-10T00:00:00Z"
        }),
    )
    .await;

    // e2: small csv — 50 rows, columns email+age, email AND age classified.
    insert_entry(
        pool,
        &exec,
        "e2",
        "dataset",
        1500,
        serde_json::json!({
            "format": "csv",
            "num_rows": 50,
            "num_columns": 2,
            "column_names": ["email", "age"],
            "columns": [
                {"name": "email", "data_type": "string", "nullable": false,
                 "classifications": [{"category": "email", "confidence": 0.9}]},
                {"name": "age", "data_type": "integer", "nullable": true,
                 "classifications": [{"category": "age", "confidence": 0.8}]}
            ],
            "data_quality": {"row_count": 50, "completeness": 0.5},
            "extracted_at": "2026-06-10T00:00:00Z"
        }),
    )
    .await;

    // e3: image — format_specific Image details.
    insert_entry(
        pool,
        &exec,
        "e3",
        "plot",
        200_000,
        serde_json::json!({
            "format": "png",
            "mime_type": "image/png",
            "format_specific": {"format": "Image", "details": {"width": 1920, "height": 1080}},
            "extracted_at": "2026-06-10T00:00:00Z"
        }),
    )
    .await;

    // e4: netcdf with dimensions.
    insert_entry(
        pool,
        &exec,
        "e4",
        "dataset",
        7777,
        serde_json::json!({
            "format": "netcdf",
            "dimensions": [{"name": "time", "size": 24}, {"name": "lat", "size": 90}],
            "extracted_at": "2026-06-10T00:00:00Z"
        }),
    )
    .await;

    // e5: no probe data at all.
    insert_entry(pool, &exec, "e5", "log", 42, serde_json::json!({})).await;

    exec
}

fn scoped(exec: &str, extra: &str) -> QueryParams {
    let qs = if extra.is_empty() {
        format!("filter[execution_id][eq]={exec}")
    } else {
        format!("filter[execution_id][eq]={exec}&{extra}")
    };
    QueryParams::from_query_str(&qs).expect("parse query string")
}

fn ids(
    page: &mekhan_service::query::pagination::Paginated<
        mekhan_service::catalogue::model::CatalogueEntry,
    >,
) -> Vec<String> {
    page.items.iter().map(|e| e.id.clone()).collect()
}

fn bucket<'a>(
    resp: &'a FacetsResponse,
    key: &str,
) -> Option<&'a mekhan_service::catalogue::facets::FacetBucket> {
    resp.buckets.iter().find(|b| b.key == key)
}

#[tokio::test]
async fn meta_filters_and_sort_via_list_entries() {
    let Some(_url) = db_url() else {
        eprintln!(
            "SKIP meta_filters_and_sort_via_list_entries: set MEKHAN__DATABASE_URL \
             (e.g. postgres://mekhan:mekhan@localhost:20210/mekhan) to run"
        );
        return;
    };
    let pool = connect().await;
    cleanup(&pool).await;
    let exec = seed(&pool).await;

    // filter[meta.num_rows][gte]=100 → only the 1000-row csv (e2 has 50; the
    // png/netcdf/empty rows have NO num_rows key → NULL → excluded).
    let page = list_entries(&pool, &scoped(&exec, "filter[meta.num_rows][gte]=100"))
        .await
        .expect("meta.num_rows gte");
    assert_eq!(ids(&page), ["e1"], "only the 1000-row csv passes gte=100");
    assert_eq!(page.total, 1);

    // filter[meta.format][eq]=csv → both csvs.
    let page = list_entries(
        &pool,
        &scoped(&exec, "filter[meta.format][eq]=csv&sort=name"),
    )
    .await
    .expect("meta.format eq");
    assert_eq!(ids(&page), ["e1", "e2"]);

    // sort=-meta.num_rows orders by the casted JSONB projection.
    let page = list_entries(
        &pool,
        &scoped(&exec, "filter[meta.format][eq]=csv&sort=-meta.num_rows"),
    )
    .await
    .expect("sort -meta.num_rows");
    assert_eq!(ids(&page), ["e1", "e2"], "1000 rows before 50 rows");
    let page = list_entries(
        &pool,
        &scoped(&exec, "filter[meta.format][eq]=csv&sort=meta.num_rows"),
    )
    .await
    .expect("sort meta.num_rows asc");
    assert_eq!(ids(&page), ["e2", "e1"], "ascending flips it");

    // A float-valued virtual field.
    let page = list_entries(&pool, &scoped(&exec, "filter[meta.completeness][gte]=0.9"))
        .await
        .expect("meta.completeness gte");
    assert_eq!(ids(&page), ["e1"], "only the 0.98-complete csv");

    // Virtual fields compose with native ones + format_specific projections.
    let page = list_entries(
        &pool,
        &scoped(
            &exec,
            "filter[meta.width][gte]=1000&filter[category][eq]=plot",
        ),
    )
    .await
    .expect("meta.width + category");
    assert_eq!(ids(&page), ["e3"]);

    // Unknown meta field → InvalidField, no SQL executed.
    let err = list_entries(&pool, &scoped(&exec, "filter[meta.bogus][eq]=x"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            mekhan_service::query::builder::QueryError::InvalidField(..)
        ),
        "unknown virtual field rejected: {err}"
    );

    cleanup(&pool).await;
}

#[tokio::test]
async fn facets_bucket_math_exact() {
    let Some(_url) = db_url() else {
        eprintln!("SKIP facets_bucket_math_exact: set MEKHAN__DATABASE_URL to run");
        return;
    };
    let pool = connect().await;
    cleanup(&pool).await;
    let exec = seed(&pool).await;
    let params = scoped(&exec, "");
    let limit = clamp_limit(None);

    // --- format ---------------------------------------------------------------
    let by_format = facets(&pool, &params, CatalogueDimension::Format, limit)
        .await
        .expect("format facets");
    assert_eq!(by_format.group_by, "format");
    let csv = bucket(&by_format, "csv").expect("csv bucket");
    assert_eq!((csv.count, csv.bytes), (2, 6500));
    let png = bucket(&by_format, "png").expect("png bucket");
    assert_eq!((png.count, png.bytes), (1, 200_000));
    let nc = bucket(&by_format, "netcdf").expect("netcdf bucket");
    assert_eq!((nc.count, nc.bytes), (1, 7777));
    let unknown = bucket(&by_format, "unknown").expect("probe-less row");
    assert_eq!((unknown.count, unknown.bytes), (1, 42));
    assert_eq!(by_format.buckets.len(), 4);
    assert_eq!(by_format.total_count, 5);
    assert_eq!(by_format.total_bytes, 5000 + 1500 + 200_000 + 7777 + 42);
    // count DESC, key ASC: csv(2) first, then the count-1 keys alphabetical.
    let keys: Vec<&str> = by_format.buckets.iter().map(|b| b.key.as_str()).collect();
    assert_eq!(keys, ["csv", "netcdf", "png", "unknown"]);

    // --- column (lateral unnest of column_names) -------------------------------
    let by_col = facets(&pool, &params, CatalogueDimension::Column, limit)
        .await
        .expect("column facets");
    let email = bucket(&by_col, "email").expect("email column");
    assert_eq!(
        (email.count, email.bytes),
        (2, 6500),
        "both csvs have email"
    );
    let name = bucket(&by_col, "name").expect("name column");
    assert_eq!((name.count, name.bytes), (1, 5000));
    let age = bucket(&by_col, "age").expect("age column");
    assert_eq!((age.count, age.bytes), (1, 1500));
    assert_eq!(
        by_col.buckets.len(),
        3,
        "non-tabular entries add no buckets"
    );
    assert_eq!(by_col.total_count, 5, "totals cover the whole scope");

    // --- classification (per-entry DISTINCT categories) ------------------------
    let by_cls = facets(&pool, &params, CatalogueDimension::Classification, limit)
        .await
        .expect("classification facets");
    let email = bucket(&by_cls, "email").expect("email class");
    assert_eq!(
        (email.count, email.bytes),
        (2, 6500),
        "entries CONTAINING the class, deduped per entry"
    );
    let age = bucket(&by_cls, "age").expect("age class");
    assert_eq!((age.count, age.bytes), (1, 1500));
    assert_eq!(by_cls.buckets.len(), 2);

    // --- category + scope composition ------------------------------------------
    let by_cat = facets(&pool, &params, CatalogueDimension::Category, limit)
        .await
        .expect("category facets");
    assert_eq!(bucket(&by_cat, "dataset").expect("dataset").count, 3);
    assert_eq!(bucket(&by_cat, "plot").expect("plot").count, 1);
    assert_eq!(bucket(&by_cat, "log").expect("log").count, 1);

    // The facet scope honours meta.* filters too.
    let csv_only = scoped(&exec, "filter[meta.format][eq]=csv");
    let by_col_csv = facets(&pool, &csv_only, CatalogueDimension::Column, limit)
        .await
        .expect("column facets over csv scope");
    assert_eq!(by_col_csv.total_count, 2);
    assert_eq!(bucket(&by_col_csv, "email").expect("email").count, 2);

    // limit truncates buckets but not totals.
    let top1 = facets(&pool, &params, CatalogueDimension::Format, 1)
        .await
        .expect("format facets limit 1");
    assert_eq!(top1.buckets.len(), 1);
    assert_eq!(top1.buckets[0].key, "csv");
    assert_eq!(top1.total_count, 5);

    cleanup(&pool).await;
}

#[tokio::test]
async fn saved_queries_crud_round_trip() {
    let Some(_url) = db_url() else {
        eprintln!("SKIP saved_queries_crud_round_trip: set MEKHAN__DATABASE_URL to run");
        return;
    };
    let pool = connect().await;
    cleanup(&pool).await;

    let run = Uuid::new_v4().simple().to_string();
    let name = format!("test-catq-{run}");

    // Create.
    let created = saved_queries::create(
        &pool,
        &SavedQueryCreate {
            name: name.clone(),
            description: Some("big csvs".into()),
            q: "filter[meta.format][eq]=csv&sort=-meta.num_rows".into(),
            params: None,
        },
    )
    .await
    .expect("create saved query");
    assert_eq!(created.name, name);
    assert_eq!(
        created.params,
        serde_json::json!({}),
        "params defaults to {{}}"
    );

    // List (newest first) contains it.
    let listed = saved_queries::list(&pool).await.expect("list");
    assert!(listed.iter().any(|s| s.id == created.id));

    // Duplicate name → unique violation (handler maps this to 409).
    let dup = saved_queries::create(
        &pool,
        &SavedQueryCreate {
            name: name.clone(),
            description: None,
            q: "search=x".into(),
            params: None,
        },
    )
    .await
    .unwrap_err();
    assert!(
        is_unique_violation(&dup),
        "duplicate name must be a unique violation (→ 409): {dup}"
    );

    // Patch: rename + new q + params; description untouched.
    let renamed = format!("test-catq-{run}-v2");
    let updated = saved_queries::update(
        &pool,
        created.id,
        &SavedQueryUpdate {
            name: Some(renamed.clone()),
            description: None,
            q: Some("filter[meta.completeness][gte]=0.9".into()),
            params: Some(serde_json::json!({"columns": ["name"]})),
        },
    )
    .await
    .expect("update")
    .expect("row exists");
    assert_eq!(updated.name, renamed);
    assert_eq!(updated.q, "filter[meta.completeness][gte]=0.9");
    assert_eq!(updated.description.as_deref(), Some("big csvs"));
    assert_eq!(updated.params, serde_json::json!({"columns": ["name"]}));
    assert!(updated.updated_at >= created.updated_at);

    // Patch a missing id → None (handler maps to 404).
    let missing = saved_queries::update(
        &pool,
        Uuid::new_v4(),
        &SavedQueryUpdate {
            name: None,
            description: None,
            q: None,
            params: None,
        },
    )
    .await
    .expect("update missing runs");
    assert!(missing.is_none());

    // Delete → true, second delete → false (handler maps to 204 / 404).
    assert!(saved_queries::delete(&pool, created.id)
        .await
        .expect("delete"));
    assert!(!saved_queries::delete(&pool, created.id)
        .await
        .expect("re-delete"));

    cleanup(&pool).await;
}
