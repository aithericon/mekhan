use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::query::builder::{self, FieldSpec, QueryError};
use crate::query::extractor::QueryParams;
use crate::query::filter::camel_to_snake_case;
use crate::query::pagination::Paginated;

use super::model::*;

/// Explicit `CatalogueEntry` column projection.
///
/// Since the content-addressed reshape (docs/32) the legacy composite-key /
/// display columns are nullable in the DB (legacy logical rows carry only a
/// `content_hash`). The `CatalogueEntry` DTO keeps a non-Option `String` view
/// for the job-net consumers, so we COALESCE those columns to `''` on read.
/// `entry_id` / `content_hash` map straight through (both Option).
const ENTRY_COLUMNS: &str = "entry_id, content_hash, \
     COALESCE(id, '') AS id, \
     COALESCE(execution_id, '') AS execution_id, \
     job_id, \
     COALESCE(name, '') AS name, \
     COALESCE(category, '') AS category, \
     COALESCE(filename, '') AS filename, \
     mime_type, size_bytes, storage_path, source_net, source_place, \
     signal_key, process_id, process_step, source_event_sequence, \
     file_metadata, user_metadata, created_at, catalogued_at";

/// Allowed filter fields for catalogue entries (whitelist).
const ALLOWED_FILTER_FIELDS: &[&str] = &[
    "id",
    "execution_id",
    "job_id",
    "name",
    "category",
    "filename",
    "mime_type",
    "storage_path",
    "source_net",
    "source_place",
    "signal_key",
    "process_id",
    "process_step",
    "created_at",
    "catalogued_at",
    "size_bytes",
    "content_hash",
];

/// Allowed sort fields for catalogue entries (whitelist of spec NAMES — the
/// emitted SQL comes from the matching [`CATALOGUE_FIELD_SPECS`] entry).
const ALLOWED_SORT_FIELDS: &[&str] = &[
    "name",
    "category",
    "size_bytes",
    "created_at",
    "catalogued_at",
    "source_net",
    "process_id",
    "execution_id",
    "content_hash",
    "meta.num_rows",
    "meta.completeness",
    "meta.format",
];

const fn spec(name: &'static str, expr: &'static str) -> FieldSpec {
    FieldSpec {
        name,
        expr,
        timestamp: false,
    }
}

const fn tspec(name: &'static str, expr: &'static str) -> FieldSpec {
    FieldSpec {
        name,
        expr,
        timestamp: true,
    }
}

/// The catalogue query-field registry: every native filter column PLUS the
/// virtual `meta.*` fields projected out of the `file_metadata` JSONB (the
/// serialized `fmeta::FileMetadata` probe output).
///
/// SECURITY POSTURE: every `expr` is a server-side constant selected by exact
/// name lookup (see [`FieldSpec`]) — no user input reaches the SQL text; user
/// values travel exclusively as binds. Casts live inside the expr so bound
/// ints/floats/bools compare natively.
///
/// This registry is the single source for WHERE compilation, ORDER BY
/// compilation, AND the `/catalogue/query-fields` endpoint — they cannot drift.
pub const CATALOGUE_FIELD_SPECS: &[FieldSpec] = &[
    // Native columns (expr == column name).
    spec("id", "id"),
    spec("execution_id", "execution_id"),
    spec("job_id", "job_id"),
    spec("name", "name"),
    spec("category", "category"),
    spec("filename", "filename"),
    spec("mime_type", "mime_type"),
    spec("storage_path", "storage_path"),
    spec("source_net", "source_net"),
    spec("source_place", "source_place"),
    spec("signal_key", "signal_key"),
    spec("process_id", "process_id"),
    spec("process_step", "process_step"),
    tspec("created_at", "created_at"),
    tspec("catalogued_at", "catalogued_at"),
    spec("size_bytes", "size_bytes"),
    spec("content_hash", "content_hash"),
    // Virtual meta.* fields — projections into file_metadata (fmeta JSONB).
    // Fields with serde `skip_serializing_if` are ABSENT when empty, so the
    // exprs yield NULL there (is_null/is_not_null work as expected).
    spec("meta.format", "(file_metadata->>'format')"),
    spec("meta.num_rows", "((file_metadata->>'num_rows')::bigint)"),
    spec(
        "meta.num_columns",
        "((file_metadata->>'num_columns')::bigint)",
    ),
    spec(
        "meta.completeness",
        "((file_metadata->'data_quality'->>'completeness')::float8)",
    ),
    spec(
        "meta.width",
        "((file_metadata->'format_specific'->'details'->>'width')::bigint)",
    ),
    spec(
        "meta.height",
        "((file_metadata->'format_specific'->'details'->>'height')::bigint)",
    ),
    spec(
        "meta.duration_secs",
        "((file_metadata->'format_specific'->'details'->>'duration_secs')::float8)",
    ),
    spec(
        "meta.schema",
        "(file_metadata->'schema_fingerprint'->>'digest')",
    ),
    spec("meta.encrypted", "((file_metadata->>'encrypted')::boolean)"),
];

/// List catalogue entries with full filter/sort/pagination support.
pub async fn list_entries(
    pool: &PgPool,
    params: &QueryParams,
) -> Result<Paginated<CatalogueEntry>, QueryError> {
    // -- COUNT query --
    let count = {
        let mut qb =
            QueryBuilder::<Postgres>::new("SELECT COUNT(*)::bigint FROM catalogue_entries");
        append_where(&mut qb, params)?;
        let row: (i64,) = qb.build_query_as().fetch_one(pool).await?;
        row.0
    };

    // -- SELECT query --
    let entries = {
        let mut qb =
            QueryBuilder::<Postgres>::new(format!("SELECT {ENTRY_COLUMNS} FROM catalogue_entries"));
        append_where(&mut qb, params)?;

        // ORDER BY — sortability is gated on ALLOWED_SORT_FIELDS (names); the
        // emitted SQL comes from the matching CATALOGUE_FIELD_SPECS expr.
        if let Some(ref sort) = params.sort {
            let normalized = camel_to_snake_case(&sort.field);
            if !ALLOWED_SORT_FIELDS.contains(&normalized.as_str()) {
                return Err(QueryError::InvalidSortField(
                    sort.field.clone(),
                    ALLOWED_SORT_FIELDS.join(", "),
                ));
            }
            builder::build_order_by_specs(&mut qb, sort, CATALOGUE_FIELD_SPECS)?;
        } else {
            qb.push(" ORDER BY created_at DESC");
        }

        // LIMIT / OFFSET
        builder::build_pagination(&mut qb, &params.page);

        qb.build_query_as::<CatalogueEntry>()
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(CatalogueEntry::hydrate_view)
            .collect()
    };

    Ok(Paginated::new(entries, count, &params.page))
}

/// Append a WHERE clause combining typed filters (resolved through
/// [`CATALOGUE_FIELD_SPECS`], so `meta.*` virtual fields work), search, and
/// JSONB containment. Returns whether a `WHERE` was emitted, so callers
/// composing extra conditions (e.g. facet lateral guards) know whether to
/// continue with ` AND ` or open the clause themselves.
pub(crate) fn append_where(
    qb: &mut QueryBuilder<'_, Postgres>,
    params: &QueryParams,
) -> Result<bool, QueryError> {
    let has_filter = params
        .filter
        .as_ref()
        .map(|f| !f.is_empty())
        .unwrap_or(false);
    let has_search = params.search.is_some();
    let has_metadata = params.metadata.is_some();
    let has_file_metadata = params.file_metadata.is_some();

    if !has_filter && !has_search && !has_metadata && !has_file_metadata {
        return Ok(false);
    }

    qb.push(" WHERE ");
    let mut need_and = false;

    // Typed filters
    if let Some(ref filter) = params.filter {
        if !filter.is_empty() {
            builder::build_where_conditions_specs(qb, filter, CATALOGUE_FIELD_SPECS)?;
            need_and = true;
        }
    }

    // Free-text search: OR across name, filename, storage_path
    if let Some(ref search) = params.search {
        if need_and {
            qb.push(" AND ");
        }
        let pattern = format!("%{search}%");
        qb.push("(name ILIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" OR filename ILIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" OR storage_path ILIKE ");
        qb.push_bind(pattern);
        qb.push(")");
        need_and = true;
    }

    // JSONB containment on user_metadata
    if let Some(ref meta) = params.metadata {
        if need_and {
            qb.push(" AND ");
        }
        builder::push_jsonb_contains(qb, "user_metadata", meta);
        need_and = true;
    }

    // JSONB containment on file_metadata
    if let Some(ref fmeta) = params.file_metadata {
        if need_and {
            qb.push(" AND ");
        }
        builder::push_jsonb_contains(qb, "file_metadata", fmeta);
    }

    Ok(true)
}

/// Get a single catalogue entry by composite key.
pub async fn get_entry(
    pool: &PgPool,
    execution_id: &str,
    id: &str,
) -> Result<Option<CatalogueEntry>, sqlx::Error> {
    sqlx::query_as::<_, CatalogueEntry>(&format!(
        "SELECT {ENTRY_COLUMNS} FROM catalogue_entries WHERE execution_id = $1 AND id = $2"
    ))
    .bind(execution_id)
    .bind(id)
    .fetch_optional(pool)
    .await
    .map(|opt| opt.map(CatalogueEntry::hydrate_view))
}

/// Aggregate statistics, optionally filtered by the same params.
pub async fn stats(pool: &PgPool, params: &QueryParams) -> Result<CatalogueStats, QueryError> {
    // Total count + size
    let (total_entries, total_size_bytes, latest_at) = {
        let mut qb = QueryBuilder::<Postgres>::new(
            "SELECT COALESCE(COUNT(*), 0)::bigint, COALESCE(SUM(size_bytes), 0)::bigint, MAX(created_at) FROM catalogue_entries",
        );
        append_where(&mut qb, params)?;
        let row: (i64, i64, Option<chrono::DateTime<chrono::Utc>>) =
            qb.build_query_as().fetch_one(pool).await?;
        row
    };

    // Per-category breakdown
    let by_category = {
        let mut qb = QueryBuilder::<Postgres>::new(
            "SELECT category, COUNT(*)::bigint as count, COALESCE(SUM(size_bytes), 0)::bigint as total_bytes FROM catalogue_entries",
        );
        append_where(&mut qb, params)?;
        qb.push(" GROUP BY category ORDER BY count DESC");
        qb.build_query_as::<CategoryStats>().fetch_all(pool).await?
    };

    Ok(CatalogueStats {
        total_entries,
        total_size_bytes,
        by_category,
        latest_at,
    })
}

/// Per-net summary statistics.
pub async fn stats_by_net(pool: &PgPool) -> Result<Vec<NetStats>, sqlx::Error> {
    sqlx::query_as::<_, NetStats>(
        "SELECT source_net, COUNT(*)::bigint as total_artifacts, \
         COALESCE(SUM(size_bytes), 0)::bigint as total_bytes, \
         MIN(created_at) as first_at, MAX(created_at) as latest_at \
         FROM catalogue_entries GROUP BY source_net ORDER BY total_artifacts DESC",
    )
    .fetch_all(pool)
    .await
}

/// All artifacts for a given process (campaign lineage).
///
/// Resolves membership via three paths:
/// 1. Explicit `process_id` on the catalogue entry
/// 2. `job_id` prefix match (legacy: `{process_id}:{step}`)
/// 3. Causality resolution: the entry's `signal_key` maps to a
///    `causality_cross_links` egress event whose consumed tokens belong
///    to this process (via `causality_process_tags`)
pub async fn lineage(pool: &PgPool, process_id: &str) -> Result<Vec<CatalogueEntry>, sqlx::Error> {
    let job_prefix = format!("{process_id}:%");
    sqlx::query_as::<_, CatalogueEntry>(&format!(
        r#"
        SELECT {ENTRY_COLUMNS} FROM catalogue_entries
        WHERE process_id = $1
           OR job_id LIKE $2
           OR signal_key IN (
               SELECT cl.signal_key
               FROM causality_cross_links cl
               JOIN causality_event_tokens et
                 ON et.net_id = cl.egress_net
                AND et.event_seq = cl.egress_seq
               JOIN causality_process_tags pt ON pt.token_id = et.token_id
               WHERE pt.process_id = $1
           )
           OR content_hash IN (
               SELECT cp.content_hash FROM catalogue_producers cp
               WHERE cp.process_id = $1
                  OR cp.source_net = (SELECT net_id FROM hpi_processes WHERE process_id = $1)
           )
        ORDER BY created_at ASC
        "#,
    ))
    .bind(process_id)
    .bind(&job_prefix)
    .fetch_all(pool)
    .await
    .map(|rows| rows.into_iter().map(CatalogueEntry::hydrate_view).collect())
}

/// Lineage with optional category / render_hint / time-range / limit filters.
/// Used by the live-artifact backfill endpoint. Empty slices = no filter.
pub async fn lineage_filtered(
    pool: &PgPool,
    process_id: &str,
    categories: &[String],
    render_hints: &[String],
    since: Option<chrono::DateTime<chrono::Utc>>,
    until: Option<chrono::DateTime<chrono::Utc>>,
    limit: i64,
) -> Result<Vec<CatalogueEntry>, sqlx::Error> {
    let job_prefix = format!("{process_id}:%");
    let categories_opt: Option<Vec<String>> = if categories.is_empty() {
        None
    } else {
        Some(categories.to_vec())
    };
    let hints_opt: Option<Vec<String>> = if render_hints.is_empty() {
        None
    } else {
        Some(render_hints.to_vec())
    };
    sqlx::query_as::<_, CatalogueEntry>(&format!(
        r#"
        SELECT {ENTRY_COLUMNS} FROM catalogue_entries
        WHERE (
            process_id = $1
            OR job_id LIKE $2
            OR signal_key IN (
                SELECT cl.signal_key
                FROM causality_cross_links cl
                JOIN causality_event_tokens et
                  ON et.net_id = cl.egress_net
                 AND et.event_seq = cl.egress_seq
                JOIN causality_process_tags pt ON pt.token_id = et.token_id
                WHERE pt.process_id = $1
            )
            OR content_hash IN (
                SELECT cp.content_hash FROM catalogue_producers cp
                WHERE cp.process_id = $1
                   OR cp.source_net = (SELECT net_id FROM hpi_processes WHERE process_id = $1)
            )
        )
        AND ($3::text[] IS NULL OR category = ANY($3))
        AND ($4::text[] IS NULL OR (user_metadata->>'render_hint') = ANY($4))
        AND ($5::timestamptz IS NULL OR created_at >= $5)
        AND ($6::timestamptz IS NULL OR created_at < $6)
        ORDER BY created_at ASC
        LIMIT $7
        "#,
    ))
    .bind(process_id)
    .bind(&job_prefix)
    .bind(categories_opt)
    .bind(hints_opt)
    .bind(since)
    .bind(until)
    .bind(limit.clamp(1, 5000))
    .fetch_all(pool)
    .await
    .map(|rows| rows.into_iter().map(CatalogueEntry::hydrate_view).collect())
}

/// All artifacts for a given process, grouped by step (campaign lineage).
pub async fn lineage_grouped(
    pool: &PgPool,
    process_id: &str,
) -> Result<LineageResponse, sqlx::Error> {
    let entries = lineage(pool, process_id).await?;
    let total_artifacts = entries.len() as i64;

    // Group entries by step extracted from job_id ("{process_id}:{step}") or process_step field.
    let mut step_map: indexmap::IndexMap<String, Vec<CatalogueEntry>> = indexmap::IndexMap::new();

    for entry in entries {
        let step_name = entry
            .process_step
            .clone()
            .or_else(|| {
                entry
                    .job_id
                    .as_ref()
                    .and_then(|jid| jid.split_once(':').map(|(_, s)| s.to_string()))
            })
            .unwrap_or_else(|| "unknown".to_string());
        step_map.entry(step_name).or_default().push(entry);
    }

    let steps = step_map
        .into_iter()
        .map(|(step, artifacts)| {
            // Parse iteration from trailing numeric suffix, e.g. "fit-5" → 5
            let iteration = step
                .rsplit_once('-')
                .and_then(|(_, suffix)| suffix.parse::<i64>().ok());
            LineageStep {
                step,
                iteration,
                artifacts,
            }
        })
        .collect();

    Ok(LineageResponse {
        process_id: process_id.to_string(),
        steps,
        total_artifacts,
    })
}

/// Resolve process_id for a catalogue entry via the causality system.
///
/// Given a `signal_key` (from executor submit or bridge-out), finds the
/// process that triggered the executor job by walking:
///   cross_links → egress event → consumed tokens → process_tags
///
/// Returns `None` if causality data hasn't been ingested yet.
pub async fn resolve_process_id_from_causality(
    pool: &PgPool,
    signal_key: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        r#"
        SELECT pt.process_id
        FROM causality_cross_links cl
        JOIN causality_event_tokens et
          ON et.net_id = cl.egress_net
         AND et.event_seq = cl.egress_seq
         AND et.role = 'consumed'
        JOIN causality_process_tags pt ON pt.token_id = et.token_id
        WHERE cl.signal_key = $1
        LIMIT 1
        "#,
    )
    .bind(signal_key)
    .fetch_optional(pool)
    .await
}

/// Distinct values for a column (for populating filter dropdowns).
pub async fn distinct_values(pool: &PgPool, column: &str) -> Result<Vec<String>, QueryError> {
    // Validate column is allowed
    let field = builder::validate_field(column, ALLOWED_FILTER_FIELDS)?;

    let sql = format!(
        "SELECT DISTINCT {field} FROM catalogue_entries WHERE {field} IS NOT NULL ORDER BY {field}"
    );
    let rows: Vec<(String,)> = sqlx::query_as(&sql).fetch_all(pool).await?;
    Ok(rows.into_iter().map(|(v,)| v).collect())
}

// ── Query-fields registry endpoint ───────────────────────────────────────────

/// `(name, value_type, description)` — the human side of the field registry,
/// keyed by [`CATALOGUE_FIELD_SPECS`] name. Lives NEXT TO the specs so the two
/// tables evolve together (a unit test asserts full coverage). `value_type` is
/// one of `text|number|timestamp|boolean`.
const CATALOGUE_FIELD_INFO: &[(&str, &str, &str)] = &[
    ("id", "text", "Artifact id within its execution"),
    ("execution_id", "text", "Producing execution id"),
    (
        "job_id",
        "text",
        "Executor job id ({process_id}:{step} for job-net artifacts)",
    ),
    ("name", "text", "Artifact display name"),
    (
        "category",
        "text",
        "Artifact category (model, dataset, plot, ...)",
    ),
    ("filename", "text", "Original filename"),
    ("mime_type", "text", "MIME type"),
    ("storage_path", "text", "Object-store storage path"),
    ("source_net", "text", "Producing Petri net id"),
    ("source_place", "text", "Producing place within the net"),
    (
        "signal_key",
        "text",
        "Causality signal key (executor submit / bridge-out)",
    ),
    ("process_id", "text", "Producing HPI process id"),
    ("process_step", "text", "Producing step within the process"),
    ("created_at", "timestamp", "When the artifact was produced"),
    (
        "catalogued_at",
        "timestamp",
        "When the catalogue ingested the artifact",
    ),
    ("size_bytes", "number", "Artifact size in bytes"),
    (
        "content_hash",
        "text",
        "Logical content identity (bare-hex SHA-256)",
    ),
    (
        "meta.format",
        "text",
        "Probed file format (snake_case: csv, parquet, png, ...)",
    ),
    ("meta.num_rows", "number", "Row count (tabular formats)"),
    (
        "meta.num_columns",
        "number",
        "Column count (tabular formats)",
    ),
    (
        "meta.completeness",
        "number",
        "Data-quality completeness score 0..1 (tabular formats)",
    ),
    ("meta.width", "number", "Pixel width (image/video formats)"),
    (
        "meta.height",
        "number",
        "Pixel height (image/video formats)",
    ),
    (
        "meta.duration_secs",
        "number",
        "Duration in seconds (audio/video formats)",
    ),
    ("meta.schema", "text", "Schema fingerprint digest (hex16)"),
    (
        "meta.encrypted",
        "boolean",
        "Whether the probe flagged the file as encrypted",
    ),
];

/// One filterable field, described for the frontend field picker.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct QueryFieldDesc {
    /// Wire name: `filter[<name>][op]=` / `sort=<name>`.
    pub name: String,
    /// `text` | `number` | `timestamp` | `boolean`.
    pub value_type: String,
    /// Whether `sort=<name>` is accepted.
    pub sortable: bool,
    pub description: String,
}

/// One `file_metadata` containment idiom (the `file_metadata=` query param).
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ContainmentTermDesc {
    pub term: String,
    pub description: String,
    /// A literal `file_metadata=` JSON value demonstrating the idiom.
    pub example: String,
}

/// The full query-surface registry served by `GET /api/v1/catalogue/query-fields`.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct QueryFieldsResponse {
    /// Native catalogue columns.
    pub native: Vec<QueryFieldDesc>,
    /// Virtual `meta.*` fields projected from `file_metadata`.
    pub meta: Vec<QueryFieldDesc>,
    /// JSONB containment idioms for the `file_metadata=` param.
    pub containment: Vec<ContainmentTermDesc>,
    /// Valid `group_by` values for `GET /api/v1/catalogue/facets`.
    pub facet_dimensions: Vec<String>,
}

/// Build the registry response FROM [`CATALOGUE_FIELD_SPECS`] (the same table
/// that compiles WHERE/ORDER BY — the field picker cannot drift).
pub fn query_fields_response() -> QueryFieldsResponse {
    let describe = |s: &FieldSpec| {
        let (value_type, description) = CATALOGUE_FIELD_INFO
            .iter()
            .find(|(n, _, _)| *n == s.name)
            .map(|(_, t, d)| (*t, *d))
            .unwrap_or(("text", ""));
        QueryFieldDesc {
            name: s.name.to_string(),
            value_type: value_type.to_string(),
            sortable: ALLOWED_SORT_FIELDS.contains(&s.name),
            description: description.to_string(),
        }
    };

    let (meta, native): (Vec<_>, Vec<_>) = CATALOGUE_FIELD_SPECS
        .iter()
        .map(describe)
        .partition(|d| d.name.starts_with("meta."));

    let term = |term: &str, description: &str, example: &str| ContainmentTermDesc {
        term: term.to_string(),
        description: description.to_string(),
        example: example.to_string(),
    };
    let containment = vec![
        term(
            "col",
            "Entries whose tabular data has a column with this NAME",
            r#"{"column_names":["email"]}"#,
        ),
        term(
            "pii",
            "Entries with a column classified as this category (email, phone, ip_address, uuid, url, iso_date, latitude, longitude, year, age, percentage)",
            r#"{"columns":[{"classifications":[{"category":"email"}]}]}"#,
        ),
        term(
            "dim",
            "Entries with a named dimension (netcdf/hdf5/zarr)",
            r#"{"dimensions":[{"name":"time"}]}"#,
        ),
        term(
            "attr",
            "Entries carrying a custom attribute key=value",
            r#"{"attributes":{"experiment":{"type":"String","value":"run-42"}}}"#,
        ),
        term("format", "Entries of a probed file format", r#"{"format":"csv"}"#),
    ];

    QueryFieldsResponse {
        native,
        meta,
        containment,
        facet_dimensions: super::facets::CatalogueDimension::ALL
            .iter()
            .map(|d| d.as_str().to_string())
            .collect(),
    }
}

/// Distinct values for a JSONB key (e.g., file_metadata.format).
///
/// Only allows `file_metadata` and `user_metadata` columns.
pub async fn distinct_jsonb_values(
    pool: &PgPool,
    column: &str,
    key: &str,
) -> Result<Vec<String>, QueryError> {
    if column != "file_metadata" && column != "user_metadata" {
        return Err(QueryError::InvalidField(
            column.to_string(),
            "file_metadata, user_metadata".to_string(),
        ));
    }
    let sql = format!(
        "SELECT DISTINCT {column}->>$1 AS val FROM catalogue_entries \
         WHERE {column}->>$1 IS NOT NULL ORDER BY val"
    );
    let rows: Vec<(String,)> = sqlx::query_as(&sql).bind(key).fetch_all(pool).await?;
    Ok(rows.into_iter().map(|(v,)| v).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::filter::{Filter, FilterOperator};
    use crate::query::pagination::Sort;

    /// Every sortable name must resolve to a spec (otherwise the gate would
    /// pass a name the spec compiler then rejects).
    #[test]
    fn sort_allowlist_is_subset_of_specs() {
        for name in ALLOWED_SORT_FIELDS {
            assert!(
                CATALOGUE_FIELD_SPECS.iter().any(|s| s.name == *name),
                "sortable field {name} missing from CATALOGUE_FIELD_SPECS"
            );
        }
    }

    /// Native specs keep expr == name; only `_at` columns carry the
    /// timestamp flag; meta.* exprs all project out of file_metadata.
    #[test]
    fn spec_table_invariants() {
        for s in CATALOGUE_FIELD_SPECS {
            if s.name.starts_with("meta.") {
                assert!(
                    s.expr.contains("file_metadata"),
                    "meta spec {} must read file_metadata",
                    s.name
                );
                assert!(!s.timestamp, "no meta.* field is a timestamp");
            } else {
                assert_eq!(s.expr, s.name, "native spec expr == column name");
                assert_eq!(
                    s.timestamp,
                    s.name.ends_with("_at"),
                    "native timestamp flag mirrors the _at convention ({})",
                    s.name
                );
            }
        }
    }

    /// meta.* filters and sorts compile through append_where / the sort gate.
    #[test]
    fn meta_filter_and_sort_compile() {
        let mut params = QueryParams::from_query_str(
            "filter[meta.num_rows][gte]=100&filter[meta.format][eq]=csv&sort=-meta.num_rows",
        )
        .expect("parse");

        let mut qb = QueryBuilder::<Postgres>::new("SELECT 1 FROM catalogue_entries");
        let wrote = append_where(&mut qb, &params).expect("append_where");
        assert!(wrote);
        let sql = qb.sql().to_string();
        assert!(sql.contains("((file_metadata->>'num_rows')::bigint) >= "));
        assert!(sql.contains("(file_metadata->>'format') = "));

        let sort = params.sort.take().expect("sort parsed");
        let mut qb = QueryBuilder::<Postgres>::new("SELECT 1 FROM catalogue_entries");
        builder::build_order_by_specs(&mut qb, &sort, CATALOGUE_FIELD_SPECS).expect("sort");
        assert!(qb
            .sql()
            .ends_with(" ORDER BY ((file_metadata->>'num_rows')::bigint) DESC NULLS LAST"));
    }

    /// Unknown fields are rejected before SQL; non-sortable spec'd fields
    /// (e.g. meta.width) are rejected by the sort gate semantics.
    #[test]
    fn unknown_field_rejected_and_sort_gate_holds() {
        let filter = Filter::single("meta.bogus", FilterOperator::Eq, "x");
        let mut qb = QueryBuilder::<Postgres>::new("SELECT 1");
        let err = builder::build_where_conditions_specs(&mut qb, &filter, CATALOGUE_FIELD_SPECS)
            .unwrap_err();
        assert!(matches!(err, QueryError::InvalidField(..)));

        // meta.width IS a filter spec but NOT in ALLOWED_SORT_FIELDS.
        let sort = Sort::asc("meta.width");
        let normalized = camel_to_snake_case(&sort.field);
        assert!(!ALLOWED_SORT_FIELDS.contains(&normalized.as_str()));
    }

    /// The query-fields endpoint serves every spec exactly once, with the
    /// description table covering all of them.
    #[test]
    fn query_fields_response_covers_every_spec_once() {
        let resp = query_fields_response();
        let all: Vec<&QueryFieldDesc> = resp.native.iter().chain(resp.meta.iter()).collect();
        assert_eq!(all.len(), CATALOGUE_FIELD_SPECS.len());
        for s in CATALOGUE_FIELD_SPECS {
            let matches = all.iter().filter(|d| d.name == s.name).count();
            assert_eq!(matches, 1, "spec {} must appear exactly once", s.name);
            // The human table must cover every spec (no fallback blanks).
            assert!(
                CATALOGUE_FIELD_INFO.iter().any(|(n, _, _)| *n == s.name),
                "CATALOGUE_FIELD_INFO missing {}",
                s.name
            );
        }
        for d in &all {
            assert!(
                ["text", "number", "timestamp", "boolean"].contains(&d.value_type.as_str()),
                "bad value_type {} on {}",
                d.value_type,
                d.name
            );
            assert!(!d.description.is_empty(), "{} needs a description", d.name);
            assert_eq!(
                d.sortable,
                ALLOWED_SORT_FIELDS.contains(&d.name.as_str()),
                "sortable flag mirrors ALLOWED_SORT_FIELDS ({})",
                d.name
            );
        }
        assert!(resp.meta.iter().all(|d| d.name.starts_with("meta.")));
        assert!(resp.native.iter().all(|d| !d.name.starts_with("meta.")));
        // Containment idioms: the five documented terms, examples are valid JSON.
        let terms: Vec<&str> = resp.containment.iter().map(|t| t.term.as_str()).collect();
        assert_eq!(terms, ["col", "pii", "dim", "attr", "format"]);
        for t in &resp.containment {
            serde_json::from_str::<serde_json::Value>(&t.example)
                .unwrap_or_else(|e| panic!("example for {} not JSON: {e}", t.term));
        }
        // Facet dimensions mirror the enum.
        assert_eq!(
            resp.facet_dimensions,
            super::super::facets::CatalogueDimension::ALL
                .iter()
                .map(|d| d.as_str().to_string())
                .collect::<Vec<_>>()
        );
    }
}
