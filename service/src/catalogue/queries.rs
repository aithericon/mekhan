use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::query::builder::{self, QueryError};
use crate::query::extractor::QueryParams;
use crate::query::pagination::Paginated;

use super::model::*;

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
    "correlation_id",
    "process_id",
    "process_step",
    "created_at",
    "catalogued_at",
    "size_bytes",
];

/// Allowed sort fields for catalogue entries (whitelist).
const ALLOWED_SORT_FIELDS: &[&str] = &[
    "name",
    "category",
    "size_bytes",
    "created_at",
    "catalogued_at",
    "source_net",
    "process_id",
    "execution_id",
];

/// List catalogue entries with full filter/sort/pagination support.
pub async fn list_entries(
    pool: &PgPool,
    params: &QueryParams,
) -> Result<Paginated<CatalogueEntry>, QueryError> {
    // -- COUNT query --
    let count = {
        let mut qb = QueryBuilder::<Postgres>::new("SELECT COUNT(*)::bigint FROM catalogue_entries");
        append_where(&mut qb, params, ALLOWED_FILTER_FIELDS)?;
        let row: (i64,) = qb.build_query_as().fetch_one(pool).await?;
        row.0
    };

    // -- SELECT query --
    let entries = {
        let mut qb =
            QueryBuilder::<Postgres>::new("SELECT * FROM catalogue_entries");
        append_where(&mut qb, params, ALLOWED_FILTER_FIELDS)?;

        // ORDER BY
        if let Some(ref sort) = params.sort {
            builder::build_order_by(&mut qb, sort, ALLOWED_SORT_FIELDS)?;
        } else {
            qb.push(" ORDER BY created_at DESC");
        }

        // LIMIT / OFFSET
        builder::build_pagination(&mut qb, &params.page);

        qb.build_query_as::<CatalogueEntry>()
            .fetch_all(pool)
            .await?
    };

    Ok(Paginated::new(entries, count, &params.page))
}

/// Append a WHERE clause combining typed filters, search, and JSONB containment.
fn append_where(
    qb: &mut QueryBuilder<'_, Postgres>,
    params: &QueryParams,
    allowed_fields: &[&str],
) -> Result<(), QueryError> {
    let has_filter = params
        .filter
        .as_ref()
        .map(|f| !f.is_empty())
        .unwrap_or(false);
    let has_search = params.search.is_some();
    let has_metadata = params.metadata.is_some();
    let has_file_metadata = params.file_metadata.is_some();

    if !has_filter && !has_search && !has_metadata && !has_file_metadata {
        return Ok(());
    }

    qb.push(" WHERE ");
    let mut need_and = false;

    // Typed filters
    if let Some(ref filter) = params.filter {
        if !filter.is_empty() {
            builder::build_where_conditions(qb, filter, allowed_fields)?;
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

    Ok(())
}

/// Get a single catalogue entry by composite key.
pub async fn get_entry(
    pool: &PgPool,
    execution_id: &str,
    id: &str,
) -> Result<Option<CatalogueEntry>, sqlx::Error> {
    sqlx::query_as::<_, CatalogueEntry>(
        "SELECT * FROM catalogue_entries WHERE execution_id = $1 AND id = $2",
    )
    .bind(execution_id)
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Aggregate statistics, optionally filtered by the same params.
pub async fn stats(pool: &PgPool, params: &QueryParams) -> Result<CatalogueStats, QueryError> {
    // Total count + size
    let (total_entries, total_size_bytes, latest_at) = {
        let mut qb = QueryBuilder::<Postgres>::new(
            "SELECT COALESCE(COUNT(*), 0)::bigint, COALESCE(SUM(size_bytes), 0)::bigint, MAX(created_at) FROM catalogue_entries",
        );
        append_where(&mut qb, params, ALLOWED_FILTER_FIELDS)?;
        let row: (i64, i64, Option<chrono::DateTime<chrono::Utc>>) =
            qb.build_query_as().fetch_one(pool).await?;
        row
    };

    // Per-category breakdown
    let by_category = {
        let mut qb = QueryBuilder::<Postgres>::new(
            "SELECT category, COUNT(*)::bigint as count, COALESCE(SUM(size_bytes), 0)::bigint as total_bytes FROM catalogue_entries",
        );
        append_where(&mut qb, params, ALLOWED_FILTER_FIELDS)?;
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
pub async fn lineage(
    pool: &PgPool,
    process_id: &str,
) -> Result<Vec<CatalogueEntry>, sqlx::Error> {
    sqlx::query_as::<_, CatalogueEntry>(
        "SELECT * FROM catalogue_entries WHERE process_id = $1 ORDER BY created_at ASC",
    )
    .bind(process_id)
    .fetch_all(pool)
    .await
}

/// Distinct values for a column (for populating filter dropdowns).
pub async fn distinct_values(
    pool: &PgPool,
    column: &str,
) -> Result<Vec<String>, QueryError> {
    // Validate column is allowed
    let field = builder::validate_field(column, ALLOWED_FILTER_FIELDS)?;

    let sql = format!(
        "SELECT DISTINCT {field} FROM catalogue_entries WHERE {field} IS NOT NULL ORDER BY {field}"
    );
    let rows: Vec<(String,)> = sqlx::query_as(&sql).fetch_all(pool).await?;
    Ok(rows.into_iter().map(|(v,)| v).collect())
}
