//! Library-pack CRUD + import/export (Phase: library packs).
//!
//! A "pack" groups the `library_node` templates that ship together under one
//! `vendor/slug` coordinate (see `migrations/20240187000000_library_packs.sql`).
//! These endpoints let a workspace Admin/Owner IMPORT a self-contained
//! [`PackBundle`] (creating the pack row + recompiling each node's graph into a
//! published `library_node` template), EXPORT an existing pack/vendor back to a
//! bundle, LIST/GET packs, and DELETE a pack (removing its node families).
//!
//! ## Reuse, not reimplementation
//!
//! - Compilation goes through [`PublishService::compile_artifacts`] — the exact
//!   path the demo seeder (`demos::seed_one`) uses. Import never reimplements
//!   graph→AIR lowering, and the carried bundle never ships AIR (only the
//!   authored graph), so a tampered bundle can't smuggle stale artifacts.
//! - Coordinate + category validation reuse `governance::{validate_coordinate,
//!   validate_category}`.
//! - Role gating reuses `auth::require_role` (workspace Admin/Owner), the same
//!   gate `governance::promote_template` uses.
//! - Coordinate uniqueness uses the same pre-insert SELECT pattern as
//!   `demos::seed_one` / `governance::promote_template`.

use axum::{
    extract::{Multipart, Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::{map_to_api_error, require_role, AuthUser, Role};
use crate::handlers::governance::{validate_category, validate_coordinate};
use crate::handlers::node_library::LibraryNodeDescriptor;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::library_pack::{
    LibraryPack, LibraryPackDetail, LibraryPackSummary, PackBundle, PackImportResult, PackManifest,
    PackNode,
};
use crate::models::template::{Presentation, WorkflowGraph};
use crate::process::publish::{ArtifactKeySpace, CompiledArtifacts, PublishService};
use crate::AppState;

/// Origin assigned to every IMPORTED pack + its nodes. `system` and `community`
/// are reserved (seed-only / platform-admin review) — import rejects them.
const IMPORT_ORIGIN: &str = "workspace";

/// `asset:`-token prefix a `presentation.icon` uses to reference an uploaded
/// logo blob (`asset:{uuid}`). Mirrors the [`PackAsset::ref`] convention.
///
/// CONVENTION: a `presentation.icon` value that starts with `asset:` is a custom
/// uploaded logo (`asset:{uuid}` → serve via `GET /api/v1/library/icons/{uuid}`);
/// any other value is a named icon-registry key resolved by the frontend
/// `resolveNodeIcon`.
const ICON_ASSET_PREFIX: &str = "asset:";

/// Maximum size of an uploaded library logo (1 MiB). Logos are small branding
/// glyphs; this keeps a stray large upload out of the icon keyspace.
const MAX_LOGO_BYTES: usize = 1024 * 1024;

/// MIME types accepted for an uploaded library logo. Image-only (the icon serve
/// endpoint streams them straight back with this content-type into an `<img>`).
const ALLOWED_LOGO_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/svg+xml",
];

/// True when a `presentation.icon` value is a custom uploaded-logo token
/// (`asset:{uuid}`) rather than a named icon-registry key.
pub fn is_asset_icon(icon: &str) -> bool {
    icon.starts_with(ICON_ASSET_PREFIX)
}

/// Store a library logo blob and return its `asset:{uuid}` token. Reusable by
/// the upload endpoint and by pack import (which re-stores carried logo bytes).
/// The returned token is what a node's `presentation.icon` should carry.
pub async fn store_library_icon(
    state: &AppState,
    bytes: &[u8],
    mime: &str,
) -> Result<String, ApiError> {
    let logo_id = state
        .s3
        .upload_library_logo(bytes, mime)
        .await
        .map_err(|e| ApiError::internal(format!("store library logo: {e}")))?;
    Ok(format!("{ICON_ASSET_PREFIX}{logo_id}"))
}

/// Load a library logo blob by its `asset:{uuid}` token. Returns
/// `(bytes, mime)`. Reusable by the icon serve endpoint and by pack export
/// (which embeds the bytes as a `PackAsset`).
pub async fn load_library_icon(
    state: &AppState,
    asset_token: &str,
) -> Result<(Vec<u8>, String), ApiError> {
    let logo_id = asset_token
        .strip_prefix(ICON_ASSET_PREFIX)
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| ApiError::bad_request("not a valid asset icon token"))?;
    state
        .s3
        .get_library_logo(logo_id)
        .await
        .map_err(|_| ApiError::not_found("library logo not found"))
}

/// POST /api/v1/library/icons
///
/// Upload a custom library logo (multipart image). Any authenticated user may
/// upload — no special role is required to provide a branding glyph; the
/// resulting `asset:{uuid}` token is only ever adopted into a node's
/// `presentation.icon` through a role-gated promote/import path. The bytes are
/// stored under the dedicated `library-icons/{uuid}` keyspace (NOT the
/// asset-type/record system). Returns `{ icon: "asset:{uuid}" }`.
#[utoipa::path(
    post,
    path = "/api/v1/library/icons",
    request_body(content = LibraryIconUpload, content_type = "multipart/form-data"),
    responses(
        (status = 200, description = "Logo stored; returns its asset token", body = LibraryIconResponse),
        (status = 400, description = "Missing/empty file, oversize, or unsupported type", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "library-packs",
)]
pub async fn upload_library_icon(
    State(state): State<AppState>,
    _user: AuthUser,
    mut multipart: Multipart,
) -> Result<Json<LibraryIconResponse>, ApiError> {
    let field = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("invalid multipart: {e}")))?
        .ok_or_else(|| ApiError::bad_request("no file field in multipart body"))?;

    let mime = field
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_string();
    if !ALLOWED_LOGO_TYPES.contains(&mime.as_str()) {
        return Err(ApiError::bad_request(format!(
            "unsupported logo type: {mime}. Allowed: {ALLOWED_LOGO_TYPES:?}"
        )));
    }

    let bytes = field
        .bytes()
        .await
        .map_err(|e| ApiError::bad_request(format!("failed to read file: {e}")))?;
    if bytes.is_empty() {
        return Err(ApiError::bad_request("uploaded logo is empty"));
    }
    if bytes.len() > MAX_LOGO_BYTES {
        return Err(ApiError::bad_request(format!(
            "logo too large: {} bytes (max {MAX_LOGO_BYTES})",
            bytes.len()
        )));
    }

    let icon = store_library_icon(&state, &bytes, &mime).await?;
    tracing::info!(icon = %icon, mime = %mime, bytes = bytes.len(), "library logo uploaded");
    Ok(Json(LibraryIconResponse { icon }))
}

/// GET /api/v1/library/icons/{id}
///
/// Stream a previously uploaded library logo by its id. Within the auth gate but
/// requires no role — the palette/management views render these inline in an
/// `<img>` for any signed-in user. Served with the stored content-type and an
/// immutable long-lived cache (logos are content-addressed by id).
#[utoipa::path(
    get,
    path = "/api/v1/library/icons/{id}",
    params(("id" = Uuid, Path, description = "Library logo id")),
    responses(
        (status = 200, description = "Logo bytes", content_type = "application/octet-stream"),
        (status = 404, description = "Logo not found", body = ErrorResponse),
    ),
    tag = "library-packs",
)]
pub async fn get_library_icon(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match state.s3.get_library_logo(id).await {
        Ok((bytes, mime)) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, mime),
                (
                    header::CACHE_CONTROL,
                    "public, max-age=31536000, immutable".to_string(),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => {
            tracing::warn!(logo_id = %id, error = %e, "failed to get library logo from S3");
            ApiError::not_found("library logo not found").into_response()
        }
    }
}

/// Multipart upload body for `POST /api/v1/library/icons` (OpenAPI shape only).
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct LibraryIconUpload {
    /// The image file (png/jpeg/webp/gif/svg+xml, ≤ 1 MiB).
    #[schema(format = "binary", value_type = String)]
    file: String,
}

/// Response of `POST /api/v1/library/icons`: the `asset:{uuid}` token to drop
/// into a node's `presentation.icon`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryIconResponse {
    /// The custom-logo token, e.g. `asset:550e8400-e29b-41d4-a716-446655440000`.
    pub icon: String,
}

/// GET /api/v1/library/packs
///
/// List the library packs visible to the caller: those in the caller's active
/// workspace plus any `system`-origin pack (platform-shipped, visible
/// everywhere — mirrors the public library-node visibility rule). Each row
/// carries `nodeCount` (the number of `is_latest` library-node families it
/// owns) and `myEffectiveRole` so the management view can gate Import/Delete to
/// `admin`+.
#[utoipa::path(
    get,
    path = "/api/v1/library/packs",
    responses(
        (status = 200, description = "Visible library packs", body = Vec<LibraryPackSummary>),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "library-packs",
)]
pub async fn list_packs(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<LibraryPackSummary>>, ApiError> {
    let workspace_id = user.require_workspace()?;

    // Own-workspace OR system-origin packs, each with its live node count.
    let rows = sqlx::query_as::<_, PackWithCount>(
        "SELECT p.id, p.workspace_id, p.vendor, p.slug, p.version, p.name, \
                p.description, p.origin, p.installed_by, p.installed_at, \
                COALESCE(( \
                    SELECT COUNT(*) FROM workflow_templates t \
                     WHERE t.pack_id = p.id AND t.is_latest = TRUE \
                       AND t.template_kind = 'library_node' \
                ), 0) AS node_count \
           FROM library_packs p \
          WHERE p.workspace_id = $1 OR p.origin = 'system' \
          ORDER BY p.vendor, p.slug",
    )
    .bind(workspace_id)
    .fetch_all(&state.db)
    .await?;

    let mut items: Vec<LibraryPackSummary> = rows
        .into_iter()
        .map(|r| LibraryPackSummary {
            node_count: r.node_count,
            my_effective_role: None,
            pack: LibraryPack {
                id: r.id,
                workspace_id: r.workspace_id,
                vendor: r.vendor,
                slug: r.slug,
                version: r.version,
                name: r.name,
                description: r.description,
                origin: r.origin,
                installed_by: r.installed_by,
                installed_at: r.installed_at,
            },
        })
        .collect();

    annotate_pack_roles(&state, &user, workspace_id, &mut items).await?;
    Ok(Json(items))
}

#[derive(sqlx::FromRow)]
struct PackWithCount {
    id: Uuid,
    workspace_id: Uuid,
    vendor: String,
    slug: String,
    version: String,
    name: String,
    description: String,
    origin: String,
    installed_by: Option<Uuid>,
    installed_at: chrono::DateTime<chrono::Utc>,
    node_count: i64,
}

impl crate::auth::AclAnnotated for LibraryPackSummary {
    fn acl_id(&self) -> Uuid {
        // The pack itself has no object_acl rows; gate on the WORKSPACE role
        // (set directly below in `annotate_pack_roles`). `acl_id` is unused on
        // this path, but the trait requires it — return the pack id.
        self.pack.id
    }
    fn set_my_effective_role(&mut self, role: Option<String>) {
        self.my_effective_role = role;
    }
}

/// Stamp each summary's `myEffectiveRole` from the caller's WORKSPACE membership
/// role on the pack's owning workspace. A pack is not an ACL object (no
/// object_acl rows), so unlike library nodes the gate is purely the workspace
/// role: own-workspace packs get the caller's role; `system` packs (other
/// workspace) get no role (read-only) unless the caller is also a member there.
async fn annotate_pack_roles(
    state: &AppState,
    user: &AuthUser,
    _workspace_id: Uuid,
    items: &mut [LibraryPackSummary],
) -> Result<(), ApiError> {
    // Resolve the caller's role per distinct workspace once.
    let mut role_cache: HashMap<Uuid, Option<String>> = HashMap::new();
    for item in items.iter_mut() {
        let ws = item.pack.workspace_id;
        let role = match role_cache.get(&ws) {
            Some(r) => r.clone(),
            None => {
                let r = crate::auth::member_role(&state.db, user, ws)
                    .await
                    .ok()
                    .map(|role| role.as_label().to_string());
                role_cache.insert(ws, r.clone());
                r
            }
        };
        item.my_effective_role = role;
    }
    Ok(())
}

/// GET /api/v1/library/packs/{id}
///
/// Pack detail plus the library nodes it owns (latest version of each family,
/// in the same descriptor shape the palette consumes). Visible when the pack is
/// in the caller's workspace or is `system`-origin.
#[utoipa::path(
    get,
    path = "/api/v1/library/packs/{id}",
    params(("id" = Uuid, Path, description = "Pack id")),
    responses(
        (status = 200, description = "Pack detail + its library nodes", body = LibraryPackDetail),
        (status = 404, description = "Pack not found / not visible", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "library-packs",
)]
pub async fn get_pack(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<LibraryPackDetail>, ApiError> {
    let workspace_id = user.require_workspace()?;
    let pack = load_visible_pack(&state, workspace_id, id).await?;

    // The pack's library nodes — latest version of each family, descriptor shape.
    let node_rows = sqlx::query_as::<_, PackNodeRow>(
        "SELECT coordinate, \
                COALESCE(base_template_id, id) AS template_id, \
                version, name, description, origin, lifecycle_status, superseded_by, presentation \
           FROM workflow_templates \
          WHERE pack_id = $1 AND is_latest = TRUE \
            AND template_kind = 'library_node' AND coordinate IS NOT NULL \
          ORDER BY (presentation->>'category') NULLS LAST, \
                   (presentation->>'vendor') NULLS LAST, name",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    let nodes = node_rows
        .into_iter()
        .map(|r| LibraryNodeDescriptor {
            coordinate: r.coordinate,
            template_id: r.template_id,
            version: r.version,
            name: r.name,
            description: r.description,
            origin: r.origin.unwrap_or_else(|| "system".to_string()),
            lifecycle_status: r.lifecycle_status,
            superseded_by: r.superseded_by,
            presentation: r
                .presentation
                .and_then(|v| serde_json::from_value::<Presentation>(v).ok()),
            my_effective_role: None,
        })
        .collect();

    Ok(Json(LibraryPackDetail { pack, nodes }))
}

#[derive(sqlx::FromRow)]
struct PackNodeRow {
    coordinate: String,
    template_id: Uuid,
    version: i32,
    name: String,
    description: Option<String>,
    origin: Option<String>,
    lifecycle_status: String,
    superseded_by: Option<String>,
    presentation: Option<serde_json::Value>,
}

/// Load a pack the caller may see (own workspace OR system origin), 404 otherwise.
async fn load_visible_pack(
    state: &AppState,
    workspace_id: Uuid,
    id: Uuid,
) -> Result<LibraryPack, ApiError> {
    sqlx::query_as::<_, LibraryPack>(
        "SELECT id, workspace_id, vendor, slug, version, name, description, \
                origin, installed_by, installed_at \
           FROM library_packs \
          WHERE id = $1 AND (workspace_id = $2 OR origin = 'system')",
    )
    .bind(id)
    .bind(workspace_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("pack not found"))
}

/// POST /api/v1/library/packs/import
///
/// Import a self-contained [`PackBundle`] into the caller's active workspace.
/// Workspace Admin/Owner only. ALL-OR-NOTHING: the pack row + every node + every
/// asset commit together or not at all. Each node's graph is RECOMPILED through
/// the same path the seeder uses (no AIR is carried); coordinate format +
/// category are validated; coordinate uniqueness within the `workspace` origin
/// is enforced (409 on clash). Asset bytes are re-stored under fresh logo ids
/// and the node's `presentation.icon` token is rewritten to point at the new id.
#[utoipa::path(
    post,
    path = "/api/v1/library/packs/import",
    request_body = PackBundle,
    responses(
        (status = 200, description = "Pack imported", body = PackImportResult),
        (status = 400, description = "Invalid bundle / coordinate / category / origin", body = ErrorResponse),
        (status = 403, description = "Caller lacks workspace Admin/Owner", body = ErrorResponse),
        (status = 409, description = "Pack or node coordinate already in use", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "library-packs",
)]
pub async fn import_pack(
    State(state): State<AppState>,
    user: AuthUser,
    Json(bundle): Json<PackBundle>,
) -> Result<Json<PackImportResult>, ApiError> {
    let workspace_id = user
        .workspace_id
        .ok_or_else(|| ApiError::bad_request("no active workspace"))?;
    let principal = user.subject_as_uuid();

    // Admin/Owner on the active workspace (same gate as promote).
    require_role(&state.db, &user, workspace_id, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    // Pack coordinate (vendor/slug) must be well-formed.
    let pack_coord = format!("{}/{}", bundle.manifest.vendor, bundle.manifest.slug);
    validate_coordinate(&pack_coord)?;

    if bundle.nodes.is_empty() {
        return Err(ApiError::bad_request("pack bundle carries no nodes"));
    }

    // Pre-flight (read-only, outside the txn): pack-coordinate uniqueness within
    // the import origin. The unique index also enforces this, but a pre-check
    // turns 23505 into a friendly 409.
    let pack_clash: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM library_packs \
          WHERE origin = $1 AND vendor = $2 AND slug = $3",
    )
    .bind(IMPORT_ORIGIN)
    .bind(&bundle.manifest.vendor)
    .bind(&bundle.manifest.slug)
    .fetch_optional(&state.db)
    .await?;
    if pack_clash.is_some() {
        return Err(ApiError::conflict(format!(
            "a {IMPORT_ORIGIN} pack already exists at coordinate `{pack_coord}`"
        )));
    }

    // Validate + parse each node BEFORE any write so a malformed bundle fails
    // cleanly (the compile + INSERT loop below still runs in a transaction, but
    // cheap validation up front gives precise 400s).
    let mut prepared: Vec<PreparedNode> = Vec::with_capacity(bundle.nodes.len());
    for node in &bundle.nodes {
        validate_coordinate(&node.coordinate)?;
        let presentation: Presentation = serde_json::from_value(node.presentation.clone())
            .map_err(|e| {
                ApiError::bad_request(format!(
                    "node `{}` has an invalid presentation: {e}",
                    node.coordinate
                ))
            })?;
        validate_category(&presentation)?;
        let graph: WorkflowGraph = serde_json::from_value(node.graph.clone()).map_err(|e| {
            ApiError::bad_request(format!(
                "node `{}` has an invalid graph: {e}",
                node.coordinate
            ))
        })?;
        prepared.push(PreparedNode {
            coordinate: node.coordinate.clone(),
            name: node.name.clone(),
            description: node.description.clone(),
            presentation_json: node.presentation.clone(),
            graph,
            files: node.files.clone(),
        });
    }

    // Re-store every asset blob NOW (before the txn) and build the
    // old-ref → asset:{new_id} rewrite map. S3 writes can't enroll in the
    // Postgres txn; an orphaned blob (if the txn later rolls back) is inert and
    // GC-eligible, the same posture the publish path takes with staged files.
    let mut icon_rewrite: HashMap<String, String> = HashMap::new();
    for asset in &bundle.assets {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(asset.data_base64.as_bytes())
            .map_err(|e| {
                ApiError::bad_request(format!("asset `{}` is not valid base64: {e}", asset.r#ref))
            })?;
        let new_token = store_library_icon(&state, &bytes, &asset.mime).await?;
        icon_rewrite.insert(asset.r#ref.clone(), new_token);
    }

    // Compile every node's artifacts BEFORE opening the txn. `compile_artifacts`
    // performs S3-free reads only (catalogue lookups), so doing it outside the
    // txn keeps the transaction short and never holds a row lock across a
    // compile. Each node is assigned a fresh family id (= its own row id).
    let publisher = PublishService::new(&state);
    let mut compiled: Vec<CompiledNode> = Vec::with_capacity(prepared.len());
    for p in &prepared {
        let template_id = Uuid::new_v4();

        // Rewrite the presentation icon token, if it references a carried asset.
        let mut presentation_json = p.presentation_json.clone();
        rewrite_icon_token(&mut presentation_json, &icon_rewrite);

        // Seed the compile file set with the bundle's carried per-node sources
        // (e.g. a Python step's `main.py`). The graph references these by name;
        // without them the compiler's validation fails ("entrypoint not found").
        // `compile_artifacts` may add generated stubs (e.g. `_aithericon_io.pyi`)
        // into this map — we keep the post-compile set to re-seed the editor doc.
        let mut files: HashMap<String, HashMap<String, String>> = p.files.clone();
        let CompiledArtifacts {
            air_json,
            graph_json,
            interface_json,
            node_configs,
            metrics,
        } = publisher
            .compile_artifacts(
                &p.graph,
                &p.name,
                &p.description,
                template_id,
                1,
                ArtifactKeySpace::Version,
                Some(template_id),
                &mut files,
                principal,
                workspace_id,
            )
            .await
            .map_err(|e| {
                tracing::warn!(coordinate = %p.coordinate, error = ?e, "pack import node compile failed");
                ApiError::bad_request(format!("node `{}` failed to compile", p.coordinate))
            })?;

        // Upload the node's compiled files/configs to S3 (version keyspace),
        // same as the seeder. Inert-on-rollback like the asset blobs above.
        publisher
            .upload_files(template_id, 1, &files)
            .await
            .map_err(|e| ApiError::internal(format!("upload node files: {e}")))?;
        publisher
            .upload_node_configs(template_id, 1, &node_configs)
            .await
            .map_err(|e| ApiError::internal(format!("upload node configs: {e}")))?;

        compiled.push(CompiledNode {
            template_id,
            coordinate: p.coordinate.clone(),
            name: p.name.clone(),
            description: p.description.clone(),
            presentation_json,
            graph: p.graph.clone(),
            graph_json,
            air_json,
            interface_json,
            metrics,
            files,
        });
    }

    // Transactional all-or-nothing: pack row + every node row.
    let mut tx = state.db.begin().await?;

    let pack_id = Uuid::new_v4();
    let pack: LibraryPack = sqlx::query_as(
        "INSERT INTO library_packs \
            (id, workspace_id, vendor, slug, version, name, description, origin, installed_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         RETURNING id, workspace_id, vendor, slug, version, name, description, origin, \
                   installed_by, installed_at",
    )
    .bind(pack_id)
    .bind(workspace_id)
    .bind(&bundle.manifest.vendor)
    .bind(&bundle.manifest.slug)
    .bind(&bundle.manifest.version)
    .bind(&bundle.manifest.name)
    .bind(&bundle.manifest.description)
    .bind(IMPORT_ORIGIN)
    .bind(principal)
    .fetch_one(&mut *tx)
    .await?;

    for node in &compiled {
        // Coordinate uniqueness within the import origin among live families —
        // inside the txn so a concurrent import can't slip a clashing coordinate
        // between this SELECT and the INSERT (the partial unique index is the
        // ultimate guard; this yields the friendly 409). Re-using the seeder's
        // pre-insert pattern.
        let clash: Option<(Uuid,)> = sqlx::query_as(
            "SELECT COALESCE(base_template_id, id) FROM workflow_templates \
              WHERE coordinate = $1 AND origin IS NOT DISTINCT FROM $2 \
                AND is_latest AND coordinate IS NOT NULL LIMIT 1",
        )
        .bind(&node.coordinate)
        .bind(IMPORT_ORIGIN)
        .fetch_optional(&mut *tx)
        .await?;
        if clash.is_some() {
            tx.rollback().await?;
            return Err(ApiError::conflict(format!(
                "node coordinate `{}` is already in use by another {IMPORT_ORIGIN} library node",
                node.coordinate
            )));
        }

        sqlx::query(
            "INSERT INTO workflow_templates \
                (id, name, description, base_template_id, version, is_latest, published, \
                 published_at, graph, air_json, interface_json, author_id, workspace_id, \
                 visibility, owner_template_id, metrics, template_kind, origin, coordinate, \
                 presentation, lifecycle_status, pack_id) \
             VALUES ($1, $2, $3, $1, 1, TRUE, TRUE, NOW(), $4, $5, $6, $7, $8, \
                     'public', NULL, $9, 'library_node', $10, $11, $12, 'active', $13)",
        )
        .bind(node.template_id)
        .bind(&node.name)
        .bind(&node.description)
        .bind(&node.graph_json)
        .bind(&node.air_json)
        .bind(&node.interface_json)
        .bind(principal)
        .bind(workspace_id)
        .bind(&node.metrics)
        .bind(IMPORT_ORIGIN)
        .bind(&node.coordinate)
        .bind(&node.presentation_json)
        .bind(pack_id)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    // Seed each imported node's editor Yjs doc from its graph + post-compile
    // files, mirroring the demo seeder. Best-effort: a doc-init failure is
    // logged but doesn't fail the (already-committed) import — the node is still
    // usable from the palette; only its in-editor source view would be empty
    // until first open re-derives from the graph.
    for node in &compiled {
        if let Err(e) = state
            .yjs
            .persistence
            .init_doc_from_graph_with_files(node.template_id, &node.graph, &node.files)
            .await
        {
            tracing::warn!(
                template_id = %node.template_id,
                coordinate = %node.coordinate,
                error = %e,
                "pack import: y.doc init failed"
            );
        }
    }

    let node_count = compiled.len() as i64;
    tracing::info!(
        pack_id = %pack_id,
        coordinate = %pack_coord,
        nodes = node_count,
        workspace = %workspace_id,
        principal = %principal,
        "library pack imported"
    );

    Ok(Json(PackImportResult { pack, node_count }))
}

struct PreparedNode {
    coordinate: String,
    name: String,
    description: String,
    presentation_json: serde_json::Value,
    graph: WorkflowGraph,
    files: HashMap<String, HashMap<String, String>>,
}

struct CompiledNode {
    template_id: Uuid,
    coordinate: String,
    name: String,
    description: String,
    presentation_json: serde_json::Value,
    graph: WorkflowGraph,
    graph_json: serde_json::Value,
    air_json: serde_json::Value,
    interface_json: serde_json::Value,
    metrics: serde_json::Value,
    /// Post-compile per-node file set (authored sources + generated stubs),
    /// used to seed the editor Yjs doc after the row is committed.
    files: HashMap<String, HashMap<String, String>>,
}

/// Rewrite a presentation's `icon` field in-place when it is an `asset:{old}`
/// token present in `rewrite`. No-op for registry-key icons or unknown tokens.
fn rewrite_icon_token(presentation: &mut serde_json::Value, rewrite: &HashMap<String, String>) {
    if let Some(icon) = presentation.get("icon").and_then(|v| v.as_str()) {
        if let Some(new_token) = rewrite.get(icon) {
            presentation["icon"] = serde_json::Value::String(new_token.clone());
        }
    }
}

/// Query params for `GET /api/v1/library/packs/export`.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ExportParams {
    /// Export the pack with this id (preferred — exact).
    #[serde(default)]
    pub pack_id: Option<Uuid>,
    /// Export every visible library node carrying this `presentation.vendor`
    /// (fallback when the nodes were promoted ad-hoc with no `pack_id`).
    #[serde(default)]
    pub vendor: Option<String>,
}

/// GET /api/v1/library/packs/export
///
/// Assemble a portable [`PackBundle`] from existing library nodes — by `packId`
/// (exact) or by `vendor` (every visible library node with that
/// `presentation.vendor`). Each node's coordinate/name/description/presentation/
/// graph are emitted verbatim from the DB row; any `presentation.icon` of the
/// form `asset:{uuid}` has its bytes loaded from S3 and embedded as a
/// [`PackAsset`]. The result is import-ready (symmetric round-trip).
#[utoipa::path(
    get,
    path = "/api/v1/library/packs/export",
    params(ExportParams),
    responses(
        (status = 200, description = "Assembled pack bundle", body = PackBundle),
        (status = 400, description = "Neither packId nor vendor supplied", body = ErrorResponse),
        (status = 404, description = "Pack not found / no matching nodes", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "library-packs",
)]
pub async fn export_pack(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ExportParams>,
) -> Result<Json<PackBundle>, ApiError> {
    let workspace_id = user.require_workspace()?;

    // Resolve the manifest + the node rows. `packId` is exact; `vendor` collects
    // visible (own-workspace or public) library nodes by their branding vendor.
    let (manifest, node_rows): (PackManifest, Vec<ExportNodeRow>) =
        if let Some(pack_id) = params.pack_id {
            let pack = load_visible_pack(&state, workspace_id, pack_id).await?;
            let rows = sqlx::query_as::<_, ExportNodeRow>(
                "SELECT id, coordinate, name, description, presentation, graph \
               FROM workflow_templates \
              WHERE pack_id = $1 AND is_latest = TRUE \
                AND template_kind = 'library_node' AND coordinate IS NOT NULL \
              ORDER BY coordinate",
            )
            .bind(pack_id)
            .fetch_all(&state.db)
            .await?;
            let manifest = PackManifest {
                vendor: pack.vendor,
                slug: pack.slug,
                version: pack.version,
                name: pack.name,
                description: pack.description,
            };
            (manifest, rows)
        } else if let Some(vendor) = params.vendor {
            let rows = sqlx::query_as::<_, ExportNodeRow>(
                "SELECT id, coordinate, name, description, presentation, graph \
               FROM workflow_templates \
              WHERE is_latest = TRUE AND template_kind = 'library_node' \
                AND coordinate IS NOT NULL \
                AND presentation->>'vendor' = $1 \
                AND (workspace_id = $2 OR visibility = 'public') \
              ORDER BY coordinate",
            )
            .bind(&vendor)
            .bind(workspace_id)
            .fetch_all(&state.db)
            .await?;
            if rows.is_empty() {
                return Err(ApiError::not_found(format!(
                    "no visible library nodes for vendor `{vendor}`"
                )));
            }
            // Derive a manifest slug from the first node's coordinate vendor half.
            let slug = rows[0]
                .coordinate
                .split('/')
                .next()
                .unwrap_or("vendor")
                .to_string();
            let manifest = PackManifest {
                vendor: vendor.clone(),
                slug,
                version: "1".to_string(),
                name: vendor,
                description: String::new(),
            };
            (manifest, rows)
        } else {
            return Err(ApiError::bad_request("supply either `packId` or `vendor`"));
        };

    // Build the node list + collect referenced asset tokens (deduped).
    let mut nodes: Vec<PackNode> = Vec::with_capacity(node_rows.len());
    let mut asset_refs: Vec<String> = Vec::new();
    for r in node_rows {
        if let Some(icon) = r.presentation.get("icon").and_then(|v| v.as_str()) {
            if is_asset_icon(icon) && !asset_refs.iter().any(|a| a == icon) {
                asset_refs.push(icon.to_string());
            }
        }
        // Per-node source files (e.g. `main.py`) live in the template's editor
        // Yjs doc, NOT the graph JSON — pull them so the bundle is self-contained
        // and import can recompile. A missing/empty doc yields no files (a
        // graph-only node still round-trips).
        let files = match state.yjs.persistence.load_doc(r.id).await {
            Ok(doc) => crate::yjs::doc_ops::extract_files_from_doc(&doc),
            Err(e) => {
                tracing::warn!(template_id = %r.id, coordinate = %r.coordinate, error = %e, "export: failed to load node doc for files");
                std::collections::HashMap::new()
            }
        };
        nodes.push(PackNode {
            coordinate: r.coordinate,
            name: r.name,
            description: r.description.unwrap_or_default(),
            presentation: r.presentation,
            graph: r.graph,
            files,
        });
    }

    // Embed each referenced logo blob (base64). A missing blob is skipped with a
    // warning — export must not 500 on a dangling icon token.
    let mut assets = Vec::new();
    for token in asset_refs {
        match load_library_icon(&state, &token).await {
            Ok((bytes, mime)) => {
                assets.push(crate::models::library_pack::PackAsset {
                    r#ref: token,
                    mime,
                    data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
                });
            }
            Err(e) => {
                tracing::warn!(token = %token, error = ?e, "export: pack logo blob missing, skipped");
            }
        }
    }

    Ok(Json(PackBundle {
        manifest,
        nodes,
        assets,
    }))
}

#[derive(sqlx::FromRow)]
struct ExportNodeRow {
    id: Uuid,
    coordinate: String,
    name: String,
    description: Option<String>,
    presentation: serde_json::Value,
    graph: serde_json::Value,
}

/// DELETE /api/v1/library/packs/{id}
///
/// Remove a pack and its library-node families. Workspace Admin/Owner only.
/// REFUSES (409) when any of the pack's nodes is still embedded as a frozen
/// sub-workflow in another template's graph (best-effort in-use check — see
/// below). Otherwise deletes the node families (all versions) + the pack row,
/// transactionally.
///
/// ## In-use check (best-effort)
///
/// A library node is dropped onto a canvas as a `sub_workflow` node stamped with
/// the node's `sourceCoordinate` (decision 12). We refuse the delete if any OTHER
/// template's `graph` JSON references one of this pack's coordinates as a
/// sub-workflow `sourceCoordinate`. There is no dedicated reference-count table
/// yet, so this scans the graph JSONB; it is intentionally conservative (a stale
/// draft referencing the coordinate also blocks). `system`-origin packs are never
/// deletable via this endpoint (seed-managed).
#[utoipa::path(
    delete,
    path = "/api/v1/library/packs/{id}",
    params(("id" = Uuid, Path, description = "Pack id")),
    responses(
        (status = 204, description = "Pack deleted"),
        (status = 403, description = "Caller lacks workspace Admin/Owner", body = ErrorResponse),
        (status = 404, description = "Pack not found", body = ErrorResponse),
        (status = 409, description = "A pack node is still referenced by a consumer", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "library-packs",
)]
pub async fn delete_pack(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<axum::http::StatusCode, ApiError> {
    // Load the pack scoped to the caller's workspace (NOT system-visible: a
    // delete must target an owned pack).
    let workspace_id = user
        .workspace_id
        .ok_or_else(|| ApiError::bad_request("no active workspace"))?;
    let pack: LibraryPack = sqlx::query_as(
        "SELECT id, workspace_id, vendor, slug, version, name, description, \
                origin, installed_by, installed_at \
           FROM library_packs WHERE id = $1 AND workspace_id = $2",
    )
    .bind(id)
    .bind(workspace_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("pack not found"))?;

    if pack.origin == "system" {
        return Err(ApiError::conflict(
            "system packs are seed-managed and cannot be deleted",
        ));
    }

    require_role(&state.db, &user, pack.workspace_id, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    // Collect the pack's coordinates (all latest families).
    let coords: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT coordinate FROM workflow_templates \
          WHERE pack_id = $1 AND coordinate IS NOT NULL",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    // Best-effort in-use check: any OTHER template's graph JSON referencing one
    // of these coordinates as a sub-workflow `sourceCoordinate`. The graph stores
    // node data with a `sourceCoordinate` string; a JSONB containment scan on the
    // serialized graph is the cheapest cross-template signal absent a ref table.
    for (coord,) in &coords {
        let referenced: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM workflow_templates \
              WHERE pack_id IS DISTINCT FROM $1 \
                AND graph::text LIKE '%' || $2 || '%' \
              LIMIT 1",
        )
        .bind(id)
        .bind(coord)
        .fetch_optional(&state.db)
        .await?;
        if referenced.is_some() {
            return Err(ApiError::conflict(format!(
                "library node `{coord}` is still embedded in another template; \
                 remove those references before deleting the pack"
            )));
        }
    }

    // Transactional teardown: node families (all versions, via pack_id) then the
    // pack row. The FK is ON DELETE SET NULL, but we delete the rows explicitly.
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM workflow_templates WHERE pack_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM library_packs WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    tracing::info!(
        pack_id = %id,
        coordinate = %format!("{}/{}", pack.vendor, pack.slug),
        workspace = %workspace_id,
        principal = %user.subject_as_uuid(),
        "library pack deleted"
    );

    Ok(axum::http::StatusCode::NO_CONTENT)
}
