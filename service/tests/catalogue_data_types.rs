//! Registered data types — live-DB integration suite (Cut B).
//!
//! Exercises the promote → attach → detach → delete lifecycle against a real
//! Postgres, with seed entries embedding REAL computed schema fingerprints
//! (typed `FileMetadata` through `compute_schema_fingerprint`, then
//! serialized — never handwritten digests), plus the `schema` facet dimension
//! and the `filter[meta.schema][in]` compile path end-to-end.
//!
//! Gated on `MEKHAN__DATABASE_URL` (skips with a clear message if unset, like
//! `service/tests/catalogue_query.rs`). Uses a per-run unique `test-catdt-`
//! namespace and cleans up everything it created at start AND end. Schema
//! digests are made run-unique by including the run id as a column name, so
//! the global digest PK can never collide across runs or leftover state.
//!
//! Run: MEKHAN__DATABASE_URL=postgres://mekhan:mekhan@localhost:20210/mekhan \
//!      cargo test -p mekhan-service --test catalogue_data_types

use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use aithericon_file_metadata::compute_schema_fingerprint;
use aithericon_file_metadata::data_type::DataType;
use aithericon_file_metadata::format::FileFormat;
use aithericon_file_metadata::types::{ColumnInfo, FileMetadata};

use mekhan_service::catalogue::data_types::{self, DataTypePromote, DataTypeUpdate, PromoteError};
use mekhan_service::catalogue::facets::{clamp_limit, facets, CatalogueDimension};
use mekhan_service::catalogue::queries::list_entries;
use mekhan_service::catalogue::saved_queries::is_unique_violation;
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

/// Serializes the tests in this binary: cleanup wipes the WHOLE
/// `test-catdt-` namespace, so two tests running concurrently would delete
/// each other's seeds mid-flight.
static DB_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Tear down everything in the `test-catdt-` namespace (start AND end — a
/// crashed prior run must not poison counts; digest rows cascade off types).
async fn cleanup(pool: &PgPool) {
    let _ = sqlx::query("DELETE FROM catalogue_entries WHERE execution_id LIKE 'test-catdt-%'")
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM catalogue_data_types WHERE name LIKE 'test-catdt-%'")
        .execute(pool)
        .await;
}

fn col(name: &str, dt: DataType, nullable: bool) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        data_type: dt,
        nullable,
        metadata: HashMap::new(),
        statistics: None,
        classifications: vec![],
    }
}

/// Build a typed `FileMetadata` carrying `columns`, with a REAL computed
/// fingerprint — the seeds embed exactly what the probe would have stored.
fn meta_with_columns(columns: Vec<ColumnInfo>) -> FileMetadata {
    let mut fm = FileMetadata {
        format: FileFormat::Csv,
        mime_type: Some("text/csv".into()),
        num_rows: Some(100),
        num_columns: Some(columns.len() as u64),
        file_size_bytes: None,
        file_name: None,
        modified_at: None,
        created_at: None,
        readonly: false,
        unix_mode: None,
        column_names: columns.iter().map(|c| c.name.clone()).collect(),
        dimensions: vec![],
        columns,
        attributes: HashMap::new(),
        format_specific: None,
        preview: None,
        encrypted: None,
        checksum: None,
        schema_fingerprint: None,
        data_quality: None,
        extracted_at: chrono::Utc::now(),
    };
    compute_schema_fingerprint(&mut fm);
    fm
}

fn digest_of(fm: &FileMetadata) -> String {
    fm.schema_fingerprint
        .as_ref()
        .expect("fingerprint computed")
        .digest
        .clone()
}

async fn insert_entry(pool: &PgPool, exec: &str, id: &str, size_bytes: i64, fm: &FileMetadata) {
    sqlx::query(
        "INSERT INTO catalogue_entries \
         (id, execution_id, name, category, filename, size_bytes, source_net, process_step, file_metadata) \
         VALUES ($1, $2, $1, 'dataset', $1, $3, 'test-catdt-net', 'probe', $4)",
    )
    .bind(id)
    .bind(exec)
    .bind(size_bytes)
    .bind(serde_json::to_value(fm).expect("serialize fmeta"))
    .execute(pool)
    .await
    .expect("insert catalogue entry");
}

/// Seeded fixture: schema A on two entries, schema B on one, plus one entry
/// with no fingerprint at all. Digests are run-unique (run id is a column
/// name), so the global digest PK can't collide with anything pre-existing.
struct Fixture {
    exec: String,
    run: String,
    digest_a: String,
    digest_b: String,
}

async fn seed(pool: &PgPool) -> Fixture {
    let run = Uuid::new_v4().simple().to_string();
    let exec = format!("test-catdt-{run}");

    let schema_a = meta_with_columns(vec![
        col(&format!("run_{run}"), DataType::String, false),
        col(
            "ts",
            DataType::Timestamp {
                timezone: Some("UTC".into()),
            },
            false,
        ),
        col("score", DataType::Float64, true),
    ]);
    let schema_b = meta_with_columns(vec![
        col(&format!("run_{run}"), DataType::String, false),
        col("label", DataType::String, true),
    ]);

    insert_entry(pool, &exec, "a1", 1000, &schema_a).await;
    insert_entry(pool, &exec, "a2", 2000, &schema_a).await;
    insert_entry(pool, &exec, "b1", 3000, &schema_b).await;
    // f1: probe data with NO columns → fingerprint of the empty schema would
    // be shared across runs; store no fingerprint at all instead.
    let mut bare = meta_with_columns(vec![]);
    bare.schema_fingerprint = None;
    insert_entry(pool, &exec, "f1", 50, &bare).await;

    Fixture {
        exec,
        run,
        digest_a: digest_of(&schema_a),
        digest_b: digest_of(&schema_b),
    }
}

#[tokio::test]
async fn promote_attach_detach_delete_round_trip() {
    let Some(_url) = db_url() else {
        eprintln!(
            "SKIP promote_attach_detach_delete_round_trip: set MEKHAN__DATABASE_URL \
             (e.g. postgres://mekhan:mekhan@localhost:20210/mekhan) to run"
        );
        return;
    };
    let _gate = DB_GATE.lock().await;
    let pool = connect().await;
    cleanup(&pool).await;
    let fx = seed(&pool).await;
    let author = Uuid::new_v4();
    let name = format!("test-catdt-{}", fx.run);

    // Promote happy path: exemplar resolved, fingerprint verified, columns
    // projected HUMANIZED (incl. timestamp<UTC>), live entry_count = 2.
    let created = data_types::promote(
        &pool,
        &DataTypePromote {
            digest: fx.digest_a.clone(),
            name: name.clone(),
            description: Some("run telemetry".into()),
        },
        author,
    )
    .await
    .expect("promote schema A");
    assert_eq!(created.name, name);
    assert_eq!(created.digests, std::slice::from_ref(&fx.digest_a));
    assert_eq!(created.entry_count, 2, "a1 + a2 carry digest A");
    assert_eq!(created.created_by, Some(author));
    assert_eq!(created.updated_by, Some(author));
    let cols: Vec<(&str, &str, bool)> = created
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.data_type.as_str(), c.nullable))
        .collect();
    assert_eq!(
        cols,
        [
            (format!("run_{}", fx.run).as_str(), "string", false),
            ("ts", "timestamp<UTC>", false),
            ("score", "float64", true),
        ]
    );

    // 404: unknown digest has no exemplar.
    let err = data_types::promote(
        &pool,
        &DataTypePromote {
            digest: "ffffffffffffffff".into(),
            name: format!("{name}-none"),
            description: None,
        },
        author,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, PromoteError::NoExemplar(ref d) if d == "ffffffffffffffff"),
        "unknown digest must be NoExemplar (→ 404): {err}"
    );

    // 409: duplicate name (digest B is free, the NAME collides).
    let err = data_types::promote(
        &pool,
        &DataTypePromote {
            digest: fx.digest_b.clone(),
            name: name.clone(),
            description: None,
        },
        author,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, PromoteError::Database(ref e) if is_unique_violation(e)),
        "duplicate name must be a unique violation (→ 409): {err}"
    );

    // 409: digest already owned (fresh name, digest A is taken).
    let err = data_types::promote(
        &pool,
        &DataTypePromote {
            digest: fx.digest_a.clone(),
            name: format!("{name}-again"),
            description: None,
        },
        author,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, PromoteError::Database(ref e) if is_unique_violation(e)),
        "owned digest must be a unique violation (→ 409): {err}"
    );

    // Attach digest B: entry_count sums across owned digests (2 + 1); the
    // stored columns stay the promote-time exemplar's (variants may differ).
    let editor = Uuid::new_v4();
    let updated = data_types::update(
        &pool,
        created.id,
        &DataTypeUpdate {
            name: None,
            description: None,
            attach_digests: Some(vec![fx.digest_b.clone()]),
            detach_digests: None,
        },
        editor,
    )
    .await
    .expect("attach digest B")
    .expect("row exists");
    let mut digests = updated.digests.clone();
    digests.sort();
    let mut expected = vec![fx.digest_a.clone(), fx.digest_b.clone()];
    expected.sort();
    assert_eq!(digests, expected);
    assert_eq!(updated.entry_count, 3, "a1 + a2 + b1");
    assert_eq!(updated.columns.len(), 3, "columns stay the exemplar's");
    assert_eq!(updated.created_by, Some(author), "created_by preserved");
    assert_eq!(updated.updated_by, Some(editor), "updated_by stamped");

    // 404: attaching a digest with no exemplar fails BEFORE any write.
    let err = data_types::update(
        &pool,
        created.id,
        &DataTypeUpdate {
            name: None,
            description: None,
            attach_digests: Some(vec!["ffffffffffffffff".into()]),
            detach_digests: None,
        },
        editor,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PromoteError::NoExemplar(_)), "got: {err}");

    // 409: re-attaching an owned digest hits the global digest PK.
    let err = data_types::update(
        &pool,
        created.id,
        &DataTypeUpdate {
            name: None,
            description: None,
            attach_digests: Some(vec![fx.digest_a.clone()]),
            detach_digests: None,
        },
        editor,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, PromoteError::Database(ref e) if is_unique_violation(e)),
        "re-attach must conflict (→ 409): {err}"
    );

    // Detach digest B (unconditional); rename + description COALESCE through.
    let renamed = format!("{name}-v2");
    let updated = data_types::update(
        &pool,
        created.id,
        &DataTypeUpdate {
            name: Some(renamed.clone()),
            description: None,
            attach_digests: None,
            detach_digests: Some(vec![fx.digest_b.clone()]),
        },
        editor,
    )
    .await
    .expect("detach digest B")
    .expect("row exists");
    assert_eq!(updated.name, renamed);
    assert_eq!(
        updated.description.as_deref(),
        Some("run telemetry"),
        "untouched description survives COALESCE"
    );
    assert_eq!(updated.digests, std::slice::from_ref(&fx.digest_a));
    assert_eq!(updated.entry_count, 2, "back to schema A only");

    // Update a missing id → None (handler maps to 404).
    let missing = data_types::update(
        &pool,
        Uuid::new_v4(),
        &DataTypeUpdate {
            name: None,
            description: None,
            attach_digests: None,
            detach_digests: None,
        },
        editor,
    )
    .await
    .expect("update missing runs");
    assert!(missing.is_none());

    // List contains it; get round-trips.
    let listed = data_types::list(&pool).await.expect("list");
    assert!(listed.iter().any(|t| t.id == created.id));
    let fetched = data_types::get(&pool, created.id)
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(fetched.name, renamed);

    // Delete cascades the digest rows: digest A is free again immediately.
    assert!(data_types::delete(&pool, created.id).await.expect("delete"));
    assert!(!data_types::delete(&pool, created.id)
        .await
        .expect("re-delete"));
    let orphans: (i64,) = sqlx::query_as(
        "SELECT count(*)::bigint FROM catalogue_data_type_digests WHERE digest = ANY($1)",
    )
    .bind(vec![fx.digest_a.clone(), fx.digest_b.clone()])
    .fetch_one(&pool)
    .await
    .expect("count digest rows");
    assert_eq!(orphans.0, 0, "ON DELETE CASCADE cleared the digest rows");
    let repromoted = data_types::promote(
        &pool,
        &DataTypePromote {
            digest: fx.digest_a.clone(),
            name: format!("{name}-reborn"),
            description: None,
        },
        author,
    )
    .await
    .expect("freed digest promotes again");
    assert_eq!(repromoted.entry_count, 2);

    cleanup(&pool).await;
}

#[tokio::test]
async fn schema_facets_and_in_filter() {
    let Some(_url) = db_url() else {
        eprintln!("SKIP schema_facets_and_in_filter: set MEKHAN__DATABASE_URL to run");
        return;
    };
    let _gate = DB_GATE.lock().await;
    let pool = connect().await;
    cleanup(&pool).await;
    let fx = seed(&pool).await;

    let params = QueryParams::from_query_str(&format!("filter[execution_id][eq]={}", fx.exec))
        .expect("parse scope");

    // facets(schema): one bucket per digest + the 'none' placeholder for the
    // fingerprint-less entry, over the scoped seed set.
    let by_schema = facets(
        &pool,
        Uuid::nil(),
        &params,
        CatalogueDimension::Schema,
        clamp_limit(None),
    )
    .await
    .expect("schema facets");
    assert_eq!(by_schema.group_by, "schema");
    let bucket = |key: &str| {
        by_schema
            .buckets
            .iter()
            .find(|b| b.key == key)
            .unwrap_or_else(|| panic!("bucket {key} missing"))
    };
    assert_eq!(
        (bucket(&fx.digest_a).count, bucket(&fx.digest_a).bytes),
        (2, 3000)
    );
    assert_eq!(
        (bucket(&fx.digest_b).count, bucket(&fx.digest_b).bytes),
        (1, 3000)
    );
    assert_eq!((bucket("none").count, bucket("none").bytes), (1, 50));
    assert_eq!(by_schema.buckets.len(), 3);
    assert_eq!(by_schema.total_count, 4);

    // filter[meta.schema][in]=A,B — the compiled `= ANY` target of the
    // client-side `datatype:` sugar — returns exactly the digest-carrying rows.
    let in_params = QueryParams::from_query_str(&format!(
        "filter[execution_id][eq]={}&filter[meta.schema][in]={},{}&sort=name",
        fx.exec, fx.digest_a, fx.digest_b
    ))
    .expect("parse in filter");
    let page = list_entries(&pool, Uuid::nil(), &in_params)
        .await
        .expect("in filter");
    let ids: Vec<&str> = page.items.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["a1", "a2", "b1"], "f1 has no fingerprint → excluded");
    assert_eq!(page.total, 3);

    // Single-digest eq (what a one-digest datatype compiles to).
    let eq_params = QueryParams::from_query_str(&format!(
        "filter[execution_id][eq]={}&filter[meta.schema][eq]={}&sort=name",
        fx.exec, fx.digest_b
    ))
    .expect("parse eq filter");
    let page = list_entries(&pool, Uuid::nil(), &eq_params)
        .await
        .expect("eq filter");
    let ids: Vec<&str> = page.items.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["b1"]);

    cleanup(&pool).await;
}
