//! Postgres queries for the `file_servers` entity + derived inventory rollups.

use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use crate::inventory::model::InventoryCount;
use crate::query::builder::QueryError;

use super::model::*;

/// Per-server count + summed logical size, derived from `file_inventory`.
///
/// Size lives on `catalogue_entries` (inventory rows have no size column), so we
/// LEFT JOIN by `content_hash`; copies of unhashed/uncatalogued files add 0.
#[derive(sqlx::FromRow)]
struct ServerCountSize {
    key: String,
    file_count: i64,
    total_size_bytes: i64,
}

#[derive(sqlx::FromRow)]
struct ServerStatusCount {
    server: String,
    status: String,
    count: i64,
}

/// All-servers rollups, keyed by inventory `file_server_id`.
struct Rollups {
    count_size: HashMap<String, (i64, i64)>,
    by_status: HashMap<String, Vec<InventoryCount>>,
}

async fn load_rollups(pool: &PgPool) -> Result<Rollups, sqlx::Error> {
    let cs = sqlx::query_as::<_, ServerCountSize>(
        "SELECT fi.file_server_id AS key, \
                COUNT(*)::bigint AS file_count, \
                COALESCE(SUM(c.size_bytes), 0)::bigint AS total_size_bytes \
         FROM file_inventory fi \
         LEFT JOIN catalogue_entries c ON c.content_hash = fi.content_hash \
         GROUP BY fi.file_server_id",
    )
    .fetch_all(pool)
    .await?;

    let sc = sqlx::query_as::<_, ServerStatusCount>(
        "SELECT file_server_id AS server, status, COUNT(*)::bigint AS count \
         FROM file_inventory GROUP BY file_server_id, status ORDER BY count DESC",
    )
    .fetch_all(pool)
    .await?;

    let mut count_size = HashMap::new();
    for r in cs {
        count_size.insert(r.key, (r.file_count, r.total_size_bytes));
    }
    let mut by_status: HashMap<String, Vec<InventoryCount>> = HashMap::new();
    for r in sc {
        by_status.entry(r.server).or_default().push(InventoryCount {
            key: r.status,
            count: r.count,
        });
    }
    Ok(Rollups {
        count_size,
        by_status,
    })
}

/// Resource `path`s that exist (non-deleted) in this workspace — used to flag
/// whether a server's `resource_ref` still resolves.
async fn resource_paths(pool: &PgPool, workspace_id: Uuid) -> Result<Vec<String>, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT path FROM resources WHERE workspace_id = $1 AND deleted_at IS NULL",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// List registered servers (with rollups) + unregistered inventory keys.
pub async fn list(pool: &PgPool, workspace_id: Uuid) -> Result<FileServersResponse, QueryError> {
    let registered = sqlx::query_as::<_, FileServer>(
        "SELECT * FROM file_servers WHERE workspace_id = $1 ORDER BY key",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await?;

    let rollups = load_rollups(pool).await?;
    let resources = resource_paths(pool, workspace_id).await?;

    let mut registered_keys = std::collections::HashSet::new();
    let mut servers = Vec::with_capacity(registered.len());
    for s in registered {
        registered_keys.insert(s.key.clone());
        let (file_count, total_size_bytes) =
            rollups.count_size.get(&s.key).copied().unwrap_or((0, 0));
        let by_status = rollups.by_status.get(&s.key).cloned().unwrap_or_default();
        let resource_resolves = s
            .resource_ref
            .as_deref()
            .map(|r| resources.iter().any(|p| p == r))
            .unwrap_or(false);
        servers.push(FileServerView {
            server: s,
            file_count,
            total_size_bytes,
            by_status,
            resource_resolves,
        });
    }

    // Unregistered: inventory keys with rollups but no file_servers row.
    let mut unregistered: Vec<UnregisteredServer> = rollups
        .count_size
        .iter()
        .filter(|(k, _)| !registered_keys.contains(*k))
        .map(|(k, (count, size))| UnregisteredServer {
            key: k.clone(),
            file_count: *count,
            total_size_bytes: *size,
        })
        .collect();
    unregistered.sort_by(|a, b| b.file_count.cmp(&a.file_count));

    Ok(FileServersResponse {
        servers,
        unregistered,
    })
}

/// Fetch one server (with rollups) by key within a workspace.
pub async fn get(
    pool: &PgPool,
    workspace_id: Uuid,
    key: &str,
) -> Result<Option<FileServerView>, QueryError> {
    let server = sqlx::query_as::<_, FileServer>(
        "SELECT * FROM file_servers WHERE workspace_id = $1 AND key = $2",
    )
    .bind(workspace_id)
    .bind(key)
    .fetch_optional(pool)
    .await?;
    let Some(server) = server else {
        return Ok(None);
    };

    let rollups = load_rollups(pool).await?;
    let resources = resource_paths(pool, workspace_id).await?;
    let (file_count, total_size_bytes) = rollups.count_size.get(key).copied().unwrap_or((0, 0));
    let by_status = rollups.by_status.get(key).cloned().unwrap_or_default();
    let resource_resolves = server
        .resource_ref
        .as_deref()
        .map(|r| resources.iter().any(|p| p == r))
        .unwrap_or(false);

    Ok(Some(FileServerView {
        server,
        file_count,
        total_size_bytes,
        by_status,
        resource_resolves,
    }))
}

fn validate_kind(kind: &str) -> Result<(), QueryError> {
    if !ALLOWED_KINDS.contains(&kind) {
        return Err(QueryError::InvalidValue {
            field: "kind".to_string(),
            reason: format!("unknown kind {kind:?} (allowed: {ALLOWED_KINDS:?})"),
        });
    }
    Ok(())
}

/// Whether a `file_server_id` string appears in `file_inventory` (adopt guard).
pub async fn key_in_inventory(pool: &PgPool, key: &str) -> Result<bool, QueryError> {
    let row: (bool,) =
        sqlx::query_as("SELECT EXISTS(SELECT 1 FROM file_inventory WHERE file_server_id = $1)")
            .bind(key)
            .fetch_one(pool)
            .await?;
    Ok(row.0)
}

/// Insert a new file server. Returns `InvalidValue` on a bad kind; a unique
/// `(workspace_id, key)` violation surfaces as a DB error the handler maps to
/// 409 (we pre-check existence in the handler for a clean message).
pub async fn create(
    pool: &PgPool,
    workspace_id: Uuid,
    req: &CreateFileServerRequest,
) -> Result<FileServer, QueryError> {
    validate_kind(&req.kind)?;
    let display_name = req
        .display_name
        .clone()
        .unwrap_or_else(|| req.key.clone());
    let config = req.config.clone().unwrap_or_else(|| serde_json::json!({}));

    let server = sqlx::query_as::<_, FileServer>(
        "INSERT INTO file_servers \
            (workspace_id, key, display_name, kind, resource_ref, base_path, config) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
    )
    .bind(workspace_id)
    .bind(&req.key)
    .bind(&display_name)
    .bind(&req.kind)
    .bind(req.resource_ref.as_deref())
    .bind(req.base_path.as_deref())
    .bind(&config)
    .fetch_one(pool)
    .await?;
    Ok(server)
}

/// Whether a server already exists at `(workspace_id, key)`.
pub async fn exists(pool: &PgPool, workspace_id: Uuid, key: &str) -> Result<bool, QueryError> {
    let row: (bool,) = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM file_servers WHERE workspace_id = $1 AND key = $2)",
    )
    .bind(workspace_id)
    .bind(key)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// Update mutable fields of a server. Returns `None` if no such server.
pub async fn update(
    pool: &PgPool,
    workspace_id: Uuid,
    key: &str,
    req: &UpdateFileServerRequest,
) -> Result<Option<FileServer>, QueryError> {
    if let Some(kind) = req.kind.as_deref() {
        validate_kind(kind)?;
    }
    // COALESCE-style update: each column keeps its value unless the request
    // supplies a new one. `resource_ref`/`base_path` are double-option so they
    // can be explicitly cleared (Some(None)) vs left alone (None).
    let server = sqlx::query_as::<_, FileServer>(
        "UPDATE file_servers SET \
            display_name = COALESCE($3, display_name), \
            kind         = COALESCE($4, kind), \
            resource_ref = CASE WHEN $5 THEN $6 ELSE resource_ref END, \
            base_path    = CASE WHEN $7 THEN $8 ELSE base_path END, \
            status       = COALESCE($9, status), \
            config       = COALESCE($10, config), \
            updated_at   = NOW() \
         WHERE workspace_id = $1 AND key = $2 RETURNING *",
    )
    .bind(workspace_id)
    .bind(key)
    .bind(req.display_name.as_deref())
    .bind(req.kind.as_deref())
    .bind(req.resource_ref.is_some())
    .bind(req.resource_ref.clone().flatten())
    .bind(req.base_path.is_some())
    .bind(req.base_path.clone().flatten())
    .bind(req.status.as_deref())
    .bind(req.config.as_ref())
    .fetch_optional(pool)
    .await?;
    Ok(server)
}

/// Delete a server. Returns whether a row was removed. Inventory rows are
/// untouched (soft join) — they simply revert to "unregistered".
pub async fn delete(pool: &PgPool, workspace_id: Uuid, key: &str) -> Result<bool, QueryError> {
    let r = sqlx::query("DELETE FROM file_servers WHERE workspace_id = $1 AND key = $2")
        .bind(workspace_id)
        .bind(key)
        .execute(pool)
        .await?;
    Ok(r.rows_affected() > 0)
}

/// Idempotently seed the built-in platform object store as a `file_servers` row
/// (called at startup). `key` is the platform S3 bucket; no `resource_ref` (it
/// uses platform config). ON CONFLICT keeps any operator edits.
pub async fn seed_builtin_object_store(
    pool: &PgPool,
    workspace_id: Uuid,
    bucket: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO file_servers \
            (workspace_id, key, display_name, kind, status) \
         VALUES ($1, $2, $3, 'object_store', 'online') \
         ON CONFLICT (workspace_id, key) DO NOTHING",
    )
    .bind(workspace_id)
    .bind(bucket)
    .bind("Platform object store")
    .execute(pool)
    .await?;
    Ok(())
}
