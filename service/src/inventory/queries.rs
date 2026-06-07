use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::query::builder::{self, QueryError};
use crate::query::extractor::QueryParams;
use crate::query::pagination::Paginated;

use super::model::*;

/// Allowed filter fields for inventory entries (whitelist).
const ALLOWED_FILTER_FIELDS: &[&str] = &[
    "content_hash",
    "file_server_id",
    "path",
    "status",
    "is_canonical",
];

/// Allowed sort fields for inventory entries (whitelist).
const ALLOWED_SORT_FIELDS: &[&str] = &[
    "content_hash",
    "file_server_id",
    "path",
    "status",
    "first_seen",
    "last_seen",
    "updated_at",
];

/// List inventory entries with filter/sort/pagination support.
pub async fn list_entries(
    pool: &PgPool,
    params: &QueryParams,
) -> Result<Paginated<InventoryEntry>, QueryError> {
    // -- COUNT query --
    let count = {
        let mut qb = QueryBuilder::<Postgres>::new("SELECT COUNT(*)::bigint FROM file_inventory");
        append_where(&mut qb, params)?;
        let row: (i64,) = qb.build_query_as().fetch_one(pool).await?;
        row.0
    };

    // -- SELECT query --
    let entries = {
        let mut qb = QueryBuilder::<Postgres>::new("SELECT * FROM file_inventory");
        append_where(&mut qb, params)?;

        if let Some(ref sort) = params.sort {
            builder::build_order_by(&mut qb, sort, ALLOWED_SORT_FIELDS)?;
        } else {
            qb.push(" ORDER BY updated_at DESC");
        }

        builder::build_pagination(&mut qb, &params.page);

        qb.build_query_as::<InventoryEntry>().fetch_all(pool).await?
    };

    Ok(Paginated::new(entries, count, &params.page))
}

/// Append a WHERE clause combining typed filters + free-text search on `path`.
fn append_where(
    qb: &mut QueryBuilder<'_, Postgres>,
    params: &QueryParams,
) -> Result<(), QueryError> {
    let has_filter = params
        .filter
        .as_ref()
        .map(|f| !f.is_empty())
        .unwrap_or(false);
    let has_search = params.search.is_some();

    if !has_filter && !has_search {
        return Ok(());
    }

    qb.push(" WHERE ");
    let mut need_and = false;

    if let Some(ref filter) = params.filter {
        if !filter.is_empty() {
            builder::build_where_conditions(qb, filter, ALLOWED_FILTER_FIELDS)?;
            need_and = true;
        }
    }

    if let Some(ref search) = params.search {
        if need_and {
            qb.push(" AND ");
        }
        let pattern = format!("%{search}%");
        qb.push("(path ILIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" OR content_hash ILIKE ");
        qb.push_bind(pattern);
        qb.push(")");
    }

    Ok(())
}

/// Counts grouped by status and by file_server_id.
pub async fn stats(pool: &PgPool) -> Result<InventoryStats, sqlx::Error> {
    let total: (i64,) = sqlx::query_as("SELECT COUNT(*)::bigint FROM file_inventory")
        .fetch_one(pool)
        .await?;

    let by_status = sqlx::query_as::<_, InventoryCount>(
        "SELECT status AS key, COUNT(*)::bigint AS count \
         FROM file_inventory GROUP BY status ORDER BY count DESC",
    )
    .fetch_all(pool)
    .await?;

    let by_server = sqlx::query_as::<_, InventoryCount>(
        "SELECT file_server_id AS key, COUNT(*)::bigint AS count \
         FROM file_inventory GROUP BY file_server_id ORDER BY count DESC",
    )
    .fetch_all(pool)
    .await?;

    Ok(InventoryStats {
        total: total.0,
        by_status,
        by_server,
    })
}

/// Batched by-reference upsert.
///
/// For each item: if it carries content metadata + a `content_hash`, UPSERT a
/// logical `catalogue_entries` row (`ON CONFLICT (content_hash) DO NOTHING`,
/// `execution_id`/`id` NULL, `category = 'legacy'`); then UPSERT the
/// `file_inventory` row (`ON CONFLICT (file_server_id, path) DO UPDATE` the
/// status / last_seen / updated_at / content_hash). No bytes.
pub async fn register(
    pool: &PgPool,
    req: &InventoryRegisterRequest,
) -> Result<InventoryRegisterResponse, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mut catalogue_inserted: i64 = 0;
    let mut inventory_upserted: i64 = 0;

    for item in &req.entries {
        if let Some(hash) = item.content_hash.as_ref() {
            // Logical catalogue row keyed on content_hash. execution_id/id NULL.
            let r = sqlx::query(
                r#"
                INSERT INTO catalogue_entries
                    (content_hash, category, name, size_bytes, mime_type)
                VALUES ($1, 'legacy', $2, $3, $4)
                ON CONFLICT (content_hash) DO NOTHING
                "#,
            )
            .bind(hash)
            .bind(&item.name)
            .bind(item.size_bytes)
            .bind(&item.mime_type)
            .execute(&mut *tx)
            .await?;
            catalogue_inserted += r.rows_affected() as i64;
        }

        let r = sqlx::query(
            r#"
            INSERT INTO file_inventory
                (content_hash, file_server_id, path, status, provenance, last_seen, updated_at)
            VALUES ($1, $2, $3, $4, $5, NOW(), NOW())
            ON CONFLICT (file_server_id, path) DO UPDATE SET
                status      = EXCLUDED.status,
                content_hash = COALESCE(EXCLUDED.content_hash, file_inventory.content_hash),
                provenance  = EXCLUDED.provenance,
                last_seen   = NOW(),
                updated_at  = NOW()
            "#,
        )
        .bind(&item.content_hash)
        .bind(&item.file_server_id)
        .bind(&item.path)
        .bind(&item.status)
        .bind(&item.provenance)
        .execute(&mut *tx)
        .await?;
        inventory_upserted += r.rows_affected() as i64;
    }

    tx.commit().await?;

    Ok(InventoryRegisterResponse {
        inventory_upserted,
        catalogue_inserted,
    })
}
