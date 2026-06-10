//! Read-model that joins the catalogue (logical) and inventory (physical) for
//! the unified Data browser. Composes the existing `catalogue` + `inventory`
//! repositories rather than reimplementing their filters.

use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use crate::inventory::model::InventoryEntry;
use crate::query::builder::QueryError;
use crate::query::extractor::QueryParams;
use crate::query::pagination::Paginated;

use super::model::*;

/// Max uncatalogued rows returned inline (the UI gets a count for the rest).
const UNCATALOGUED_PEEK: i64 = 50;

/// TTL for the uncatalogued count+peek cache (see [`uncatalogued_cached`]).
const UNCATALOGUED_TTL: std::time::Duration = std::time::Duration::from_secs(15);

/// Process-wide cache for the uncatalogued section of the response.
///
/// The anti-join COUNT (+ the peek's `ORDER BY updated_at` sort) scans all of
/// `file_inventory` — fine at demo scale, a multi-second full pass per
/// pageview at the 4M-file corpus. The section is workspace-global (the
/// underlying queries carry no workspace filter), so one short-TTL entry
/// serves every request; 15 s staleness is invisible next to a crawl
/// campaign's own batch cadence. `(checked_at, count, peek_rows)`.
static UNCATALOGUED_CACHE: tokio::sync::RwLock<
    Option<(std::time::Instant, i64, Vec<InventoryEntry>)>,
> = tokio::sync::RwLock::const_new(None);

/// Resolve `file_server_id` (== `file_servers.key`) → (display_name, kind) for a
/// workspace's servers. `kind` now lives on the child endpoints, so we surface
/// the highest-priority endpoint's `access_method` as the server's effective
/// transport kind (NULL when the server has no endpoints yet).
async fn server_lookup(
    pool: &PgPool,
    workspace_id: Uuid,
) -> Result<HashMap<String, (String, Option<String>, bool)>, sqlx::Error> {
    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT fs.key, fs.display_name, \
                (SELECT e.access_method FROM file_server_endpoints e \
                 WHERE e.file_server_id = fs.id \
                 ORDER BY e.priority DESC, e.access_method, e.root LIMIT 1) AS kind \
         FROM file_servers fs WHERE fs.workspace_id = $1",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await?;

    // Per-server "can any endpoint actually deliver bytes" — evaluated in Rust
    // with the SAME predicate routing uses (`endpoint_servable`), so the
    // browser's Download affordance can never disagree with the serve route.
    let endpoints: Vec<(String, crate::file_servers::model::FileServerEndpoint)> = sqlx::query_as(
        "SELECT fs.key, e.* FROM file_server_endpoints e \
         JOIN file_servers fs ON fs.id = e.file_server_id \
         WHERE fs.workspace_id = $1",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row: ServerEndpointRow| (row.key, row.endpoint))
    .collect();
    let mut servable_by_key: HashMap<String, bool> = HashMap::new();
    for (key, ep) in endpoints {
        let entry = servable_by_key.entry(key).or_insert(false);
        *entry = *entry || crate::data::serve::endpoint_servable(&ep);
    }

    Ok(rows
        .into_iter()
        .map(|(k, d, kind)| {
            let servable = servable_by_key.get(&k).copied().unwrap_or(false);
            (k, (d, kind, servable))
        })
        .collect())
}

/// Row shape for the endpoint-servability join: the server key + the full
/// endpoint record (flattened — `e.*` columns follow `fs.key`).
#[derive(sqlx::FromRow)]
struct ServerEndpointRow {
    key: String,
    #[sqlx(flatten)]
    endpoint: crate::file_servers::model::FileServerEndpoint,
}

fn to_copy(
    inv: InventoryEntry,
    servers: &HashMap<String, (String, Option<String>, bool)>,
) -> DataCopy {
    let (display, kind, servable) = servers
        .get(&inv.file_server_id)
        .map(|(d, k, s)| (Some(d.clone()), k.clone(), *s))
        .unwrap_or((None, None, false));
    DataCopy {
        file_server_id: inv.file_server_id,
        path: inv.path,
        status: inv.status,
        is_canonical: inv.is_canonical,
        server_display_name: display,
        server_kind: kind,
        servable,
    }
}

/// Page of catalogued entries (each with physical copies) + an uncatalogued peek.
pub async fn list_entries(
    pool: &PgPool,
    workspace_id: Uuid,
    params: &QueryParams,
) -> Result<DataEntriesResponse, QueryError> {
    // 1. Page of logical entries — reuse the catalogue list filters/pagination.
    let page = crate::catalogue::queries::list_entries(pool, params).await?;
    let servers = server_lookup(pool, workspace_id).await?;

    // 2. Physical copies for this page's content hashes, grouped by hash.
    let hashes: Vec<String> = page
        .items
        .iter()
        .filter_map(|e| e.content_hash.clone())
        .collect();
    let mut copies_by_hash: HashMap<String, Vec<DataCopy>> = HashMap::new();
    if !hashes.is_empty() {
        let rows = sqlx::query_as::<_, InventoryEntry>(
            "SELECT * FROM file_inventory WHERE content_hash = ANY($1)",
        )
        .bind(&hashes)
        .fetch_all(pool)
        .await?;
        for r in rows {
            let hash = r.content_hash.clone().unwrap_or_default();
            copies_by_hash
                .entry(hash)
                .or_default()
                .push(to_copy(r, &servers));
        }
    }

    // 3. Assemble DataEntry rows — carry the FULL catalogue entry so the
    //    unified browser renders the same rich card the catalogue page did.
    let entries: Vec<DataEntry> = page
        .items
        .into_iter()
        .map(|e| {
            let copies = e
                .content_hash
                .as_ref()
                .and_then(|h| copies_by_hash.get(h).cloned())
                .unwrap_or_default();
            DataEntry { entry: e, copies }
        })
        .collect();

    let page_out = Paginated {
        items: entries,
        total: page.total,
        page: page.page,
        page_size: page.page_size,
        total_pages: page.total_pages,
        has_next: page.has_next,
        has_previous: page.has_previous,
    };

    // 4. Uncatalogued (index-only) files: inventory rows whose content_hash
    //    matches no catalogue row (NULL hash, or hashed-but-not-registered).
    //    Served through the short-TTL cache; per-request server resolution
    //    (display name / servable) still happens below, so a server rename or
    //    endpoint verify shows up immediately even on a cached peek.
    let (uncatalogued_count, peek_rows) = uncatalogued_cached(pool).await?;
    let uncatalogued = peek_rows
        .into_iter()
        .map(|r| {
            let name = r.path.rsplit('/').next().unwrap_or(&r.path).to_string();
            let content_hash = r.content_hash.clone();
            let first_seen = r.first_seen;
            UncataloguedFile {
                name,
                content_hash,
                first_seen,
                copies: vec![to_copy(r, &servers)],
            }
        })
        .collect();

    Ok(DataEntriesResponse {
        page: page_out,
        uncatalogued,
        uncatalogued_count,
    })
}

/// Count + peek of uncatalogued inventory rows, through [`UNCATALOGUED_CACHE`].
async fn uncatalogued_cached(pool: &PgPool) -> Result<(i64, Vec<InventoryEntry>), sqlx::Error> {
    if let Some((at, count, rows)) = UNCATALOGUED_CACHE.read().await.as_ref() {
        if at.elapsed() < UNCATALOGUED_TTL {
            return Ok((*count, rows.clone()));
        }
    }

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM file_inventory fi \
         LEFT JOIN catalogue_entries c ON c.content_hash = fi.content_hash \
         WHERE c.entry_id IS NULL",
    )
    .fetch_one(pool)
    .await?;

    let rows = if count > 0 {
        sqlx::query_as::<_, InventoryEntry>(
            "SELECT fi.* FROM file_inventory fi \
             LEFT JOIN catalogue_entries c ON c.content_hash = fi.content_hash \
             WHERE c.entry_id IS NULL ORDER BY fi.updated_at DESC LIMIT $1",
        )
        .bind(UNCATALOGUED_PEEK)
        .fetch_all(pool)
        .await?
    } else {
        Vec::new()
    };

    *UNCATALOGUED_CACHE.write().await = Some((std::time::Instant::now(), count, rows.clone()));
    Ok((count, rows))
}
