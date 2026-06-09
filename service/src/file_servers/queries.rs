//! Postgres queries for the `file_servers` entity (identity-only parent), its
//! `file_server_endpoints` children, and derived inventory rollups.

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
/// whether an endpoint's `resource_ref` still resolves.
async fn resource_paths(pool: &PgPool, workspace_id: Uuid) -> Result<Vec<String>, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT path FROM resources WHERE workspace_id = $1 AND deleted_at IS NULL",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// Load all endpoints for a set of server ids, grouped by `file_server_id`.
async fn endpoints_for(
    pool: &PgPool,
    server_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<FileServerEndpoint>>, sqlx::Error> {
    let mut by_server: HashMap<Uuid, Vec<FileServerEndpoint>> = HashMap::new();
    if server_ids.is_empty() {
        return Ok(by_server);
    }
    let rows = sqlx::query_as::<_, FileServerEndpoint>(
        "SELECT * FROM file_server_endpoints \
         WHERE file_server_id = ANY($1) \
         ORDER BY priority DESC, access_method, root",
    )
    .bind(server_ids)
    .fetch_all(pool)
    .await?;
    for e in rows {
        by_server.entry(e.file_server_id).or_default().push(e);
    }
    Ok(by_server)
}

/// Whether every endpoint's `resource_ref` resolves (NULL refs count as
/// resolved — object_store needs none).
fn endpoints_resolve(endpoints: &[FileServerEndpoint], resources: &[String]) -> bool {
    endpoints.iter().all(|e| match e.resource_ref.as_deref() {
        None => true,
        Some(r) => resources.iter().any(|p| p == r),
    })
}

/// List registered servers (with endpoints + rollups) + unregistered inventory keys.
pub async fn list(pool: &PgPool, workspace_id: Uuid) -> Result<FileServersResponse, QueryError> {
    let registered = sqlx::query_as::<_, FileServer>(
        "SELECT * FROM file_servers WHERE workspace_id = $1 ORDER BY key",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await?;

    let rollups = load_rollups(pool).await?;
    let resources = resource_paths(pool, workspace_id).await?;
    let server_ids: Vec<Uuid> = registered.iter().map(|s| s.id).collect();
    let mut endpoints_by_server = endpoints_for(pool, &server_ids).await?;

    let mut registered_keys = std::collections::HashSet::new();
    let mut servers = Vec::with_capacity(registered.len());
    for s in registered {
        registered_keys.insert(s.key.clone());
        let (file_count, total_size_bytes) =
            rollups.count_size.get(&s.key).copied().unwrap_or((0, 0));
        let by_status = rollups.by_status.get(&s.key).cloned().unwrap_or_default();
        let endpoints = endpoints_by_server.remove(&s.id).unwrap_or_default();
        let resource_resolves = endpoints_resolve(&endpoints, &resources);
        servers.push(FileServerView {
            server: s,
            endpoints,
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
    unregistered.sort_by_key(|u| std::cmp::Reverse(u.file_count));

    Ok(FileServersResponse {
        servers,
        unregistered,
    })
}

/// Fetch one server (with endpoints + rollups) by key within a workspace.
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
    let endpoints = endpoints_for(pool, &[server.id])
        .await?
        .remove(&server.id)
        .unwrap_or_default();
    let (file_count, total_size_bytes) = rollups.count_size.get(key).copied().unwrap_or((0, 0));
    let by_status = rollups.by_status.get(key).cloned().unwrap_or_default();
    let resource_resolves = endpoints_resolve(&endpoints, &resources);

    Ok(Some(FileServerView {
        server,
        endpoints,
        file_count,
        total_size_bytes,
        by_status,
        resource_resolves,
    }))
}

fn validate_access_method(method: &str) -> Result<(), QueryError> {
    if !ALLOWED_ACCESS_METHODS.contains(&method) {
        return Err(QueryError::InvalidValue {
            field: "access_method".to_string(),
            reason: format!(
                "unknown access_method {method:?} (allowed: {ALLOWED_ACCESS_METHODS:?})"
            ),
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

/// The canonical `endpoint_root` a `crawl` stamped onto this key's inventory
/// rows, if any. Picked as the most-common non-empty `provenance->>endpoint_root`
/// across all copies on the server — so `adopt` can promote it onto the created
/// endpoint's `root`. Returns `None` when no copy recorded one (legacy / upload
/// artifacts), in which case `adopt` falls back to an empty root.
pub async fn inventory_endpoint_root(
    pool: &PgPool,
    key: &str,
) -> Result<Option<String>, QueryError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT provenance->>'endpoint_root' AS root \
         FROM file_inventory \
         WHERE file_server_id = $1 \
           AND NULLIF(TRIM(provenance->>'endpoint_root'), '') IS NOT NULL \
         GROUP BY provenance->>'endpoint_root' \
         ORDER BY COUNT(*) DESC, provenance->>'endpoint_root' \
         LIMIT 1",
    )
    .bind(key)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}

/// Insert a new identity-only file server, plus an optional inline first
/// endpoint. A unique `(workspace_id, key)` violation surfaces as a DB error the
/// handler maps to 409 (we pre-check existence in the handler for a clean message).
pub async fn create(
    pool: &PgPool,
    workspace_id: Uuid,
    req: &CreateFileServerRequest,
) -> Result<FileServer, QueryError> {
    if let Some(ep) = &req.endpoint {
        validate_access_method(&ep.access_method)?;
    }
    let display_name = req.display_name.clone().unwrap_or_else(|| req.key.clone());
    let config = req.config.clone().unwrap_or_else(|| serde_json::json!({}));

    let mut tx = pool.begin().await?;
    let server = sqlx::query_as::<_, FileServer>(
        "INSERT INTO file_servers (workspace_id, key, display_name, config) \
         VALUES ($1, $2, $3, $4) RETURNING *",
    )
    .bind(workspace_id)
    .bind(&req.key)
    .bind(&display_name)
    .bind(&config)
    .fetch_one(&mut *tx)
    .await?;

    if let Some(ep) = &req.endpoint {
        insert_endpoint_tx(&mut tx, server.id, ep).await?;
    }
    tx.commit().await?;
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

/// Update mutable fields of the identity-only parent. Returns `None` if no such
/// server.
pub async fn update(
    pool: &PgPool,
    workspace_id: Uuid,
    key: &str,
    req: &UpdateFileServerRequest,
) -> Result<Option<FileServer>, QueryError> {
    let server = sqlx::query_as::<_, FileServer>(
        "UPDATE file_servers SET \
            display_name = COALESCE($3, display_name), \
            status       = COALESCE($4, status), \
            config       = COALESCE($5, config), \
            updated_at   = NOW() \
         WHERE workspace_id = $1 AND key = $2 RETURNING *",
    )
    .bind(workspace_id)
    .bind(key)
    .bind(req.display_name.as_deref())
    .bind(req.status.as_deref())
    .bind(req.config.as_ref())
    .fetch_optional(pool)
    .await?;
    Ok(server)
}

/// Delete a server (its endpoints cascade). Returns whether a row was removed.
/// Inventory rows are untouched (soft join) — they revert to "unregistered".
pub async fn delete(pool: &PgPool, workspace_id: Uuid, key: &str) -> Result<bool, QueryError> {
    let r = sqlx::query("DELETE FROM file_servers WHERE workspace_id = $1 AND key = $2")
        .bind(workspace_id)
        .bind(key)
        .execute(pool)
        .await?;
    Ok(r.rows_affected() > 0)
}

// ---------------------------------------------------------------------------
// Endpoint CRUD (keyed on the parent server id).
// ---------------------------------------------------------------------------

/// Resolve a server's id from `(workspace_id, key)` — endpoint handlers address
/// the parent by key but the child table keys on the parent id.
pub async fn server_id(
    pool: &PgPool,
    workspace_id: Uuid,
    key: &str,
) -> Result<Option<Uuid>, QueryError> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM file_servers WHERE workspace_id = $1 AND key = $2")
            .bind(workspace_id)
            .bind(key)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|r| r.0))
}

/// List a server's endpoints (priority-ordered).
pub async fn list_endpoints(
    pool: &PgPool,
    file_server_id: Uuid,
) -> Result<Vec<FileServerEndpoint>, QueryError> {
    let rows = sqlx::query_as::<_, FileServerEndpoint>(
        "SELECT * FROM file_server_endpoints WHERE file_server_id = $1 \
         ORDER BY priority DESC, access_method, root",
    )
    .bind(file_server_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

async fn insert_endpoint_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    file_server_id: Uuid,
    req: &CreateEndpointRequest,
) -> Result<FileServerEndpoint, QueryError> {
    validate_access_method(&req.access_method)?;
    let root = req.root.clone().unwrap_or_default();
    let config = req.config.clone().unwrap_or_else(|| serde_json::json!({}));
    let priority = req.priority.unwrap_or(0);
    let ep = sqlx::query_as::<_, FileServerEndpoint>(
        "INSERT INTO file_server_endpoints \
            (file_server_id, access_method, root, resource_ref, group_id, priority, config) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
    )
    .bind(file_server_id)
    .bind(&req.access_method)
    .bind(&root)
    .bind(req.resource_ref.as_deref())
    .bind(req.group_id.as_deref())
    .bind(priority)
    .bind(&config)
    .fetch_one(&mut **tx)
    .await?;
    Ok(ep)
}

/// Create an endpoint under a server.
pub async fn create_endpoint(
    pool: &PgPool,
    file_server_id: Uuid,
    req: &CreateEndpointRequest,
) -> Result<FileServerEndpoint, QueryError> {
    let mut tx = pool.begin().await?;
    let ep = insert_endpoint_tx(&mut tx, file_server_id, req).await?;
    tx.commit().await?;
    Ok(ep)
}

/// Update an endpoint by id (scoped to its parent server). Returns `None` if no
/// such endpoint under that server.
pub async fn update_endpoint(
    pool: &PgPool,
    file_server_id: Uuid,
    endpoint_id: Uuid,
    req: &UpdateEndpointRequest,
) -> Result<Option<FileServerEndpoint>, QueryError> {
    if let Some(m) = req.access_method.as_deref() {
        validate_access_method(m)?;
    }
    let ep = sqlx::query_as::<_, FileServerEndpoint>(
        "UPDATE file_server_endpoints SET \
            access_method       = COALESCE($3, access_method), \
            root                = COALESCE($4, root), \
            resource_ref        = CASE WHEN $5 THEN $6 ELSE resource_ref END, \
            group_id            = CASE WHEN $7 THEN $8 ELSE group_id END, \
            status              = COALESCE($9, status), \
            verification_status = COALESCE($10, verification_status), \
            priority            = COALESCE($11, priority), \
            config              = COALESCE($12, config), \
            updated_at          = NOW() \
         WHERE file_server_id = $1 AND id = $2 RETURNING *",
    )
    .bind(file_server_id)
    .bind(endpoint_id)
    .bind(req.access_method.as_deref())
    .bind(req.root.as_deref())
    .bind(req.resource_ref.is_some())
    .bind(req.resource_ref.clone().flatten())
    .bind(req.group_id.is_some())
    .bind(req.group_id.clone().flatten())
    .bind(req.status.as_deref())
    .bind(req.verification_status.as_deref())
    .bind(req.priority)
    .bind(req.config.as_ref())
    .fetch_optional(pool)
    .await?;
    Ok(ep)
}

/// Delete an endpoint by id (scoped to its parent server). Returns whether a row
/// was removed.
pub async fn delete_endpoint(
    pool: &PgPool,
    file_server_id: Uuid,
    endpoint_id: Uuid,
) -> Result<bool, QueryError> {
    let r =
        sqlx::query("DELETE FROM file_server_endpoints WHERE file_server_id = $1 AND id = $2")
            .bind(file_server_id)
            .bind(endpoint_id)
            .execute(pool)
            .await?;
    Ok(r.rows_affected() > 0)
}

/// One servable physical copy: an inventory row joined to ONE endpoint of its
/// backing server. The serve bridge (`data::serve`) resolves a content hash to
/// these candidates and picks one by `access_method` preference.
#[derive(Debug, Clone)]
pub struct ServeCandidate {
    /// The physical path on the server (server-relative under the endpoint root).
    pub path: String,
    /// The endpoint to reach it through.
    pub endpoint: FileServerEndpoint,
}

/// Resolve every physical copy of `content_hash` in this workspace into its
/// servable endpoints. Joins `file_inventory` (the physical copies, by hash) →
/// `file_servers` (identity, by `key`) → `file_server_endpoints` (the transports).
///
/// Returns one [`ServeCandidate`] per (copy × endpoint). Only copies whose
/// server is registered in THIS workspace are returned — an unregistered
/// `file_server_id` has no endpoints to serve through. Endpoints come back
/// priority-ordered (highest first); the caller picks by `access_method`
/// preference within that order.
pub async fn serve_candidates(
    pool: &PgPool,
    workspace_id: Uuid,
    content_hash: &str,
) -> Result<Vec<ServeCandidate>, QueryError> {
    // (path, endpoint columns) per copy×endpoint, priority-ordered.
    let rows = sqlx::query_as::<_, ServeCandidateRow>(
        "SELECT fi.path AS copy_path, e.* \
         FROM file_inventory fi \
         JOIN file_servers fs \
           ON fs.key = fi.file_server_id AND fs.workspace_id = $1 \
         JOIN file_server_endpoints e \
           ON e.file_server_id = fs.id \
         WHERE fi.content_hash = $2 \
         ORDER BY e.priority DESC, e.access_method, e.root",
    )
    .bind(workspace_id)
    .bind(content_hash)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| ServeCandidate {
            path: r.copy_path,
            endpoint: r.endpoint,
        })
        .collect())
}

/// Row shape for [`serve_candidates`]: the copy's path plus the flattened
/// endpoint columns (`#[sqlx(flatten)]` maps `e.*` onto [`FileServerEndpoint`]).
#[derive(sqlx::FromRow)]
struct ServeCandidateRow {
    copy_path: String,
    #[sqlx(flatten)]
    endpoint: FileServerEndpoint,
}

/// Idempotently seed the built-in platform object store as a `file_servers` row
/// PLUS one `object_store` endpoint (called at startup). `key` is the platform
/// S3 bucket; the endpoint has no `resource_ref` (it uses platform config).
/// ON CONFLICT keeps any operator edits.
pub async fn seed_builtin_object_store(
    pool: &PgPool,
    workspace_id: Uuid,
    bucket: &str,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    // Identity-only parent.
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO file_servers (workspace_id, key, display_name, status) \
         VALUES ($1, $2, $3, 'online') \
         ON CONFLICT (workspace_id, key) DO UPDATE SET key = EXCLUDED.key \
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(bucket)
    .bind("Platform object store")
    .fetch_one(&mut *tx)
    .await?;
    let server_id = row.0;

    // One object_store endpoint at the root.
    sqlx::query(
        "INSERT INTO file_server_endpoints \
            (file_server_id, access_method, root, status) \
         VALUES ($1, 'object_store', '', 'online') \
         ON CONFLICT (file_server_id, access_method, root) DO NOTHING",
    )
    .bind(server_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}
