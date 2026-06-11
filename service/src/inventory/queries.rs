use sqlx::{PgPool, Postgres, QueryBuilder, Transaction};

use crate::query::builder::{self, QueryError};
use crate::query::extractor::QueryParams;
use crate::query::pagination::Paginated;

use super::model::*;

/// Allowed filter fields for inventory entries (whitelist). Shared with the
/// analytics breakdown endpoint, which scopes its aggregations with the same
/// filter DSL.
pub(crate) const ALLOWED_FILTER_FIELDS: &[&str] = &[
    "content_hash",
    "file_server_id",
    "path",
    "status",
    "is_canonical",
    "size_bytes",
    "mtime",
    "uid",
    "gid",
    "extension",
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
    "size_bytes",
    "mtime",
    "uid",
    "gid",
    "extension",
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

// ---------------------------------------------------------------------------
// The coupling primitive (docs/32) — "register fills both, never half".
//
// `catalogue_entries` (logical content, keyed on `content_hash`) and
// `file_inventory` (physical copies, keyed on `(file_server_id, path)`) are two
// halves of one equation: a *logical identity* and *where that content
// physically lives*. Registering an artifact must write BOTH, atomically. The
// two helpers below are the only sanctioned writers of that pair; both the HTTP
// register path and the causality projector go through them. The catalogue
// helper takes a NON-OPTIONAL `&str` hash — it is structurally impossible to
// create a logical row without a content identity. Hashless *observation* of a
// physical file (we saw it on disk but haven't hashed it) is the separate
// [`index`] path, which writes inventory only.
// ---------------------------------------------------------------------------

/// Upsert the logical `catalogue_entries` row for `content_hash` (caller owns
/// the tx). `execution_id`/`id` stay NULL — this is a by-reference logical row.
/// Returns rows newly inserted (`ON CONFLICT (content_hash) DO NOTHING`).
pub async fn upsert_catalogue_by_hash(
    tx: &mut Transaction<'_, Postgres>,
    content_hash: &str,
    category: &str,
    name: Option<&str>,
    size_bytes: Option<i64>,
    mime_type: Option<&str>,
) -> Result<u64, sqlx::Error> {
    let r = sqlx::query(
        r#"
        INSERT INTO catalogue_entries
            (content_hash, category, name, size_bytes, mime_type)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (content_hash) DO NOTHING
        "#,
    )
    .bind(content_hash)
    .bind(category)
    .bind(name)
    .bind(size_bytes)
    .bind(mime_type)
    .execute(&mut **tx)
    .await?;
    Ok(r.rows_affected())
}

/// Upsert one physical-copy `file_inventory` row on `(file_server_id, path)`
/// (caller owns the tx). `is_canonical` is set only on INSERT — re-observing an
/// existing copy never clobbers a reconcile-assigned canonical flag. The
/// promoted analytics columns are written from `facts` with a COALESCE
/// non-clobber rule (a fact-less re-observation never NULLs what a previous
/// stat-capable observer recorded); `extension` is GENERATED from `path` and
/// is deliberately never named here. Returns rows inserted-or-updated.
pub async fn upsert_inventory_copy(
    tx: &mut Transaction<'_, Postgres>,
    content_hash: Option<&str>,
    file_server_id: &str,
    path: &str,
    status: &str,
    is_canonical: bool,
    provenance: &serde_json::Value,
    facts: &ObservedFacts,
) -> Result<u64, sqlx::Error> {
    let r = sqlx::query(
        r#"
        INSERT INTO file_inventory
            (content_hash, file_server_id, path, status, is_canonical, provenance,
             size_bytes, mtime, uid, gid, last_seen, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW(), NOW())
        ON CONFLICT (file_server_id, path) DO UPDATE SET
            status       = EXCLUDED.status,
            content_hash = COALESCE(EXCLUDED.content_hash, file_inventory.content_hash),
            provenance   = EXCLUDED.provenance,
            size_bytes   = COALESCE(EXCLUDED.size_bytes, file_inventory.size_bytes),
            mtime        = COALESCE(EXCLUDED.mtime, file_inventory.mtime),
            uid          = COALESCE(EXCLUDED.uid, file_inventory.uid),
            gid          = COALESCE(EXCLUDED.gid, file_inventory.gid),
            last_seen    = NOW(),
            updated_at   = NOW()
        "#,
    )
    .bind(content_hash)
    .bind(file_server_id)
    .bind(path)
    .bind(status)
    .bind(is_canonical)
    .bind(provenance)
    .bind(facts.size_bytes)
    .bind(facts.mtime)
    .bind(facts.uid)
    .bind(facts.gid)
    .execute(&mut **tx)
    .await?;
    Ok(r.rows_affected())
}

/// Batched by-reference **register** — fills both halves of the equation.
///
/// Every item MUST carry a `content_hash`; an item without one is rejected
/// (`QueryError::InvalidValue`) and the whole batch rolls back, so you can never
/// half-register. For each item, in one transaction: upsert the logical
/// `catalogue_entries` row (keyed on hash, `category = 'legacy'`) AND the
/// physical `file_inventory` row (keyed on `(file_server_id, path)`). No bytes
/// move — this is the online crawl/reconcile path after a `probe` has supplied
/// the hash. Hashless observation goes through [`index`].
pub async fn register(
    pool: &PgPool,
    req: &InventoryRegisterRequest,
) -> Result<InventoryRegisterResponse, QueryError> {
    // Validate the invariant up front so a bad item rejects the batch cleanly
    // (before any write) rather than mid-transaction.
    for item in &req.entries {
        let has_hash = item
            .content_hash
            .as_deref()
            .map(|h| !h.trim().is_empty())
            .unwrap_or(false);
        if !has_hash {
            return Err(QueryError::InvalidValue {
                field: "content_hash".to_string(),
                reason: format!(
                    "register requires a content_hash for every item (missing for path {:?} on {}); \
                     use POST /api/v1/inventory/index to record a hashless observation",
                    item.path, item.file_server_id
                ),
            });
        }
    }

    let mut tx = pool.begin().await?;
    let mut catalogue_inserted: i64 = 0;
    let mut inventory_upserted: i64 = 0;

    for item in &req.entries {
        let hash = item.content_hash.as_deref().expect("validated above");
        catalogue_inserted += upsert_catalogue_by_hash(
            &mut tx,
            hash,
            "legacy",
            item.name.as_deref(),
            item.size_bytes,
            item.mime_type.as_deref(),
        )
        .await? as i64;
        inventory_upserted += upsert_inventory_copy(
            &mut tx,
            Some(hash),
            &item.file_server_id,
            &item.path,
            &item.status,
            false,
            &item.provenance,
            &item.facts(),
        )
        .await? as i64;
    }

    tx.commit().await?;

    Ok(InventoryRegisterResponse {
        inventory_upserted,
        catalogue_inserted,
    })
}

/// Batched hashless **index** — the explicit "observe a physical file" path.
///
/// Writes `file_inventory` rows ONLY (status defaults to `indexed`); it never
/// touches `catalogue_entries`, because an indexed file has a location but no
/// claimed content identity yet. This is where `crawl` output lands before a
/// `probe` hashes the bytes. Once hashed, the file is promoted via [`register`],
/// which couples it to a logical catalogue row.
pub async fn index(
    pool: &PgPool,
    req: &InventoryIndexRequest,
) -> Result<InventoryIndexResponse, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mut inventory_upserted: i64 = 0;

    for item in &req.items {
        inventory_upserted += upsert_inventory_copy(
            &mut tx,
            None,
            &req.file_server_id,
            &item.path,
            &item.status,
            false,
            &item.provenance,
            &item.facts(),
        )
        .await? as i64;
    }

    tx.commit().await?;

    Ok(InventoryIndexResponse { inventory_upserted })
}
