//! Asset CRUD endpoints (docs/20 §8).
//!
//! The asset layer is **user-typed, curated, static content** — material
//! parameters, simulation scripts, reference artifacts — stored as
//! schema-validated JSONB rows (+ S3 for `File` fields) and consumed by
//! workflow nodes as ordinary staged inputs. It is a *separate* layer from
//! resources (credentials) and the catalogue (machine-produced outputs).
//!
//! This module mirrors [`crate::handlers::resources`]: runtime-bound sqlx,
//! `ApiError` for the wire, ident-grammar `ref_key`/`name` validation, soft
//! delete. The two divergences from resources:
//!
//! 1. **No secrets.** There is no Vault split — record data is plain JSONB and
//!    `File` fields hold an S3 storage path. Records validate against the asset
//!    type's `fields_json` (a `Vec<PortField>`) via the *same*
//!    `Port::json_schema` / `FieldKind` validation ports use.
//! 2. **Scope is polymorphic** (`workspace | project | template`, docs/20 §2).
//!    List endpoints resolve the downward-visible, most-specific-wins set via
//!    [`crate::scope`]; create defaults to the caller's workspace.
//!
//! Schema evolution is **additive-only** (docs/20 §4.3): a type update may add
//! optional fields or widen, but rename / remove / retype / newly-require is
//! rejected server-side.

use std::sync::LazyLock;

use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    Json,
};
use regex::Regex;
use serde_json::Value;
use uuid::Uuid;

use crate::auth::{
    apply_grant, effective_object_roles, filter_and_annotate_visible, map_to_api_error,
    require_object_role, AuthUser, ObjectKind, ObjectRef, Role,
};
use crate::models::asset::{
    AssetDetail, AssetRow, AssetSummary, AssetTypeDetail, AssetTypeRow, AssetTypeSummary,
    AssetUsageItem, AssetUsageQuery, Cardinality, CreateAssetRequest, CreateAssetTypeRequest,
    CreateScopeQuery, GetAssetQuery, ImportCsvParams, ListAssetTypesQuery, ListAssetsQuery,
    MoveScopeRequest, ReplaceRecordsRequest, ScopeKind, UpdateAssetTypeRequest,
};
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::template::{PaginatedResponse, Port, PortField};
use crate::scope::{self, Scope, ScopedItem, VisibleScopes};
use crate::AppState;

/// Flat identifier grammar shared by asset `ref_key`s and asset-type `name`s —
/// the same `^[a-z][a-z0-9_]*$` resources use for `path` (docs/20 §3: the
/// ref-key stays flat + identifier-safe; folders live in `display_path`).
static IDENT_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_]*$").expect("IDENT_REGEX must compile"));

/// MIME types accepted for asset `File`-field uploads + CSV import. Mirrors the
/// files-handler allow-list (incl. `text/csv` for the importer).
const ALLOWED_FILE_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/svg+xml",
    "application/pdf",
    "text/plain",
    "text/csv",
    "application/json",
    "application/zip",
    "application/x-tar",
    "application/gzip",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/octet-stream",
];

// ── Scope helpers ───────────────────────────────────────────────────────────

/// Caller-implicit workspace: falls back to the session workspace, then
/// `Uuid::nil()` (the seeded default workspace), matching resources.
fn caller_workspace(user: &AuthUser) -> Uuid {
    user.workspace_id.unwrap_or_else(Uuid::nil)
}

/// Parse a `?scope=` query value into a concrete binding context for
/// downward-visibility resolution. Accepts `workspace`, `workspace:<uuid>`,
/// `folder:<uuid>`, `template:<uuid>`. Bare `workspace` (or absent) resolves
/// to the caller's workspace.
fn parse_scope(user: &AuthUser, scope: Option<&str>) -> Result<(ScopeKind, Uuid), ApiError> {
    let Some(raw) = scope else {
        return Ok((ScopeKind::Workspace, caller_workspace(user)));
    };
    let raw = raw.trim();
    if raw.is_empty() || raw == "workspace" {
        return Ok((ScopeKind::Workspace, caller_workspace(user)));
    }
    let (kind_str, id_str) = raw.split_once(':').ok_or_else(|| {
        ApiError::bad_request(format!(
            "invalid scope '{raw}' — expected `workspace`, `folder:<uuid>`, or `template:<uuid>`"
        ))
    })?;
    let kind = ScopeKind::from_db(kind_str)
        .ok_or_else(|| ApiError::bad_request(format!("unknown scope kind '{kind_str}'")))?;
    if kind == ScopeKind::Workspace && id_str.is_empty() {
        return Ok((ScopeKind::Workspace, caller_workspace(user)));
    }
    let id = Uuid::parse_str(id_str)
        .map_err(|_| ApiError::bad_request(format!("scope id '{id_str}' is not a uuid")))?;
    Ok((kind, id))
}

/// Resolve the owner scope for a CREATE request. Defaults to the caller's
/// workspace; an explicit `scope_kind`/`scope_id` overrides. For an explicit
/// `workspace` scope with no id, falls back to the caller's workspace.
fn create_scope(
    user: &AuthUser,
    scope_kind: Option<ScopeKind>,
    scope_id: Option<Uuid>,
) -> Result<(ScopeKind, Uuid), ApiError> {
    let kind = scope_kind.unwrap_or(ScopeKind::Workspace);
    let id = match (kind, scope_id) {
        (ScopeKind::Workspace, None) => caller_workspace(user),
        (_, Some(id)) => id,
        (k, None) => {
            return Err(ApiError::bad_request(format!(
                "scope_id is required for scope_kind '{}'",
                k.as_db()
            )))
        }
    };
    Ok((kind, id))
}

/// Resolve the owner scope for a CREATE request, accepting it from EITHER the
/// request body (`scope_kind`/`scope_id`) OR the `?scope=` query param — the
/// same grammar the list endpoints use. Previously only the body was honored,
/// so an API caller who mirrored the list convention (`POST …?scope=folder:x`)
/// silently created a workspace-scoped item. Now: if both are given they must
/// agree (else 400); otherwise whichever is present wins, defaulting to the
/// caller's workspace.
fn resolve_create_scope(
    user: &AuthUser,
    query_scope: Option<&str>,
    body_kind: Option<ScopeKind>,
    body_id: Option<Uuid>,
) -> Result<(ScopeKind, Uuid), ApiError> {
    let body_set = body_kind.is_some() || body_id.is_some();
    let query_set = query_scope.map(|s| !s.trim().is_empty()).unwrap_or(false);

    let body = if body_set {
        Some(create_scope(user, body_kind, body_id)?)
    } else {
        None
    };
    let query = if query_set {
        Some(parse_scope(user, query_scope)?)
    } else {
        None
    };

    match (body, query) {
        (Some(b), Some(q)) if b != q => Err(ApiError::bad_request(format!(
            "scope conflict: request body specifies {}:{} but ?scope= specifies \
             {}:{} — provide only one, or make them agree",
            b.0.as_db(),
            b.1,
            q.0.as_db(),
            q.1
        ))),
        (Some(b), _) => Ok(b),
        (None, Some(q)) => Ok(q),
        (None, None) => Ok((ScopeKind::Workspace, caller_workspace(user))),
    }
}

/// Map a [`scope::IncomparableClash`] to a 409 — the binding ref is ambiguous
/// (two equally-specific scopes both define it, docs/20 §2).
fn clash_to_api_error(c: scope::IncomparableClash) -> ApiError {
    ApiError::conflict(c.to_string())
}

// ── Schema validation ───────────────────────────────────────────────────────

/// Deserialize an `asset_types.fields_json` JSONB blob into `Vec<PortField>`.
fn parse_fields(fields_json: &Value) -> Result<Vec<PortField>, ApiError> {
    serde_json::from_value::<Vec<PortField>>(fields_json.clone()).map_err(|e| {
        ApiError::internal(format!(
            "asset type schema is not a valid Vec<PortField>: {e}"
        ))
    })
}

/// Build the validating [`Port`] for an asset type's schema. Records validate
/// against `port.json_schema()` exactly as port tokens do.
fn type_port(fields: Vec<PortField>) -> Port {
    Port {
        id: "asset".to_string(),
        label: "Asset record".to_string(),
        fields,
    }
}

/// Validate a single record JSON value against the asset type's port schema,
/// reusing the same `FieldKind::accepts` + required-field checks ports use.
/// Returns a human-readable error string on the first failure for this row.
fn validate_record(port: &Port, record: &Value) -> Result<(), String> {
    let Value::Object(map) = record else {
        return Err("record must be a JSON object keyed by field name".to_string());
    };

    // Reject stray keys when the port is declared (additionalProperties:false).
    if !port.fields.is_empty() {
        let known: std::collections::HashSet<&str> =
            port.fields.iter().map(|f| f.name.as_str()).collect();
        let mut stray: Vec<&str> = map
            .keys()
            .map(String::as_str)
            .filter(|k| !known.contains(k))
            .collect();
        if !stray.is_empty() {
            stray.sort_unstable();
            return Err(format!("unknown field(s): {}", stray.join(", ")));
        }
    }

    for field in &port.fields {
        match map.get(&field.name) {
            None | Some(Value::Null) => {
                if field.required {
                    return Err(format!("missing required field '{}'", field.name));
                }
            }
            Some(value) => {
                if !field.kind.accepts(value) {
                    return Err(format!(
                        "field '{}' has the wrong type for kind {:?}",
                        field.name, field.kind
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Validate every record, collecting per-row errors. Empty `Ok(())` only when
/// all rows pass; otherwise a 422 listing `row N: <reason>` for each failure.
fn validate_records(port: &Port, records: &[Value]) -> Result<(), ApiError> {
    let mut errors: Vec<String> = Vec::new();
    for (i, rec) in records.iter().enumerate() {
        if let Err(e) = validate_record(port, rec) {
            errors.push(format!("row {i}: {e}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("record validation failed:\n{}", errors.join("\n")),
        ))
    }
}

/// Enforce additive-only schema evolution (docs/20 §4.3). The new schema may
/// **add** optional fields or **widen** a field (e.g. `Text` → `Json`,
/// `required: true` → `required: false`); it may NOT rename, remove, retype to
/// a narrower kind, or newly-require an existing field. Returns the offending
/// reason on rejection.
fn check_additive_evolution(old: &[PortField], new: &[PortField]) -> Result<(), ApiError> {
    use crate::models::template::FieldKind;

    // A kind change is "widening" only when the new kind is the opaque `Json`
    // escape hatch (accepts everything the old kind did). Any other retype is
    // a breaking change.
    fn is_widening(old: FieldKind, new: FieldKind) -> bool {
        old == new || new == FieldKind::Json
    }

    let new_by_name: std::collections::HashMap<&str, &PortField> =
        new.iter().map(|f| (f.name.as_str(), f)).collect();

    let mut breaks: Vec<String> = Vec::new();
    for of in old {
        match new_by_name.get(of.name.as_str()) {
            None => breaks.push(format!("field '{}' was removed (or renamed)", of.name)),
            Some(nf) => {
                if !is_widening(of.kind, nf.kind) {
                    breaks.push(format!(
                        "field '{}' was retyped {:?} → {:?} (not a widening)",
                        of.name, of.kind, nf.kind
                    ));
                }
                if nf.required && !of.required {
                    breaks.push(format!(
                        "field '{}' was made required (existing rows may omit it)",
                        of.name
                    ));
                }
            }
        }
    }
    // Newly-added fields must be optional.
    let old_names: std::collections::HashSet<&str> = old.iter().map(|f| f.name.as_str()).collect();
    for nf in new {
        if !old_names.contains(nf.name.as_str()) && nf.required {
            breaks.push(format!(
                "new field '{}' is required (existing rows would become invalid)",
                nf.name
            ));
        }
    }

    if breaks.is_empty() {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "schema change is not additive-only (docs/20 §4.3) — clone to a new type instead:\n{}",
                breaks.join("\n")
            ),
        ))
    }
}

// ═════════════════════════════ ASSET TYPES ══════════════════════════════════

/// `GET /api/v1/asset-types` — scope-resolved, folder-aware list.
#[utoipa::path(
    get,
    path = "/api/v1/asset-types",
    params(ListAssetTypesQuery),
    responses(
        (status = 200, description = "Visible asset types (most-specific-wins)", body = PaginatedResponse<AssetTypeSummary>),
        (status = 400, description = "Bad scope", body = ErrorResponse),
        (status = 409, description = "Ambiguous ref-key across incomparable scopes", body = ErrorResponse),
    ),
    tag = "assets",
)]
pub async fn list_asset_types(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListAssetTypesQuery>,
) -> Result<Json<PaginatedResponse<AssetTypeSummary>>, ApiError> {
    let (kind, scope_id) = parse_scope(&user, params.scope.as_deref())?;

    // Exact placement filter (management browser): only types owned by this one
    // scope. Otherwise the downward-visible, most-specific-wins set (node picker).
    let mut summaries: Vec<AssetTypeSummary> = if params.exact == Some(true) {
        fetch_exact_asset_type_rows(&state.db, kind, scope_id)
            .await?
            .into_iter()
            .map(AssetTypeSummary::from)
            .filter(|s| folder_matches(s.display_path.as_deref(), params.folder.as_deref()))
            .collect()
    } else {
        let visible = scope::visible_scopes_for(&state.db, kind, scope_id).await?;
        let rows = fetch_visible_asset_type_rows(&state.db, &visible).await?;
        let items: Vec<ScopedItem<AssetTypeRow>> = rows
            .into_iter()
            .filter_map(|r| {
                let scope = row_scope(&r.scope_kind, r.scope_id)?;
                Some(ScopedItem {
                    scope,
                    ref_key: r.name.clone(),
                    item: r,
                })
            })
            .collect();
        let resolved = scope::resolve_visible(&visible, items).map_err(clash_to_api_error)?;
        resolved
            .into_values()
            .map(|si| AssetTypeSummary::from(si.item))
            .filter(|s| folder_matches(s.display_path.as_deref(), params.folder.as_deref()))
            .collect()
    };
    summaries.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Json(paginate(summaries, params.page, params.per_page)))
}

/// `POST /api/v1/asset-types` — define a new schema. Editor-or-above gated.
#[utoipa::path(
    post,
    path = "/api/v1/asset-types",
    params(CreateScopeQuery),
    request_body = CreateAssetTypeRequest,
    responses(
        (status = 201, description = "Asset type created", body = AssetTypeDetail),
        (status = 400, description = "Validation failure", body = ErrorResponse),
        (status = 403, description = "Editor role required", body = ErrorResponse),
        (status = 409, description = "Name already exists in scope", body = ErrorResponse),
    ),
    tag = "assets",
)]
pub async fn create_asset_type(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope_q): Query<CreateScopeQuery>,
    Json(req): Json<CreateAssetTypeRequest>,
) -> Result<(StatusCode, Json<AssetTypeDetail>), ApiError> {
    let (scope_kind, scope_id) = resolve_create_scope(
        &user,
        scope_q.scope.as_deref(),
        req.scope_kind,
        req.scope_id,
    )?;
    require_editor(&state, &user, scope_kind, scope_id).await?;
    let principal = user.subject_as_uuid();
    let detail = create_asset_type_internal(&state, &req, scope_kind, scope_id, principal).await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

/// Create an asset type at an ALREADY-resolved scope, bypassing the HTTP/auth
/// layer. Shared by the `create_asset_type` handler (after `create_scope` +
/// `require_editor`) and the demo seeder (`demos::seed_demo_assets`, which
/// passes the demo workspace scope + seeder principal). Validates the ref-key
/// identifier + the schema fields exactly as the handler did.
pub(crate) async fn create_asset_type_internal(
    state: &AppState,
    req: &CreateAssetTypeRequest,
    scope_kind: ScopeKind,
    scope_id: Uuid,
    principal: Uuid,
) -> Result<AssetTypeDetail, ApiError> {
    if !IDENT_REGEX.is_match(&req.name) {
        return Err(ApiError::bad_request(format!(
            "name '{}' must be a snake_case identifier (e.g. `steel_grade`): \
             lowercase letter first, then letters / digits / underscores.",
            req.name
        )));
    }
    validate_schema_fields(&req.fields)?;

    let display_name = req
        .display_name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| req.name.clone());
    let fields_json = serde_json::to_value(&req.fields)
        .map_err(|e| ApiError::internal(format!("schema serialize: {e}")))?;
    let id = Uuid::new_v4();

    let res = sqlx::query(
        "INSERT INTO asset_types \
            (id, scope_kind, scope_id, name, display_name, display_path, \
             fields_json, cardinality, version, created_by, updated_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 1, $9, $9)",
    )
    .bind(id)
    .bind(scope_kind.as_db())
    .bind(scope_id)
    .bind(&req.name)
    .bind(&display_name)
    .bind(&req.display_path)
    .bind(&fields_json)
    .bind(req.cardinality.as_db())
    .bind(principal)
    .execute(&state.db)
    .await;
    if let Err(e) = res {
        if let Some(db_err) = e.as_database_error() {
            if db_err.is_unique_violation() {
                return Err(ApiError::conflict(format!(
                    "asset type name '{}' already exists in this scope",
                    req.name
                )));
            }
        }
        return Err(ApiError::internal(e.to_string()));
    }

    Ok(AssetTypeDetail {
        id,
        scope_kind: scope_kind.as_db().to_string(),
        scope_id,
        name: req.name.clone(),
        display_name,
        display_path: req.display_path.clone(),
        fields: req.fields.clone(),
        cardinality: req.cardinality.as_db().to_string(),
        version: 1,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        created_by: Some(principal),
        updated_by: Some(principal),
    })
}

/// `GET /api/v1/asset-types/{id}` — full schema view.
#[utoipa::path(
    get,
    path = "/api/v1/asset-types/{id}",
    params(("id" = Uuid, Path, description = "Asset type id")),
    responses(
        (status = 200, description = "Asset type detail (incl. fields)", body = AssetTypeDetail),
        (status = 404, description = "Not found", body = ErrorResponse),
    ),
    tag = "assets",
)]
pub async fn get_asset_type(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<AssetTypeDetail>, ApiError> {
    let row = fetch_asset_type(&state.db, id).await?;
    Ok(Json(asset_type_detail(row)?))
}

/// `PUT /api/v1/asset-types/{id}` — additive-only schema update (docs/20 §4.3).
/// A `fields` change bumps `version` only after passing the additive gate.
#[utoipa::path(
    put,
    path = "/api/v1/asset-types/{id}",
    params(("id" = Uuid, Path, description = "Asset type id")),
    request_body = UpdateAssetTypeRequest,
    responses(
        (status = 200, description = "Asset type updated", body = AssetTypeDetail),
        (status = 403, description = "Editor role required", body = ErrorResponse),
        (status = 404, description = "Not found", body = ErrorResponse),
        (status = 422, description = "Non-additive schema change", body = ErrorResponse),
    ),
    tag = "assets",
)]
pub async fn update_asset_type(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateAssetTypeRequest>,
) -> Result<Json<AssetTypeDetail>, ApiError> {
    let row = fetch_asset_type(&state.db, id).await?;
    let scope_kind = ScopeKind::from_db(&row.scope_kind)
        .ok_or_else(|| ApiError::internal("asset type has invalid scope_kind"))?;
    require_editor(&state, &user, scope_kind, row.scope_id).await?;

    if req.display_name.is_none() && req.display_path.is_none() && req.fields.is_none() {
        return Err(ApiError::bad_request(
            "update body must set at least one of `display_name`, `display_path`, or `fields`",
        ));
    }

    let mut version = row.version;
    let principal = user.subject_as_uuid();

    if let Some(name) = req.display_name {
        let trimmed = name.trim().to_string();
        if trimmed.is_empty() {
            return Err(ApiError::bad_request("display_name cannot be empty"));
        }
        sqlx::query(
            "UPDATE asset_types SET display_name = $1, updated_at = NOW(), updated_by = $2 \
             WHERE id = $3",
        )
        .bind(&trimmed)
        .bind(principal)
        .bind(id)
        .execute(&state.db)
        .await?;
    }

    if let Some(dp) = req.display_path {
        sqlx::query(
            "UPDATE asset_types SET display_path = $1, updated_at = NOW(), updated_by = $2 \
             WHERE id = $3",
        )
        .bind(&dp)
        .bind(principal)
        .bind(id)
        .execute(&state.db)
        .await?;
    }

    if let Some(new_fields) = req.fields {
        validate_schema_fields(&new_fields)?;
        let old_fields = parse_fields(&row.fields_json)?;
        check_additive_evolution(&old_fields, &new_fields)?;

        version = row.version + 1;
        let fields_json = serde_json::to_value(&new_fields)
            .map_err(|e| ApiError::internal(format!("schema serialize: {e}")))?;
        sqlx::query(
            "UPDATE asset_types SET fields_json = $1, version = $2, updated_at = NOW(), \
                 updated_by = $3 \
             WHERE id = $4",
        )
        .bind(&fields_json)
        .bind(version)
        .bind(principal)
        .bind(id)
        .execute(&state.db)
        .await?;
    }

    let fresh = fetch_asset_type(&state.db, id).await?;
    let mut detail = asset_type_detail(fresh)?;
    detail.version = version;
    Ok(Json(detail))
}

/// `DELETE /api/v1/asset-types/{id}` — soft delete. Rejected when any live
/// asset still references the type (cascade-guard, docs/20 §8).
#[utoipa::path(
    delete,
    path = "/api/v1/asset-types/{id}",
    params(("id" = Uuid, Path, description = "Asset type id")),
    responses(
        (status = 204, description = "Asset type soft-deleted"),
        (status = 403, description = "Editor role required", body = ErrorResponse),
        (status = 404, description = "Not found", body = ErrorResponse),
        (status = 409, description = "Type still has assets", body = ErrorResponse),
    ),
    tag = "assets",
)]
pub async fn delete_asset_type(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let row = fetch_asset_type(&state.db, id).await?;
    let scope_kind = ScopeKind::from_db(&row.scope_kind)
        .ok_or_else(|| ApiError::internal("asset type has invalid scope_kind"))?;
    require_editor(&state, &user, scope_kind, row.scope_id).await?;

    let live_assets: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM assets WHERE type_id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
    if live_assets > 0 {
        return Err(ApiError::conflict(format!(
            "asset type still has {live_assets} live asset(s) — delete them first"
        )));
    }

    sqlx::query(
        "UPDATE asset_types SET deleted_at = NOW(), updated_at = NOW(), updated_by = $1 \
         WHERE id = $2",
    )
    .bind(user.subject_as_uuid())
    .bind(id)
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PATCH /api/v1/asset-types/{id}/scope` — reparent a type to a different owner
/// scope (docs/20 §2). Editor-gated on BOTH the current scope and the target,
/// matching `create_asset_type`'s membership-floor placement gate. The type's
/// `name` must be free in the target scope (else 409). Existing assets reference
/// the type by id, so they are unaffected by the move.
#[utoipa::path(
    patch,
    path = "/api/v1/asset-types/{id}/scope",
    params(("id" = Uuid, Path, description = "Asset type id")),
    request_body = MoveScopeRequest,
    responses(
        (status = 200, description = "Asset type moved", body = AssetTypeDetail),
        (status = 403, description = "Editor role required on source or target", body = ErrorResponse),
        (status = 404, description = "Not found", body = ErrorResponse),
        (status = 409, description = "Name already exists in target scope", body = ErrorResponse),
    ),
    tag = "assets",
)]
pub async fn move_asset_type(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<MoveScopeRequest>,
) -> Result<Json<AssetTypeDetail>, ApiError> {
    let row = fetch_asset_type(&state.db, id).await?;
    let src_kind = ScopeKind::from_db(&row.scope_kind)
        .ok_or_else(|| ApiError::internal("asset type has invalid scope_kind"))?;
    // Source gate: Editor on the type's current scope.
    require_editor(&state, &user, src_kind, row.scope_id).await?;
    // Target gate: Editor on the destination (same membership floor as create).
    let (scope_kind, scope_id) = create_scope(&user, Some(req.scope_kind), req.scope_id)?;
    require_editor(&state, &user, scope_kind, scope_id).await?;

    if (scope_kind, scope_id) != (src_kind, row.scope_id) {
        let res = sqlx::query(
            "UPDATE asset_types SET scope_kind = $1, scope_id = $2, updated_at = NOW(), \
                 updated_by = $3 \
             WHERE id = $4 AND deleted_at IS NULL",
        )
        .bind(scope_kind.as_db())
        .bind(scope_id)
        .bind(user.subject_as_uuid())
        .bind(id)
        .execute(&state.db)
        .await;
        if let Err(e) = res {
            if let Some(db_err) = e.as_database_error() {
                if db_err.is_unique_violation() {
                    return Err(ApiError::conflict(format!(
                        "asset type name '{}' already exists in the target scope",
                        row.name
                    )));
                }
            }
            return Err(ApiError::internal(e.to_string()));
        }
    }

    let fresh = fetch_asset_type(&state.db, id).await?;
    Ok(Json(asset_type_detail(fresh)?))
}

// ═════════════════════════════ ASSETS ═══════════════════════════════════════

/// `GET /api/v1/assets?type_id=&scope=&folder=` — scope-resolved list.
#[utoipa::path(
    get,
    path = "/api/v1/assets",
    params(ListAssetsQuery),
    responses(
        (status = 200, description = "Visible assets (most-specific-wins)", body = PaginatedResponse<AssetSummary>),
        (status = 400, description = "Bad scope", body = ErrorResponse),
        (status = 409, description = "Ambiguous ref-key across incomparable scopes", body = ErrorResponse),
    ),
    tag = "assets",
)]
pub async fn list_assets(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListAssetsQuery>,
) -> Result<Json<PaginatedResponse<AssetSummary>>, ApiError> {
    let (kind, scope_id) = parse_scope(&user, params.scope.as_deref())?;

    // Exact placement filter (management browser) vs. downward-visible set.
    let (mut summaries, workspace_id): (Vec<AssetSummary>, Uuid) = if params.exact == Some(true) {
        let summaries = fetch_exact_asset_rows(&state.db, kind, scope_id, params.type_id)
            .await?
            .into_iter()
            .map(AssetSummary::from)
            .filter(|s| folder_matches(s.display_path.as_deref(), params.folder.as_deref()))
            .collect();
        (summaries, caller_workspace(&user))
    } else {
        let visible = scope::visible_scopes_for(&state.db, kind, scope_id).await?;
        let rows = fetch_visible_asset_rows(&state.db, &visible, params.type_id).await?;
        let items: Vec<ScopedItem<AssetRow>> = rows
            .into_iter()
            .filter_map(|r| {
                let scope = row_scope(&r.scope_kind, r.scope_id)?;
                Some(ScopedItem {
                    scope,
                    ref_key: r.ref_key.clone(),
                    item: r,
                })
            })
            .collect();
        let resolved = scope::resolve_visible(&visible, items).map_err(clash_to_api_error)?;
        let summaries = resolved
            .into_values()
            .map(|si| AssetSummary::from(si.item))
            .filter(|s| folder_matches(s.display_path.as_deref(), params.folder.as_deref()))
            .collect();
        (summaries, visible.workspace.unwrap_or(scope_id))
    };
    summaries.sort_by(|a, b| a.ref_key.cmp(&b.ref_key));

    // Object-ACL: stamp the caller's effective role and DROP assets they can't
    // reach (a restricted asset with no grant is absent from the role map).
    filter_and_annotate_visible(
        &state.db,
        &user,
        ObjectKind::Asset,
        workspace_id,
        &mut summaries,
    )
    .await
    .map_err(map_to_api_error)?;

    Ok(Json(paginate(summaries, params.page, params.per_page)))
}

/// `POST /api/v1/assets` — create an empty asset of a given type. Records are
/// written separately via `PUT /records` or `POST /import-csv`.
#[utoipa::path(
    post,
    path = "/api/v1/assets",
    params(CreateScopeQuery),
    request_body = CreateAssetRequest,
    responses(
        (status = 201, description = "Asset created", body = AssetSummary),
        (status = 400, description = "Validation failure", body = ErrorResponse),
        (status = 403, description = "Editor role required", body = ErrorResponse),
        (status = 404, description = "Asset type not found", body = ErrorResponse),
        (status = 409, description = "ref_key already exists in scope", body = ErrorResponse),
    ),
    tag = "assets",
)]
pub async fn create_asset(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope_q): Query<CreateScopeQuery>,
    Json(req): Json<CreateAssetRequest>,
) -> Result<(StatusCode, Json<AssetSummary>), ApiError> {
    let (scope_kind, scope_id) = resolve_create_scope(
        &user,
        scope_q.scope.as_deref(),
        req.scope_kind,
        req.scope_id,
    )?;
    // Placement gate: Editor on the scope you create into. Workspace scope uses
    // the membership gate; folder/template scopes use the object ACL (so a
    // restricted folder's grants govern who can add assets to it).
    match scope_kind {
        ScopeKind::Workspace => require_editor(&state, &user, scope_kind, scope_id).await?,
        ScopeKind::Folder => {
            require_object_role(&state.db, &user, ObjectRef::folder(scope_id), Role::Editor)
                .await
                .map_err(map_to_api_error)?;
        }
        ScopeKind::Template => {
            require_object_role(
                &state.db,
                &user,
                ObjectRef::template(scope_id),
                Role::Editor,
            )
            .await
            .map_err(map_to_api_error)?;
        }
    }
    let principal = user.subject_as_uuid();
    let mut summary = create_asset_internal(&state, &req, scope_kind, scope_id, principal).await?;
    // Creator owns it (apply_grant in the core flow); stamp for immediate gating.
    summary.my_effective_role = Some(Role::Owner.as_label().to_string());
    Ok((StatusCode::CREATED, Json(summary)))
}

/// Create an asset at an ALREADY-resolved scope, bypassing the HTTP/auth layer.
/// Shared by the `create_asset` handler and the demo seeder. Validates the
/// ref-key + that the referenced type exists.
pub(crate) async fn create_asset_internal(
    state: &AppState,
    req: &CreateAssetRequest,
    scope_kind: ScopeKind,
    scope_id: Uuid,
    principal: Uuid,
) -> Result<AssetSummary, ApiError> {
    if !IDENT_REGEX.is_match(&req.ref_key) {
        return Err(ApiError::bad_request(format!(
            "ref_key '{}' must be a snake_case identifier (e.g. `steel`): \
             lowercase letter first, then letters / digits / underscores.",
            req.ref_key
        )));
    }

    // Type must exist + be live.
    let type_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM asset_types WHERE id = $1 AND deleted_at IS NULL)",
    )
    .bind(req.type_id)
    .fetch_one(&state.db)
    .await?;
    if !type_exists {
        return Err(ApiError::not_found("asset type not found"));
    }

    let display_name = req
        .display_name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| req.ref_key.clone());
    let id = Uuid::new_v4();

    let restricted = req.restricted.unwrap_or(false);
    let res = sqlx::query(
        "INSERT INTO assets \
            (id, scope_kind, scope_id, type_id, ref_key, display_name, display_path, \
             version, created_by, updated_by, restricted) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8, $8, $9)",
    )
    .bind(id)
    .bind(scope_kind.as_db())
    .bind(scope_id)
    .bind(req.type_id)
    .bind(&req.ref_key)
    .bind(&display_name)
    .bind(&req.display_path)
    .bind(principal)
    .bind(restricted)
    .execute(&state.db)
    .await;
    if let Err(e) = res {
        if let Some(db_err) = e.as_database_error() {
            if db_err.is_unique_violation() {
                return Err(ApiError::conflict(format!(
                    "asset ref_key '{}' already exists in this scope",
                    req.ref_key
                )));
            }
        }
        return Err(ApiError::internal(e.to_string()));
    }

    // Object-ACL: the creator owns the asset. Essential for a `restricted` asset
    // (no ws floor → without this the creator couldn't read back what they made).
    // workspace_id for the grant row is resolved from the scope.
    let grant_ws: Uuid = match scope_kind {
        ScopeKind::Workspace => scope_id,
        ScopeKind::Folder => {
            sqlx::query_scalar("SELECT workspace_id FROM folders WHERE id = $1")
                .bind(scope_id)
                .fetch_one(&state.db)
                .await?
        }
        ScopeKind::Template => {
            sqlx::query_scalar("SELECT workspace_id FROM workflow_templates WHERE id = $1")
                .bind(scope_id)
                .fetch_one(&state.db)
                .await?
        }
    };
    let _ = apply_grant(
        &state.db,
        grant_ws,
        ObjectKind::Asset,
        id,
        principal,
        Role::Owner,
        principal,
    )
    .await;

    Ok(AssetSummary {
        id,
        scope_kind: scope_kind.as_db().to_string(),
        scope_id,
        type_id: req.type_id,
        ref_key: req.ref_key.clone(),
        display_name,
        display_path: req.display_path.clone(),
        version: 1,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        created_by: Some(principal),
        updated_by: Some(principal),
        my_effective_role: None,
        restricted,
    })
}

/// Replace an asset's records (validate against the type schema, bump version),
/// bypassing the HTTP/auth layer. Shared by `put_asset_records` and the seeder.
pub(crate) async fn replace_records_internal(
    state: &AppState,
    asset_id: Uuid,
    records: &[Value],
) -> Result<i32, ApiError> {
    let row = fetch_asset(&state.db, asset_id).await?;
    write_records(state, &row, records, false, None).await
}

/// `GET /api/v1/assets/{id}` — metadata + a page of the current-version records.
#[utoipa::path(
    get,
    path = "/api/v1/assets/{id}",
    params(
        ("id" = Uuid, Path, description = "Asset id"),
        GetAssetQuery,
    ),
    responses(
        (status = 200, description = "Asset detail + paged records", body = AssetDetail),
        (status = 404, description = "Not found", body = ErrorResponse),
    ),
    tag = "assets",
)]
pub async fn get_asset(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(params): Query<GetAssetQuery>,
) -> Result<Json<AssetDetail>, ApiError> {
    let row = fetch_asset(&state.db, id).await?;
    let role = require_object_role(&state.db, &user, ObjectRef::asset(id), Role::Viewer)
        .await
        .map_err(map_to_api_error)?;
    let offset = (params.page - 1).max(0) * params.per_page;

    let records: Vec<(Value,)> = sqlx::query_as(
        "SELECT data FROM asset_records \
         WHERE asset_id = $1 AND version = $2 \
         ORDER BY row_idx ASC LIMIT $3 OFFSET $4",
    )
    .bind(id)
    .bind(row.version)
    .bind(params.per_page)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let record_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM asset_records WHERE asset_id = $1 AND version = $2",
    )
    .bind(id)
    .bind(row.version)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(AssetDetail {
        id: row.id,
        scope_kind: row.scope_kind,
        scope_id: row.scope_id,
        type_id: row.type_id,
        ref_key: row.ref_key,
        display_name: row.display_name,
        display_path: row.display_path,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
        created_by: row.created_by,
        updated_by: row.updated_by,
        records: records.into_iter().map(|(v,)| v).collect(),
        record_count,
        my_effective_role: Some(role.as_label().to_string()),
        restricted: row.restricted,
    }))
}

/// `PUT /api/v1/assets/{id}/records` — replace (or append) records. Validates
/// every row against the type schema, then writes a new version atomically.
#[utoipa::path(
    put,
    path = "/api/v1/assets/{id}/records",
    params(("id" = Uuid, Path, description = "Asset id")),
    request_body = ReplaceRecordsRequest,
    responses(
        (status = 200, description = "Records written; version bumped", body = AssetSummary),
        (status = 403, description = "Editor role required", body = ErrorResponse),
        (status = 404, description = "Not found", body = ErrorResponse),
        (status = 422, description = "Record validation failed", body = ErrorResponse),
    ),
    tag = "assets",
)]
pub async fn put_asset_records(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<ReplaceRecordsRequest>,
) -> Result<Json<AssetSummary>, ApiError> {
    let row = fetch_asset(&state.db, id).await?;
    let role = require_object_role(&state.db, &user, ObjectRef::asset(id), Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    let principal = user.subject_as_uuid();
    let new_version =
        write_records(&state, &row, &req.records, req.append, Some(principal)).await?;

    Ok(Json(AssetSummary {
        id: row.id,
        scope_kind: row.scope_kind,
        scope_id: row.scope_id,
        type_id: row.type_id,
        ref_key: row.ref_key,
        display_name: row.display_name,
        display_path: row.display_path,
        version: new_version,
        created_at: row.created_at,
        updated_at: chrono::Utc::now(),
        created_by: row.created_by,
        updated_by: Some(principal),
        my_effective_role: Some(role.as_label().to_string()),
        restricted: row.restricted,
    }))
}

/// `POST /api/v1/assets/{id}/import-csv` — parse a CSV multipart body, map
/// columns to the type's fields, coerce per `FieldKind`, validate, and write a
/// new version (replace or append per `?append=`).
#[utoipa::path(
    post,
    path = "/api/v1/assets/{id}/import-csv",
    params(
        ("id" = Uuid, Path, description = "Asset id"),
        ImportCsvParams,
    ),
    request_body(content = CsvImportBody, content_type = "multipart/form-data"),
    responses(
        (status = 200, description = "CSV imported; version bumped", body = AssetSummary),
        (status = 400, description = "Bad CSV / multipart", body = ErrorResponse),
        (status = 403, description = "Editor role required", body = ErrorResponse),
        (status = 404, description = "Not found", body = ErrorResponse),
        (status = 422, description = "Record validation failed", body = ErrorResponse),
    ),
    tag = "assets",
)]
pub async fn import_asset_csv(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(params): Query<ImportCsvParams>,
    mut multipart: Multipart,
) -> Result<Json<AssetSummary>, ApiError> {
    let row = fetch_asset(&state.db, id).await?;
    let role = require_object_role(&state.db, &user, ObjectRef::asset(id), Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    // Pull the CSV bytes out of the multipart body.
    let field = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("invalid multipart: {e}")))?
        .ok_or_else(|| ApiError::bad_request("no file field in multipart body"))?;
    let bytes = field
        .bytes()
        .await
        .map_err(|e| ApiError::bad_request(format!("failed to read csv: {e}")))?;

    let type_row = fetch_asset_type(&state.db, row.type_id).await?;
    let fields = parse_fields(&type_row.fields_json)?;

    let records = parse_csv(&bytes, &fields, params.has_header)?;
    let principal = user.subject_as_uuid();
    let new_version = write_records(&state, &row, &records, params.append, Some(principal)).await?;

    Ok(Json(AssetSummary {
        id: row.id,
        scope_kind: row.scope_kind,
        scope_id: row.scope_id,
        type_id: row.type_id,
        ref_key: row.ref_key,
        display_name: row.display_name,
        display_path: row.display_path,
        version: new_version,
        created_at: row.created_at,
        updated_at: chrono::Utc::now(),
        created_by: row.created_by,
        updated_by: Some(principal),
        my_effective_role: Some(role.as_label().to_string()),
        restricted: row.restricted,
    }))
}

/// `POST /api/v1/assets/{id}/files` — upload a file for a `File` field → S3,
/// returns the storage path to embed in a record's JSONB (docs/20 §4.1). The
/// dual-source File model also accepts a catalogue-entry `storage_path` as a
/// bare string; that path is reused verbatim (no copy) so this endpoint is only
/// for fresh uploads.
#[utoipa::path(
    post,
    path = "/api/v1/assets/{id}/files",
    params(
        ("id" = Uuid, Path, description = "Asset id"),
        ("field" = String, Query, description = "The File field name this upload is for"),
    ),
    request_body(content = AssetFileUpload, content_type = "multipart/form-data"),
    responses(
        (status = 201, description = "File uploaded; returns storage path", body = AssetFileUploadResponse),
        (status = 400, description = "Bad multipart / unsupported content type", body = ErrorResponse),
        (status = 403, description = "Editor role required", body = ErrorResponse),
        (status = 404, description = "Not found", body = ErrorResponse),
    ),
    tag = "assets",
)]
pub async fn upload_asset_file(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<UploadFileQuery>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<AssetFileUploadResponse>), ApiError> {
    let row = fetch_asset(&state.db, id).await?;
    require_object_role(&state.db, &user, ObjectRef::asset(id), Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    if !IDENT_REGEX.is_match(&q.field) {
        return Err(ApiError::bad_request(format!(
            "field '{}' is not a valid field name",
            q.field
        )));
    }

    let field = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("invalid multipart: {e}")))?
        .ok_or_else(|| ApiError::bad_request("no file field in multipart body"))?;

    let content_type = field
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_string();
    if !ALLOWED_FILE_TYPES.contains(&content_type.as_str()) {
        return Err(ApiError::bad_request(format!(
            "unsupported content type: {content_type}. Allowed: {ALLOWED_FILE_TYPES:?}"
        )));
    }
    let filename = field.file_name().unwrap_or("upload.bin").to_string();
    let bytes = field
        .bytes()
        .await
        .map_err(|e| ApiError::bad_request(format!("failed to read file: {e}")))?;

    // Key is pinned at the asset's CURRENT version so an instance launched
    // against this version keeps resolving the object after later edits.
    let key = state
        .s3
        .upload_asset_file(id, row.version, &q.field, &filename, &bytes, &content_type)
        .await
        .map_err(|e| ApiError::internal(format!("upload failed: {e}")))?;

    Ok((
        StatusCode::CREATED,
        Json(AssetFileUploadResponse {
            storage_path: key,
            filename,
            content_type,
            size: bytes.len(),
        }),
    ))
}

/// `DELETE /api/v1/assets/{id}` — soft delete. Records stay (CASCADE only on
/// hard delete) so already-pinned instances keep resolving.
#[utoipa::path(
    delete,
    path = "/api/v1/assets/{id}",
    params(("id" = Uuid, Path, description = "Asset id")),
    responses(
        (status = 204, description = "Asset soft-deleted"),
        (status = 403, description = "Editor role required", body = ErrorResponse),
        (status = 404, description = "Not found", body = ErrorResponse),
    ),
    tag = "assets",
)]
pub async fn delete_asset(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let _row = fetch_asset(&state.db, id).await?;
    require_object_role(&state.db, &user, ObjectRef::asset(id), Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    sqlx::query(
        "UPDATE assets SET deleted_at = NOW(), updated_at = NOW(), updated_by = $1 WHERE id = $2",
    )
    .bind(user.subject_as_uuid())
    .bind(id)
    .execute(&state.db)
    .await?;

    // Object grants are polymorphic with no FK — drop them on delete.
    sqlx::query(
        "DELETE FROM object_grants WHERE object_type = 'asset'::object_kind AND object_id = $1",
    )
    .bind(id)
    .execute(&state.db)
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// `PATCH /api/v1/assets/{id}/scope` — reparent an asset to a different owner
/// scope (docs/20 §2). Re-authorizes on BOTH sides: Editor on the asset itself
/// (object ACL) to move it OUT, and the `create_asset` placement gate on the
/// target to drop it IN. The asset's own grants (incl. the creator's Owner
/// grant) ride along — only the inheritance parent changes. `ref_key` must be
/// free in the target scope (else 409).
#[utoipa::path(
    patch,
    path = "/api/v1/assets/{id}/scope",
    params(("id" = Uuid, Path, description = "Asset id")),
    request_body = MoveScopeRequest,
    responses(
        (status = 200, description = "Asset moved", body = AssetSummary),
        (status = 403, description = "Editor role required on source or target", body = ErrorResponse),
        (status = 404, description = "Not found", body = ErrorResponse),
        (status = 409, description = "ref_key already exists in target scope", body = ErrorResponse),
    ),
    tag = "assets",
)]
pub async fn move_asset(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<MoveScopeRequest>,
) -> Result<Json<AssetSummary>, ApiError> {
    let row = fetch_asset(&state.db, id).await?;
    // Source gate: Editor on the asset (object ACL) to move it out.
    require_object_role(&state.db, &user, ObjectRef::asset(id), Role::Editor)
        .await
        .map_err(map_to_api_error)?;
    // Target gate: Editor on the destination scope (create_asset placement rule).
    let (scope_kind, scope_id) = create_scope(&user, Some(req.scope_kind), req.scope_id)?;
    require_asset_placement(&state, &user, scope_kind, scope_id).await?;

    if (scope_kind, scope_id)
        != (
            ScopeKind::from_db(&row.scope_kind).unwrap_or(ScopeKind::Workspace),
            row.scope_id,
        )
    {
        let res = sqlx::query(
            "UPDATE assets SET scope_kind = $1, scope_id = $2, updated_at = NOW(), \
                 updated_by = $3 \
             WHERE id = $4 AND deleted_at IS NULL",
        )
        .bind(scope_kind.as_db())
        .bind(scope_id)
        .bind(user.subject_as_uuid())
        .bind(id)
        .execute(&state.db)
        .await;
        if let Err(e) = res {
            if let Some(db_err) = e.as_database_error() {
                if db_err.is_unique_violation() {
                    return Err(ApiError::conflict(format!(
                        "asset ref_key '{}' already exists in the target scope",
                        row.ref_key
                    )));
                }
            }
            return Err(ApiError::internal(e.to_string()));
        }
    }

    // Re-stamp the caller's effective role at the new scope (non-fatal: a move
    // into a restricted container the caller can't read yields None, like list).
    let fresh = fetch_asset(&state.db, id).await?;
    let mut summary = AssetSummary::from(fresh);
    let roles = effective_object_roles(
        &state.db,
        &user,
        ObjectKind::Asset,
        caller_workspace(&user),
        &[id],
    )
    .await
    .map_err(map_to_api_error)?;
    summary.my_effective_role = roles.get(&id).map(|r| r.as_label().to_string());
    Ok(Json(summary))
}

/// Internal `FromRow` for the usage query — the instance columns plus the raw
/// `asset_pins` map, from which we extract the matching alias + version.
#[derive(sqlx::FromRow)]
struct UsageRow {
    id: Uuid,
    template_id: Uuid,
    template_name: String,
    template_version: i32,
    status: String,
    mode: String,
    created_at: chrono::DateTime<chrono::Utc>,
    asset_pins: Value,
}

/// `GET /api/v1/assets/{id}/usage` — **reverse lineage** (docs/20 §9): every run
/// (workflow instance) that pinned this asset, newest first. Answers "which runs
/// used asset X" straight from `workflow_instances.asset_pins` (GIN-indexed
/// jsonpath). Record/material-level lineage ("runs that used Copper C110") is a
/// deferred follow-on — see docs/20 §9.
#[utoipa::path(
    get,
    path = "/api/v1/assets/{id}/usage",
    params(
        ("id" = Uuid, Path, description = "Asset id"),
        AssetUsageQuery,
    ),
    responses(
        (status = 200, description = "Runs that used this asset", body = PaginatedResponse<AssetUsageItem>),
        (status = 404, description = "Asset not found", body = ErrorResponse),
    ),
    tag = "assets",
)]
pub async fn asset_usage(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(params): Query<AssetUsageQuery>,
) -> Result<Json<PaginatedResponse<AssetUsageItem>>, ApiError> {
    // 404 if the asset doesn't exist.
    let _ = fetch_asset(&state.db, id).await?;
    require_object_role(&state.db, &user, ObjectRef::asset(id), Role::Viewer)
        .await
        .map_err(map_to_api_error)?;

    // Match any alias entry whose `asset_id` equals this asset. `id` is a `Uuid`
    // so interpolating it into the jsonpath literal is injection-safe.
    let filter = format!("$.* ? (@.asset_id == \"{id}\")");
    let offset = (params.page - 1).max(0) * params.per_page;

    let rows: Vec<UsageRow> = sqlx::query_as::<_, UsageRow>(
        "SELECT wi.id, wi.template_id, wt.name AS template_name, wi.template_version, \
                wi.status, wi.mode, wi.created_at, wi.asset_pins \
         FROM workflow_instances wi \
         JOIN workflow_templates wt \
           ON wt.id = wi.template_id AND wt.version = wi.template_version \
         WHERE wi.asset_pins @? $1::jsonpath \
         ORDER BY wi.created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(&filter)
    .bind(params.per_page)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let total: i64 = sqlx::query_as::<_, (i64,)>(
        "SELECT COUNT(*) FROM workflow_instances WHERE asset_pins @? $1::jsonpath",
    )
    .bind(&filter)
    .fetch_one(&state.db)
    .await?
    .0;

    let target = id.to_string();
    let items = rows
        .into_iter()
        .map(|r| {
            // Pull the alias + version under which this run pinned the asset.
            let (alias, version_used) = r
                .asset_pins
                .as_object()
                .and_then(|m| {
                    m.iter().find_map(|(alias, pin)| {
                        let aid = pin.get("asset_id").and_then(Value::as_str)?;
                        (aid == target).then(|| {
                            let v = pin.get("version").and_then(Value::as_i64).unwrap_or(0) as i32;
                            (alias.clone(), v)
                        })
                    })
                })
                .unwrap_or_default();
            AssetUsageItem {
                instance_id: r.id,
                template_id: r.template_id,
                template_name: r.template_name,
                template_version: r.template_version,
                status: r.status,
                mode: r.mode,
                alias,
                version_used,
                created_at: r.created_at,
            }
        })
        .collect();

    Ok(Json(PaginatedResponse {
        items,
        total,
        page: params.page,
        per_page: params.per_page,
    }))
}

// ── Multipart / query DTOs ──────────────────────────────────────────────────

/// Spec-only multipart wrapper for the CSV import body.
#[derive(Debug, utoipa::ToSchema)]
#[allow(dead_code)]
pub struct CsvImportBody {
    /// The CSV file contents.
    #[schema(value_type = String, format = Binary)]
    pub file: Vec<u8>,
}

/// Spec-only multipart wrapper for an asset File-field upload.
#[derive(Debug, utoipa::ToSchema)]
#[allow(dead_code)]
pub struct AssetFileUpload {
    /// Binary file contents.
    #[schema(value_type = String, format = Binary)]
    pub file: Vec<u8>,
}

/// Response of `POST /api/v1/assets/{id}/files` — the storage path to drop into
/// a record's `File` field value.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct AssetFileUploadResponse {
    /// The S3 storage key (`InputSource::StoragePath`).
    pub storage_path: String,
    pub filename: String,
    pub content_type: String,
    pub size: usize,
}

/// Query for `POST /api/v1/assets/{id}/files` — which File field this upload
/// targets (used in the deterministic S3 key).
#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
pub struct UploadFileQuery {
    pub field: String,
}

// ── Internal helpers ────────────────────────────────────────────────────────

/// Validate an asset-type schema at definition time: every field name must be a
/// valid identifier (so records map cleanly to columns / CSV headers) and names
/// must be unique.
fn validate_schema_fields(fields: &[PortField]) -> Result<(), ApiError> {
    let mut seen = std::collections::HashSet::new();
    for f in fields {
        if !IDENT_REGEX.is_match(&f.name) {
            return Err(ApiError::bad_request(format!(
                "field name '{}' must be a snake_case identifier",
                f.name
            )));
        }
        if !seen.insert(f.name.as_str()) {
            return Err(ApiError::bad_request(format!(
                "duplicate field name '{}'",
                f.name
            )));
        }
    }
    Ok(())
}

/// Build a [`Scope`] from a DB row's `(scope_kind, scope_id)`. `None` when the
/// kind is unrecognized (defensive — shouldn't happen given the CHECK-free
/// columns are only written by our handlers).
fn row_scope(scope_kind: &str, scope_id: Uuid) -> Option<Scope> {
    ScopeKind::from_db(scope_kind).map(|kind| Scope { kind, id: scope_id })
}

/// `?folder=` prefix filter on `display_path`. Absent filter ⇒ everything;
/// otherwise the row's `display_path` must equal or be nested under the prefix.
fn folder_matches(display_path: Option<&str>, folder: Option<&str>) -> bool {
    match folder {
        None => true,
        Some("") => true,
        Some(prefix) => match display_path {
            None => false,
            Some(dp) => dp == prefix || dp.starts_with(&format!("{prefix}/")),
        },
    }
}

/// Slice a sorted vec into a `PaginatedResponse` (the visible set is small —
/// in-memory paging after most-specific-wins resolution is fine).
fn paginate<T: utoipa::ToSchema>(items: Vec<T>, page: i64, per_page: i64) -> PaginatedResponse<T> {
    let total = items.len() as i64;
    let offset = ((page - 1).max(0) * per_page).max(0) as usize;
    let per = per_page.max(0) as usize;
    let paged = items.into_iter().skip(offset).take(per).collect();
    PaginatedResponse {
        items: paged,
        total,
        page,
        per_page,
    }
}

/// All `asset_types` rows owned by any scope in `visible`.
async fn fetch_visible_asset_type_rows(
    db: &sqlx::PgPool,
    visible: &VisibleScopes,
) -> Result<Vec<AssetTypeRow>, ApiError> {
    let (kinds, ids) = visible_pairs(visible);
    if kinds.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, AssetTypeRow>(
        "SELECT * FROM asset_types \
         WHERE deleted_at IS NULL \
           AND (scope_kind, scope_id) IN (SELECT * FROM UNNEST($1::text[], $2::uuid[]))",
    )
    .bind(&kinds)
    .bind(&ids)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// All `assets` rows owned by any scope in `visible`, optionally type-filtered.
async fn fetch_visible_asset_rows(
    db: &sqlx::PgPool,
    visible: &VisibleScopes,
    type_id: Option<Uuid>,
) -> Result<Vec<AssetRow>, ApiError> {
    let (kinds, ids) = visible_pairs(visible);
    if kinds.is_empty() {
        return Ok(Vec::new());
    }
    let rows = if let Some(tid) = type_id {
        sqlx::query_as::<_, AssetRow>(
            "SELECT * FROM assets \
             WHERE deleted_at IS NULL AND type_id = $3 \
               AND (scope_kind, scope_id) IN (SELECT * FROM UNNEST($1::text[], $2::uuid[]))",
        )
        .bind(&kinds)
        .bind(&ids)
        .bind(tid)
        .fetch_all(db)
        .await?
    } else {
        sqlx::query_as::<_, AssetRow>(
            "SELECT * FROM assets \
             WHERE deleted_at IS NULL \
               AND (scope_kind, scope_id) IN (SELECT * FROM UNNEST($1::text[], $2::uuid[]))",
        )
        .bind(&kinds)
        .bind(&ids)
        .fetch_all(db)
        .await?
    };
    Ok(rows)
}

/// All `asset_types` owned by EXACTLY one scope — the placement filter the
/// management browser uses (vs. the downward-visible set). No most-specific-wins
/// resolution: a single scope's ref-keys are unique, so the rows map straight
/// to summaries.
async fn fetch_exact_asset_type_rows(
    db: &sqlx::PgPool,
    kind: ScopeKind,
    scope_id: Uuid,
) -> Result<Vec<AssetTypeRow>, ApiError> {
    let rows = sqlx::query_as::<_, AssetTypeRow>(
        "SELECT * FROM asset_types \
         WHERE deleted_at IS NULL AND scope_kind = $1 AND scope_id = $2",
    )
    .bind(kind.as_db())
    .bind(scope_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// All `assets` owned by EXACTLY one scope, optionally type-filtered (the
/// placement filter — see [`fetch_exact_asset_type_rows`]).
async fn fetch_exact_asset_rows(
    db: &sqlx::PgPool,
    kind: ScopeKind,
    scope_id: Uuid,
    type_id: Option<Uuid>,
) -> Result<Vec<AssetRow>, ApiError> {
    let rows = if let Some(tid) = type_id {
        sqlx::query_as::<_, AssetRow>(
            "SELECT * FROM assets \
             WHERE deleted_at IS NULL AND scope_kind = $1 AND scope_id = $2 AND type_id = $3",
        )
        .bind(kind.as_db())
        .bind(scope_id)
        .bind(tid)
        .fetch_all(db)
        .await?
    } else {
        sqlx::query_as::<_, AssetRow>(
            "SELECT * FROM assets \
             WHERE deleted_at IS NULL AND scope_kind = $1 AND scope_id = $2",
        )
        .bind(kind.as_db())
        .bind(scope_id)
        .fetch_all(db)
        .await?
    };
    Ok(rows)
}

/// Flatten a [`VisibleScopes`] into parallel `(kind, id)` arrays for the
/// `UNNEST(...)` membership filter.
fn visible_pairs(visible: &VisibleScopes) -> (Vec<String>, Vec<Uuid>) {
    let mut kinds = Vec::new();
    let mut ids = Vec::new();
    if let Some(ws) = visible.workspace {
        kinds.push(ScopeKind::Workspace.as_db().to_string());
        ids.push(ws);
    }
    for p in &visible.folders {
        kinds.push(ScopeKind::Folder.as_db().to_string());
        ids.push(*p);
    }
    if let Some(t) = visible.template {
        kinds.push(ScopeKind::Template.as_db().to_string());
        ids.push(t);
    }
    (kinds, ids)
}

/// Fetch a live `asset_types` row or 404.
async fn fetch_asset_type(db: &sqlx::PgPool, id: Uuid) -> Result<AssetTypeRow, ApiError> {
    sqlx::query_as::<_, AssetTypeRow>(
        "SELECT * FROM asset_types WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| ApiError::not_found("asset type not found"))
}

/// Fetch a live `assets` row or 404.
async fn fetch_asset(db: &sqlx::PgPool, id: Uuid) -> Result<AssetRow, ApiError> {
    sqlx::query_as::<_, AssetRow>("SELECT * FROM assets WHERE id = $1 AND deleted_at IS NULL")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| ApiError::not_found("asset not found"))
}

/// Build an [`AssetTypeDetail`] from a row (deserializing `fields_json`).
fn asset_type_detail(row: AssetTypeRow) -> Result<AssetTypeDetail, ApiError> {
    let fields = parse_fields(&row.fields_json)?;
    Ok(AssetTypeDetail {
        id: row.id,
        scope_kind: row.scope_kind,
        scope_id: row.scope_id,
        name: row.name,
        display_name: row.display_name,
        display_path: row.display_path,
        fields,
        cardinality: row.cardinality,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
        created_by: row.created_by,
        updated_by: row.updated_by,
    })
}

/// Editor-or-above gate (docs/20 §2 governance). When the caller has no DB
/// membership context (dev_noop / no `workspace_id`), the gate is permissive —
/// matching how resources stays open in the offline dev posture. The role
/// check only fires for `workspace`-scoped writes (project/template scopes
/// resolve through their workspace, which we do not look up here to avoid an
/// extra round-trip — the workspace membership on the acting workspace is the
/// governance edge).
async fn require_editor(
    state: &AppState,
    user: &AuthUser,
    scope_kind: ScopeKind,
    scope_id: Uuid,
) -> Result<(), ApiError> {
    use crate::auth::membership::{map_to_api_error, require_role, MembershipError, Role};

    // The workspace whose membership governs this write. For a workspace scope
    // it's the scope id itself; for project/template scopes we fall back to the
    // caller's acting workspace (the membership edge that authenticated them).
    let governing_ws = match scope_kind {
        ScopeKind::Workspace => scope_id,
        _ => caller_workspace(user),
    };

    match require_role(&state.db, user, governing_ws, Role::Editor).await {
        Ok(_) => Ok(()),
        // No membership row at all (dev_noop / offline) → stay permissive so the
        // single-tenant dev stack keeps working, identical to resources.
        Err(MembershipError::NotMember(_)) => Ok(()),
        Err(other) => Err(map_to_api_error(other)),
    }
}

/// Editor-on-the-target-scope gate for an ASSET placement (create or move),
/// identical to the `create_asset` gate: workspace scope uses the membership
/// floor; folder/template scopes use the object ACL so a restricted container's
/// grants govern who can drop assets into it.
async fn require_asset_placement(
    state: &AppState,
    user: &AuthUser,
    scope_kind: ScopeKind,
    scope_id: Uuid,
) -> Result<(), ApiError> {
    match scope_kind {
        ScopeKind::Workspace => require_editor(state, user, scope_kind, scope_id).await,
        ScopeKind::Folder => {
            require_object_role(&state.db, user, ObjectRef::folder(scope_id), Role::Editor)
                .await
                .map(|_| ())
                .map_err(map_to_api_error)
        }
        ScopeKind::Template => {
            require_object_role(&state.db, user, ObjectRef::template(scope_id), Role::Editor)
                .await
                .map(|_| ())
                .map_err(map_to_api_error)
        }
    }
}

/// Validate + write a new record version. Replace clears the prior set; append
/// validates only the new rows and concatenates them after the current version
/// (renumbering `row_idx`). Always bumps `assets.version`.
async fn write_records(
    state: &AppState,
    asset: &AssetRow,
    new_records: &[Value],
    append: bool,
    updated_by: Option<Uuid>,
) -> Result<i32, ApiError> {
    let type_row = fetch_asset_type(&state.db, asset.type_id).await?;
    let fields = parse_fields(&type_row.fields_json)?;
    let port = type_port(fields);

    // Validate the incoming rows against the type schema.
    validate_records(&port, new_records)?;

    // Cardinality guard: `object` types hold exactly one row.
    if type_row.cardinality == Cardinality::Object.as_db() {
        let prior = if append {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM asset_records WHERE asset_id = $1 AND version = $2",
            )
            .bind(asset.id)
            .bind(asset.version)
            .fetch_one(&state.db)
            .await?
        } else {
            0
        };
        if prior + new_records.len() as i64 > 1 {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "object-cardinality asset holds at most one record",
            ));
        }
    }

    let new_version = asset.version + 1;

    let mut tx = state.db.begin().await?;

    // The carried-forward base when appending: copy the current version's rows
    // into the new version first, then append the new rows after them.
    let mut next_idx: i32 = 0;
    if append {
        let prior: Vec<(i32, Value)> = sqlx::query_as(
            "SELECT row_idx, data FROM asset_records \
             WHERE asset_id = $1 AND version = $2 ORDER BY row_idx ASC",
        )
        .bind(asset.id)
        .bind(asset.version)
        .fetch_all(&mut *tx)
        .await?;
        for (_, data) in prior {
            sqlx::query(
                "INSERT INTO asset_records (asset_id, version, row_idx, data) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(asset.id)
            .bind(new_version)
            .bind(next_idx)
            .bind(&data)
            .execute(&mut *tx)
            .await?;
            next_idx += 1;
        }
    }

    for rec in new_records {
        sqlx::query(
            "INSERT INTO asset_records (asset_id, version, row_idx, data) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(asset.id)
        .bind(new_version)
        .bind(next_idx)
        .bind(rec)
        .execute(&mut *tx)
        .await?;
        next_idx += 1;
    }

    sqlx::query(
        "UPDATE assets SET version = $1, updated_at = NOW(), \
             updated_by = COALESCE($2, updated_by) \
         WHERE id = $3",
    )
    .bind(new_version)
    .bind(updated_by)
    .bind(asset.id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(new_version)
}

/// Parse CSV bytes into record JSON objects keyed by field name, coercing each
/// cell per the field's [`crate::models::template::FieldKind`]. Header mode maps
/// columns by name (unmapped columns ignored); headerless mode maps columns
/// positionally to the type's field order.
fn parse_csv(bytes: &[u8], fields: &[PortField], has_header: bool) -> Result<Vec<Value>, ApiError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(has_header)
        .flexible(true)
        .from_reader(bytes);

    // Build the column-index → field map.
    // Header mode: read the header row, match each header cell to a field name.
    // Headerless: position i → fields[i].
    let column_fields: Vec<Option<&PortField>> = if has_header {
        let headers = reader
            .headers()
            .map_err(|e| ApiError::bad_request(format!("bad csv header: {e}")))?
            .clone();
        let by_name: std::collections::HashMap<&str, &PortField> =
            fields.iter().map(|f| (f.name.as_str(), f)).collect();
        headers
            .iter()
            .map(|h| by_name.get(h.trim()).copied())
            .collect()
    } else {
        fields.iter().map(Some).collect()
    };

    let mut records: Vec<Value> = Vec::new();
    for (line, result) in reader.records().enumerate() {
        let row = result.map_err(|e| ApiError::bad_request(format!("bad csv row {line}: {e}")))?;
        let mut obj = serde_json::Map::new();
        for (i, cell) in row.iter().enumerate() {
            let Some(field) = column_fields.get(i).copied().flatten() else {
                continue; // unmapped / extra column
            };
            if cell.is_empty() {
                continue; // empty cell → absent (reads as null/absent)
            }
            obj.insert(field.name.clone(), coerce_csv_cell(field, cell));
        }
        records.push(Value::Object(obj));
    }
    Ok(records)
}

/// Coerce a raw CSV string cell into the JSON shape the field's `FieldKind`
/// expects. Numbers/bools parse; everything else stays a string (File holds a
/// storage-path string; Json tries to parse, falling back to a string).
fn coerce_csv_cell(field: &PortField, cell: &str) -> Value {
    use crate::models::template::FieldKind;
    match field.kind {
        FieldKind::Number => cell
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(cell.to_string())),
        FieldKind::Bool => match cell.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "y" => Value::Bool(true),
            "false" | "0" | "no" | "n" => Value::Bool(false),
            _ => Value::String(cell.to_string()),
        },
        FieldKind::Json => {
            serde_json::from_str::<Value>(cell).unwrap_or_else(|_| Value::String(cell.to_string()))
        }
        _ => Value::String(cell.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::FieldKind;

    fn pf(name: &str, kind: FieldKind, required: bool) -> PortField {
        PortField {
            default: None,
            name: name.to_string(),
            label: name.to_string(),
            kind,
            required,
            options: None,
            description: None,
            accept: None,
            schema: None,
        }
    }

    #[test]
    fn record_validation_accepts_well_typed() {
        let port = type_port(vec![
            pf("name", FieldKind::Text, true),
            pf("density", FieldKind::Number, false),
        ]);
        let rec = serde_json::json!({"name": "steel", "density": 7.8});
        assert!(validate_record(&port, &rec).is_ok());
    }

    #[test]
    fn record_validation_rejects_missing_required() {
        let port = type_port(vec![pf("name", FieldKind::Text, true)]);
        let rec = serde_json::json!({});
        assert!(validate_record(&port, &rec).is_err());
    }

    #[test]
    fn record_validation_rejects_wrong_type() {
        let port = type_port(vec![pf("density", FieldKind::Number, false)]);
        let rec = serde_json::json!({"density": "heavy"});
        assert!(validate_record(&port, &rec).is_err());
    }

    #[test]
    fn record_validation_rejects_stray_key() {
        let port = type_port(vec![pf("name", FieldKind::Text, true)]);
        let rec = serde_json::json!({"name": "x", "bogus": 1});
        assert!(validate_record(&port, &rec).is_err());
    }

    #[test]
    fn additive_add_optional_ok() {
        let old = vec![pf("a", FieldKind::Text, true)];
        let new = vec![
            pf("a", FieldKind::Text, true),
            pf("b", FieldKind::Number, false),
        ];
        assert!(check_additive_evolution(&old, &new).is_ok());
    }

    #[test]
    fn additive_widen_to_json_ok() {
        let old = vec![pf("a", FieldKind::Text, true)];
        let new = vec![pf("a", FieldKind::Json, true)];
        assert!(check_additive_evolution(&old, &new).is_ok());
    }

    #[test]
    fn additive_remove_rejected() {
        let old = vec![
            pf("a", FieldKind::Text, true),
            pf("b", FieldKind::Text, false),
        ];
        let new = vec![pf("a", FieldKind::Text, true)];
        assert!(check_additive_evolution(&old, &new).is_err());
    }

    #[test]
    fn additive_retype_narrowing_rejected() {
        let old = vec![pf("a", FieldKind::Text, false)];
        let new = vec![pf("a", FieldKind::Number, false)];
        assert!(check_additive_evolution(&old, &new).is_err());
    }

    #[test]
    fn additive_newly_required_rejected() {
        let old = vec![pf("a", FieldKind::Text, false)];
        let new = vec![pf("a", FieldKind::Text, true)];
        assert!(check_additive_evolution(&old, &new).is_err());
    }

    #[test]
    fn additive_new_required_field_rejected() {
        let old = vec![pf("a", FieldKind::Text, true)];
        let new = vec![
            pf("a", FieldKind::Text, true),
            pf("b", FieldKind::Text, true),
        ];
        assert!(check_additive_evolution(&old, &new).is_err());
    }

    #[test]
    fn csv_header_maps_and_coerces() {
        let fields = vec![
            pf("name", FieldKind::Text, true),
            pf("density", FieldKind::Number, false),
            pf("ferrous", FieldKind::Bool, false),
        ];
        let csv = b"name,density,ferrous\nsteel,7.8,true\naluminium,2.7,no\n";
        let recs = parse_csv(csv, &fields, true).expect("parse");
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0]["name"], serde_json::json!("steel"));
        assert_eq!(recs[0]["density"], serde_json::json!(7.8));
        assert_eq!(recs[0]["ferrous"], serde_json::json!(true));
        assert_eq!(recs[1]["ferrous"], serde_json::json!(false));
    }

    #[test]
    fn csv_ignores_unmapped_columns() {
        let fields = vec![pf("name", FieldKind::Text, true)];
        let csv = b"name,junk\nsteel,ignore\n";
        let recs = parse_csv(csv, &fields, true).expect("parse");
        assert_eq!(recs.len(), 1);
        assert!(recs[0].get("junk").is_none());
        assert_eq!(recs[0]["name"], serde_json::json!("steel"));
    }

    #[test]
    fn folder_prefix_match() {
        assert!(folder_matches(Some("materials/metals"), Some("materials")));
        assert!(folder_matches(Some("materials"), Some("materials")));
        assert!(!folder_matches(Some("scripts"), Some("materials")));
        assert!(!folder_matches(None, Some("materials")));
        assert!(folder_matches(None, None));
    }

    // ── resolve_create_scope: body / query reconciliation ────────────────────

    fn test_user(ws: Uuid) -> AuthUser {
        AuthUser {
            subject: "dev".into(),
            email: None,
            display_name: None,
            roles: vec![],
            org_id: None,
            workspace_id: Some(ws),
            workspace_role: None,
            avatar_url: None,
        }
    }

    #[test]
    fn create_scope_defaults_to_caller_workspace() {
        let ws = Uuid::new_v4();
        let u = test_user(ws);
        assert_eq!(
            resolve_create_scope(&u, None, None, None).unwrap(),
            (ScopeKind::Workspace, ws)
        );
    }

    #[test]
    fn create_scope_body_only() {
        let ws = Uuid::new_v4();
        let folder = Uuid::new_v4();
        let u = test_user(ws);
        assert_eq!(
            resolve_create_scope(&u, None, Some(ScopeKind::Folder), Some(folder)).unwrap(),
            (ScopeKind::Folder, folder)
        );
    }

    #[test]
    fn create_scope_query_only() {
        // The previously-silent footgun: `?scope=folder:<id>` with no body
        // scope now actually takes effect instead of defaulting to workspace.
        let ws = Uuid::new_v4();
        let folder = Uuid::new_v4();
        let u = test_user(ws);
        let q = format!("folder:{folder}");
        assert_eq!(
            resolve_create_scope(&u, Some(&q), None, None).unwrap(),
            (ScopeKind::Folder, folder)
        );
    }

    #[test]
    fn create_scope_body_and_query_agree() {
        let ws = Uuid::new_v4();
        let folder = Uuid::new_v4();
        let u = test_user(ws);
        let q = format!("folder:{folder}");
        assert_eq!(
            resolve_create_scope(&u, Some(&q), Some(ScopeKind::Folder), Some(folder)).unwrap(),
            (ScopeKind::Folder, folder)
        );
    }

    #[test]
    fn create_scope_body_and_query_conflict_is_400() {
        let ws = Uuid::new_v4();
        let u = test_user(ws);
        let q = format!("folder:{}", Uuid::new_v4());
        let err = resolve_create_scope(&u, Some(&q), Some(ScopeKind::Folder), Some(Uuid::new_v4()))
            .unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }
}
