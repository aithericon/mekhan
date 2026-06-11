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
     file_metadata, user_metadata, created_at, catalogued_at, created_by";

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
        applies_to: &[],
    }
}

const fn tspec(name: &'static str, expr: &'static str) -> FieldSpec {
    FieldSpec {
        name,
        expr,
        timestamp: true,
        applies_to: &[],
    }
}

/// Format-scoped spec: `applies_to` is discovery metadata for field pickers
/// (see [`FieldSpec::applies_to`]) — it never reaches the SQL.
const fn fspec(
    name: &'static str,
    expr: &'static str,
    applies_to: &'static [&'static str],
) -> FieldSpec {
    FieldSpec {
        name,
        expr,
        timestamp: false,
        applies_to,
    }
}

// Format groups for `applies_to` (snake_case fmeta `FileFormat` wire strings —
// the values of `file_metadata->>'format'`; machine-validated against the real
// enum by `applies_to_strings_are_known_formats`). Combined groups are spelled
// out because const slices can't concatenate.
const TABULAR_SPREADSHEET_FORMATS: &[&str] =
    &["csv", "parquet", "json", "arrow", "xlsx", "xls", "ods"];
const SPREADSHEET_FORMATS: &[&str] = &["xlsx", "xls", "ods"];
const IMAGE_FORMATS: &[&str] = &["jpeg", "png", "tiff", "web_p", "gif", "bmp"];
const AUDIO_FORMATS: &[&str] = &["mp3", "flac", "wav", "ogg", "aac"];
const VIDEO_FORMATS: &[&str] = &["mp4", "mkv", "avi", "web_m"];
const ARCHIVE_FORMATS: &[&str] = &["zip", "tar", "seven_zip", "rar"];
const IMAGE_VIDEO_FORMATS: &[&str] = &[
    "jpeg", "png", "tiff", "web_p", "gif", "bmp", "mp4", "mkv", "avi", "web_m",
];
const IMAGE_AUDIO_FORMATS: &[&str] = &[
    "jpeg", "png", "tiff", "web_p", "gif", "bmp", "mp3", "flac", "wav", "ogg", "aac",
];
const AUDIO_VIDEO_FORMATS: &[&str] = &[
    "mp3", "flac", "wav", "ogg", "aac", "mp4", "mkv", "avi", "web_m",
];
const COMPRESSION_FORMATS: &[&str] = &[
    "parquet",
    "jpeg",
    "png",
    "tiff",
    "web_p",
    "gif",
    "bmp",
    "zip",
    "tar",
    "seven_zip",
    "rar",
];
const VTK_FORMATS: &[&str] = &["vtk_legacy", "vtu", "vtp", "vts", "vtr", "vti"];
const ZARR_FORMATS: &[&str] = &["zarr_v2", "zarr_v3"];

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
    fspec(
        "meta.num_rows",
        "((file_metadata->>'num_rows')::bigint)",
        TABULAR_SPREADSHEET_FORMATS,
    ),
    fspec(
        "meta.num_columns",
        "((file_metadata->>'num_columns')::bigint)",
        TABULAR_SPREADSHEET_FORMATS,
    ),
    fspec(
        "meta.completeness",
        "((file_metadata->'data_quality'->>'completeness')::float8)",
        TABULAR_SPREADSHEET_FORMATS,
    ),
    fspec(
        "meta.width",
        "((file_metadata->'format_specific'->'details'->>'width')::bigint)",
        IMAGE_VIDEO_FORMATS,
    ),
    fspec(
        "meta.height",
        "((file_metadata->'format_specific'->'details'->>'height')::bigint)",
        IMAGE_VIDEO_FORMATS,
    ),
    fspec(
        "meta.duration_secs",
        "((file_metadata->'format_specific'->'details'->>'duration_secs')::float8)",
        AUDIO_VIDEO_FORMATS,
    ),
    spec(
        "meta.schema",
        "(file_metadata->'schema_fingerprint'->>'digest')",
    ),
    spec("meta.encrypted", "((file_metadata->>'encrypted')::boolean)"),
    // Per-format detail fields (flat `meta.<leaf>` naming — shared JSONB
    // leaves like `compression` get ONE spec spanning every format that
    // carries them; `applies_to` carries the per-format story).
    fspec(
        "meta.delimiter",
        "(file_metadata->'format_specific'->'details'->>'delimiter')",
        &["csv"],
    ),
    fspec(
        "meta.encoding",
        "(file_metadata->'format_specific'->'details'->>'encoding')",
        &["csv"],
    ),
    fspec(
        "meta.has_header",
        "((file_metadata->'format_specific'->'details'->>'has_header')::boolean)",
        &["csv"],
    ),
    fspec(
        "meta.compression",
        "(file_metadata->'format_specific'->'details'->>'compression')",
        COMPRESSION_FORMATS,
    ),
    fspec(
        "meta.num_row_groups",
        "((file_metadata->'format_specific'->'details'->>'num_row_groups')::bigint)",
        &["parquet"],
    ),
    fspec(
        "meta.color_space",
        "(file_metadata->'format_specific'->'details'->>'color_space')",
        IMAGE_FORMATS,
    ),
    fspec(
        "meta.bit_depth",
        "((file_metadata->'format_specific'->'details'->>'bit_depth')::bigint)",
        IMAGE_AUDIO_FORMATS,
    ),
    fspec(
        "meta.channels",
        "((file_metadata->'format_specific'->'details'->>'channels')::bigint)",
        IMAGE_AUDIO_FORMATS,
    ),
    fspec(
        "meta.animated",
        "((file_metadata->'format_specific'->'details'->>'animated')::boolean)",
        &["png", "gif", "web_p"],
    ),
    fspec(
        "meta.sample_rate",
        "((file_metadata->'format_specific'->'details'->>'sample_rate')::bigint)",
        AUDIO_FORMATS,
    ),
    fspec(
        "meta.codec",
        "(file_metadata->'format_specific'->'details'->>'codec')",
        AUDIO_FORMATS,
    ),
    fspec(
        "meta.bitrate_kbps",
        "((file_metadata->'format_specific'->'details'->>'bitrate_kbps')::bigint)",
        AUDIO_VIDEO_FORMATS,
    ),
    fspec(
        "meta.fps",
        "((file_metadata->'format_specific'->'details'->>'fps')::float8)",
        VIDEO_FORMATS,
    ),
    fspec(
        "meta.video_codec",
        "(file_metadata->'format_specific'->'details'->>'video_codec')",
        VIDEO_FORMATS,
    ),
    fspec(
        "meta.audio_codec",
        "(file_metadata->'format_specific'->'details'->>'audio_codec')",
        VIDEO_FORMATS,
    ),
    fspec(
        "meta.num_entries",
        "((file_metadata->'format_specific'->'details'->>'num_entries')::bigint)",
        ARCHIVE_FORMATS,
    ),
    fspec(
        "meta.num_sheets",
        "((file_metadata->'format_specific'->'details'->>'num_sheets')::bigint)",
        SPREADSHEET_FORMATS,
    ),
    fspec(
        "meta.zarr_version",
        "((file_metadata->'format_specific'->'details'->>'zarr_version')::bigint)",
        ZARR_FORMATS,
    ),
    fspec(
        "meta.num_arrays",
        "((file_metadata->'format_specific'->'details'->>'num_arrays')::bigint)",
        ZARR_FORMATS,
    ),
    fspec(
        "meta.dataset_type",
        "(file_metadata->'format_specific'->'details'->>'dataset_type')",
        VTK_FORMATS,
    ),
    fspec(
        "meta.num_points",
        "((file_metadata->'format_specific'->'details'->>'num_points')::bigint)",
        VTK_FORMATS,
    ),
    fspec(
        "meta.num_cells",
        "((file_metadata->'format_specific'->'details'->>'num_cells')::bigint)",
        VTK_FORMATS,
    ),
    fspec(
        "meta.conventions",
        "(file_metadata->'format_specific'->'details'->>'conventions')",
        &["net_cdf"],
    ),
    fspec(
        "meta.num_hdus",
        "((file_metadata->'format_specific'->'details'->>'num_hdus')::bigint)",
        &["fits"],
    ),
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
    (
        "meta.delimiter",
        "text",
        "CSV field delimiter (e.g. ',' or '\\t')",
    ),
    ("meta.encoding", "text", "CSV text encoding (e.g. utf-8)"),
    (
        "meta.has_header",
        "boolean",
        "Whether the CSV first row is a header",
    ),
    (
        "meta.compression",
        "text",
        "Compression codec/method (parquet, image, archive formats)",
    ),
    ("meta.num_row_groups", "number", "Parquet row-group count"),
    (
        "meta.color_space",
        "text",
        "Image color space (RGB, RGBA, Grayscale, CMYK, ...)",
    ),
    (
        "meta.bit_depth",
        "number",
        "Bits per channel/sample (image/audio formats)",
    ),
    (
        "meta.channels",
        "number",
        "Color or audio channel count (image/audio formats)",
    ),
    ("meta.animated", "boolean", "Whether the image is animated"),
    ("meta.sample_rate", "number", "Audio sample rate in Hz"),
    (
        "meta.codec",
        "text",
        "Audio codec (mp3, flac, aac, vorbis, ...)",
    ),
    (
        "meta.bitrate_kbps",
        "number",
        "Bitrate in kbps (audio/video formats)",
    ),
    ("meta.fps", "number", "Video frames per second"),
    (
        "meta.video_codec",
        "text",
        "Video codec (h264, h265, vp9, av1, ...)",
    ),
    (
        "meta.audio_codec",
        "text",
        "Codec of the primary audio track (video formats)",
    ),
    (
        "meta.num_entries",
        "number",
        "Archive entry count (files + directories)",
    ),
    (
        "meta.num_sheets",
        "number",
        "Spreadsheet workbook sheet count",
    ),
    (
        "meta.zarr_version",
        "number",
        "Zarr format version (2 or 3)",
    ),
    (
        "meta.num_arrays",
        "number",
        "Array count in the Zarr hierarchy",
    ),
    (
        "meta.dataset_type",
        "text",
        "VTK dataset type (UnstructuredGrid, PolyData, ...)",
    ),
    ("meta.num_points", "number", "VTK mesh point count"),
    ("meta.num_cells", "number", "VTK mesh cell count"),
    (
        "meta.conventions",
        "text",
        "NetCDF CF conventions string (e.g. CF-1.8)",
    ),
    ("meta.num_hdus", "number", "FITS Header Data Unit count"),
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
    /// Probed file formats (snake_case `meta.format` values) this field is
    /// meaningful for; empty = universal. Discovery metadata only — the
    /// server accepts the filter regardless.
    pub applies_to: Vec<String>,
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
            applies_to: s.applies_to.iter().map(|f| f.to_string()).collect(),
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
    use aithericon_file_metadata::format::{
        ArchiveMetadata, AudioMetadata, CsvMetadata, FileFormat, FitsMetadata, FormatMetadata,
        ImageMetadata, NetCdfMetadata, ParquetMetadata, SpreadsheetMetadata, VideoMetadata,
        VtkMetadata, ZarrMetadata,
    };

    /// Parse the `details` leaf key a detail spec projects, from the expr
    /// itself (split on the final `->>'…'`) — no parallel table to drift.
    fn details_leaf(expr: &str) -> Option<&str> {
        let (_, rest) = expr.split_once("->'details'->>'")?;
        rest.split_once('\'').map(|(leaf, _)| leaf)
    }

    /// Fully-populated samples of every `FormatMetadata` variant a detail
    /// spec applies to, built from the REAL fmeta types — if a field is
    /// renamed/retyped upstream, these tests catch the drift at compile or
    /// serialize time.
    fn format_metadata_samples() -> Vec<FormatMetadata> {
        vec![
            FormatMetadata::Csv(CsvMetadata {
                delimiter: ';',
                quote_char: Some('"'),
                has_header: true,
                encoding: "utf-8".into(),
                comment_lines: 1,
            }),
            FormatMetadata::Parquet(ParquetMetadata {
                num_row_groups: 4,
                num_rows: 1_000,
                compression: "SNAPPY".into(),
                created_by: Some("parquet-rs".into()),
                version: 2,
                row_groups: vec![],
            }),
            FormatMetadata::Spreadsheet(SpreadsheetMetadata {
                num_sheets: 2,
                sheets: vec![],
            }),
            FormatMetadata::NetCdf(NetCdfMetadata {
                conventions: Some("CF-1.8".into()),
                unlimited_dimensions: vec![],
                variables: vec!["time".into()],
            }),
            FormatMetadata::Fits(FitsMetadata {
                num_hdus: 3,
                primary_bitpix: Some(16),
                header_cards: vec![],
            }),
            FormatMetadata::Zarr(ZarrMetadata {
                zarr_version: 2,
                num_arrays: 3,
                num_groups: 1,
                hierarchy: vec![],
            }),
            FormatMetadata::Vtk(VtkMetadata {
                version: Some("4.2".into()),
                title: Some("mesh".into()),
                dataset_type: "PolyData".into(),
                num_points: Some(100),
                num_cells: Some(50),
                point_data: vec![],
                cell_data: vec![],
            }),
            FormatMetadata::Image(ImageMetadata {
                width: 1920,
                height: 1080,
                color_space: Some("RGB".into()),
                bit_depth: Some(8),
                channels: Some(3),
                animated: true,
                frame_count: Some(10),
                dpi: Some(72.0),
                compression: Some("lossless".into()),
            }),
            FormatMetadata::Audio(AudioMetadata {
                duration_secs: Some(245.5),
                sample_rate: Some(44_100),
                channels: Some(2),
                bit_depth: Some(16),
                bitrate_kbps: Some(320),
                codec: Some("mp3".into()),
            }),
            FormatMetadata::Video(VideoMetadata {
                width: Some(3840),
                height: Some(2160),
                duration_secs: Some(7200.0),
                fps: Some(23.976),
                video_codec: Some("h265".into()),
                audio_codec: Some("aac".into()),
                bitrate_kbps: Some(15_000),
                audio_tracks: Some(2),
                subtitle_tracks: Some(1),
            }),
            FormatMetadata::Archive(ArchiveMetadata {
                num_entries: Some(42),
                total_uncompressed_size: Some(1024),
                total_compressed_size: Some(512),
                compression: Some("deflate".into()),
                encrypted: true,
                comment: Some("c".into()),
                entries: vec![],
            }),
        ]
    }

    /// Anti-drift: every detail-projecting spec's leaf key exists in at least
    /// one serialized REAL `FormatMetadata` sample, with the JSON kind the
    /// registry declares as `value_type`. Catches fmeta field renames/retypes
    /// without a parallel leaf table.
    #[test]
    fn detail_spec_leaves_exist_in_real_fmeta_with_declared_kind() {
        let samples: Vec<serde_json::Value> = format_metadata_samples()
            .iter()
            .map(|m| serde_json::to_value(m).expect("serialize sample"))
            .collect();

        for s in CATALOGUE_FIELD_SPECS {
            let Some(leaf) = details_leaf(s.expr) else {
                continue;
            };
            let (_, value_type, _) = CATALOGUE_FIELD_INFO
                .iter()
                .find(|(n, _, _)| *n == s.name)
                .unwrap_or_else(|| panic!("CATALOGUE_FIELD_INFO missing {}", s.name));

            let mut covered = false;
            for sample in &samples {
                let Some(leaf_val) = sample["details"].get(leaf) else {
                    continue;
                };
                covered = true;
                let kind_ok = match *value_type {
                    "text" => leaf_val.is_string(),
                    "number" => leaf_val.is_number(),
                    "boolean" => leaf_val.is_boolean(),
                    other => panic!("unexpected value_type {other} on {}", s.name),
                };
                assert!(
                    kind_ok,
                    "{}: leaf '{leaf}' serializes as {leaf_val} but registry declares {value_type} \
                     (sample tag {})",
                    s.name, sample["format"]
                );
            }
            assert!(
                covered,
                "{}: leaf '{leaf}' not found in any FormatMetadata sample — \
                 fmeta drift or missing sample",
                s.name
            );
        }

        // Coverage guard: every detail spec carries a non-empty applies_to
        // (a per-format leaf with no format annotation is a registry bug).
        for s in CATALOGUE_FIELD_SPECS {
            if details_leaf(s.expr).is_some() {
                assert!(
                    !s.applies_to.is_empty(),
                    "detail spec {} must declare applies_to",
                    s.name
                );
            }
        }
    }

    /// Every `applies_to` string must deserialize to a KNOWN snake_case
    /// `FileFormat` (machine-validates the web_p / web_m / seven_zip /
    /// net_cdf / zarr_v2 casing — `Unknown(_)` and typos both fail).
    #[test]
    fn applies_to_strings_are_known_formats() {
        for s in CATALOGUE_FIELD_SPECS {
            for fmt in s.applies_to {
                let parsed: FileFormat = serde_json::from_value(serde_json::json!(fmt))
                    .unwrap_or_else(|e| {
                        panic!("applies_to '{fmt}' on {} is not a FileFormat: {e}", s.name)
                    });
                assert!(
                    !matches!(parsed, FileFormat::Unknown(_)),
                    "applies_to '{fmt}' on {} parsed as Unknown",
                    s.name
                );
            }
        }
    }

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

    /// The client-side `datatype:` sugar compiles to
    /// `filter[meta.schema][in]=d1,d2` — the In op must emit `= ANY(<bind>)`
    /// over the digest expr (hex16 digests are comma-free, so the
    /// comma-separated wire form is lossless).
    #[test]
    fn meta_schema_in_filter_compiles_to_any_over_digest_expr() {
        let params = QueryParams::from_query_str(
            "filter[meta.schema][in]=00000000aaaaaaaa,11111111bbbbbbbb",
        )
        .expect("parse");
        let mut qb = QueryBuilder::<Postgres>::new("SELECT 1 FROM catalogue_entries");
        append_where(&mut qb, &params).expect("append_where");
        let sql = qb.sql().to_string();
        assert!(
            sql.contains("(file_metadata->'schema_fingerprint'->>'digest') = ANY("),
            "in-op over meta.schema must be = ANY over the digest expr: {sql}"
        );
        assert!(
            !sql.contains("aaaaaaaa"),
            "digests must be bound, not inlined: {sql}"
        );
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
            // applies_to is copied verbatim from the spec table.
            let desc = all.iter().find(|d| d.name == s.name).unwrap();
            assert_eq!(
                desc.applies_to,
                s.applies_to
                    .iter()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>(),
                "applies_to mirrors the spec table ({})",
                s.name
            );
        }
        // Native columns are format-agnostic by construction.
        assert!(
            resp.native.iter().all(|d| d.applies_to.is_empty()),
            "native fields must not be format-scoped"
        );
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
