//! Phase B.9 — Resource CRUD endpoints.
//!
//! Eight handlers under the `resources` tag. The split between public
//! fields (DB `public_config` JSONB) and secret fields (Vault) happens
//! once, in [`split_config`], using the `ResourceTypeDescriptor.{public,
//! secret}_fields` lists. The handler then:
//!
//! 1. Looks up the descriptor from the registry — unknown type → 400.
//! 2. Structurally validates the config against the descriptor's lists
//!    (no stray keys, all secret fields present).
//! 3. Inserts the `resources` row (create only), then inserts the new
//!    `resource_versions` row.
//! 4. Calls [`ResourceSecretStore::put_version`] for the secret half.
//! 5. Writes one `resource_audit` row.
//!
//! Reads bypass the store entirely — they only need the DB-side
//! `public_config` and `latest_version`. The secret content lives in
//! Vault and is never re-emitted on the wire (the admin view returns
//! `<redacted>` placeholders via `redacted_secret_fields`).
//!
//! No workspace concept exists in v1. Every endpoint accepts an optional
//! `workspace_id` and resolves a missing one to `Uuid::nil()` — the
//! placeholder until the workspaces table lands.

use std::sync::LazyLock;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use regex::Regex;
use serde_json::{Map as JsonMap, Value};
use uuid::Uuid;

use aithericon_resources::registry::{all, lookup, schema_json_cached};

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::resource::{
    CreateResourceRequest, ListResourceAuditQuery, ListResourcesQuery, ResourceAuditEntry,
    ResourceDetail, ResourceRow, ResourceSummary, ResourceTypeInfo, ResourceVersionRow,
    RotateResourceRequest, UpdateResourceRequest,
};
use crate::models::template::PaginatedResponse;
use crate::petri::resource_resolver::AuditAction;
use crate::AppState;

/// Direct-mode resource identifier — a single snake_case identifier
/// that doubles as the reference key in Python source (`local_pg.host`)
/// and as the `WHERE path = $head` lookup at publish time. Must start
/// with a lowercase letter, then lowercase letters / digits / underscore.
/// Slashes and dashes are deliberately disallowed: a `<head>.<field>`
/// access in Python source must be a valid Python identifier, and the
/// path IS the head — the trailing-segment compromise would silently
/// break renames and create ambiguity between two resources sharing
/// the same trailing segment.
/// Snake_case identifier grammar shared by resource `path`s and `kv` key
/// names: both are dereferenced as `<head>.<field>` in workflow source and
/// so must be valid identifiers (lowercase leading letter, then lowercase
/// letters / digits / underscore).
static IDENT_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_]*$").expect("IDENT_REGEX must compile"));

/// Caller-implicit workspace: falls back to the user's session workspace
/// (set by the resolver from claims), then to `Uuid::nil()` for code paths
/// without an `AuthUser` (legacy `dev_noop` shape + the seeded default
/// workspace). The list/create endpoints accept an explicit `workspace_id`
/// query/body field that overrides this.
fn caller_workspace(user: &AuthUser) -> Uuid {
    user.workspace_id.unwrap_or_else(Uuid::nil)
}

/// Resolve the resource type or fail 400.
fn descriptor_or_400(
    type_name: &str,
) -> Result<&'static aithericon_resources::ResourceTypeDescriptor, ApiError> {
    lookup(type_name).ok_or_else(|| {
        ApiError::bad_request(format!(
            "unknown resource_type '{type_name}' — see GET /api/v1/resources/types"
        ))
    })
}

/// Internal marker key stored in `public_config` for `kv`-style resources.
/// Lists the user-supplied field names so the picker + resolver can
/// iterate without unwrapping the Vault bundle. Underscore-prefixed so it
/// can't collide with a real key name (real keys must match the same
/// `[a-z][a-z0-9_]*` shape as resource paths, see [`IDENT_REGEX`]).
const KV_KEYS_FIELD: &str = "__kv_keys";

/// Internal marker key R1 stashes in a `datacenter` resource's `public_config`
/// when (and only when) the optional `nomad_token` secret was supplied. mekhan
/// can't see Vault from the deploy path, so this public sentinel tells the
/// adapter-net builder whether to thread the `{{secret:…#nomad_token}}` template
/// (present → authenticated Nomad) or omit it (absent → unauthenticated Nomad).
/// Underscore-prefixed so it can't collide with a real connection field name.
pub(crate) const NOMAD_TOKEN_SENTINEL: &str = "__has_nomad_token";

/// Split a raw config map into `(public, secret)` JsonMaps based on the
/// descriptor's field lists. Strays (keys that match neither list) become
/// a structured 400 so the picker can highlight the offending field.
///
/// `dynamic_fields` types (today: just `kv`) take a different path: every
/// user-supplied key is treated as a secret, the field list is stashed in
/// `public_config.__kv_keys`, and the strays / required-fields gates are
/// replaced by a per-key identifier-safety check.
#[allow(clippy::type_complexity)]
/// Authoritative create/update validation for `datacenter` resources: a
/// `scheduler_flavor` must carry the connection fields that flavor needs so a
/// half-configured cluster can never be persisted (let alone reach a fire).
/// This is the belt-and-suspenders gate; the compiler re-asserts the PUBLIC
/// fields at publish (`DatacenterConnectionIncomplete`) so the editor can
/// highlight the offending node. No-op for every non-`datacenter` kind.
///
/// Required per flavor:
/// - `slurm` → public `ssh_host`, `ssh_user`, `template_dir` + secret `ssh_key`
/// - `nomad` → public `nomad_addr`
/// - `http`  → public `allocator_url`
///
/// A field counts as present when it's a non-null, non-empty-string value.
fn validate_datacenter_connection(
    resource_type: &str,
    public: &JsonMap<String, Value>,
    secret: &JsonMap<String, Value>,
) -> Result<(), ApiError> {
    if resource_type != "datacenter" {
        return Ok(());
    }

    let present = |map: &JsonMap<String, Value>, key: &str| -> bool {
        match map.get(key) {
            None | Some(Value::Null) => false,
            Some(Value::String(s)) => !s.trim().is_empty(),
            Some(_) => true,
        }
    };

    let flavor = public
        .get("scheduler_flavor")
        .and_then(|v| v.as_str())
        .unwrap_or("http")
        .to_string();

    // (field, is_secret) pairs the flavor requires.
    let required: &[(&str, bool)] = match flavor.as_str() {
        "slurm" => &[
            ("ssh_host", false),
            ("ssh_user", false),
            ("template_dir", false),
            ("ssh_key", true),
        ],
        "nomad" => &[("nomad_addr", false)],
        // "http" and any unrecognized flavor fall back to the HTTP leg.
        _ => &[("allocator_url", false)],
    };

    let missing: Vec<&str> = required
        .iter()
        .filter(|(field, is_secret)| {
            let map = if *is_secret { secret } else { public };
            !present(map, field)
        })
        .map(|(field, _)| *field)
        .collect();

    if missing.is_empty() {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "datacenter (flavor '{flavor}') is missing required connection \
                 field(s): {missing:?}"
            ),
        ))
    }
}

type ConfigSplit = (JsonMap<String, Value>, JsonMap<String, Value>);

fn split_config(
    descriptor: &aithericon_resources::ResourceTypeDescriptor,
    config: Value,
) -> Result<ConfigSplit, ApiError> {
    let Value::Object(map) = config else {
        return Err(ApiError::bad_request(
            "config must be a JSON object keyed by field name",
        ));
    };

    // Dynamic-fields fast path: every key is a user-supplied secret. Validate
    // each key matches the identifier grammar (so `<path>.<key>` references
    // are parseable downstream) and stash the key list as a `__kv_keys`
    // sentinel in `public_config`. The Vault bundle still carries the
    // values; only the names live in public_config so the picker can
    // surface them without unwrapping secrets.
    if descriptor.dynamic_fields {
        let mut public = JsonMap::new();
        let mut secret = JsonMap::new();
        let mut keys: Vec<String> = Vec::with_capacity(map.len());
        let mut bad_keys: Vec<String> = Vec::new();
        for (k, v) in map {
            if k == KV_KEYS_FIELD {
                // Caller can't write the internal marker directly — would
                // mask the real keys. Surface as 400 so a misuse from a
                // hand-rolled client is loud.
                return Err(ApiError::bad_request(format!(
                    "key '{KV_KEYS_FIELD}' is reserved",
                )));
            }
            if !IDENT_REGEX.is_match(&k) {
                bad_keys.push(k);
                continue;
            }
            secret.insert(k.clone(), v);
            keys.push(k);
        }
        if !bad_keys.is_empty() {
            bad_keys.sort();
            return Err(ApiError::bad_request(format!(
                "invalid kv key(s): {} — keys must be snake_case identifiers \
                 (start with a lowercase letter, then letters / digits / underscores)",
                bad_keys.join(", "),
            )));
        }
        keys.sort();
        public.insert(
            KV_KEYS_FIELD.to_string(),
            Value::Array(keys.into_iter().map(Value::String).collect()),
        );
        return Ok((public, secret));
    }

    let mut public = JsonMap::new();
    let mut secret = JsonMap::new();
    let mut stray = Vec::new();
    for (k, v) in map {
        if descriptor.public_fields.contains(&k.as_str()) {
            public.insert(k, v);
        } else if descriptor.secret_fields.contains(&k.as_str()) {
            secret.insert(k, v);
        } else {
            stray.push(k);
        }
    }
    if !stray.is_empty() {
        stray.sort();
        return Err(ApiError::bad_request(format!(
            "unknown config field(s) for type '{}': {} (allowed: {} public, {} secret)",
            descriptor.name,
            stray.join(", "),
            descriptor.public_fields.join(", "),
            descriptor.secret_fields.join(", "),
        )));
    }

    // Required-field gate, driven SOLELY by the schema's "required" array (the
    // schemars-derived schema lists non-Option fields as required; Option ones
    // — e.g. Postgres.sslmode, and every per-flavor datacenter connection field
    // incl. the `ssh_key`/`nomad_token`/`token` secrets — are absent). A secret
    // field is therefore required iff the schema says so: a flavor-tagged
    // datacenter supplies only its own flavor's secret (slurm→ssh_key,
    // nomad→nomad_token, both Option), so we must NOT demand all three. The
    // per-flavor connection COMPLETENESS (a slurm datacenter must carry ssh_host
    // + ssh_key, etc.) is enforced separately by validate_datacenter_connection.
    let schema = schema_json_cached(descriptor);
    let mut missing = Vec::new();
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for r in required {
            if let Some(name) = r.as_str() {
                let present = if descriptor.secret_fields.contains(&name) {
                    secret.contains_key(name)
                } else {
                    public.contains_key(name)
                };
                if !present {
                    missing.push(name.to_string());
                }
            }
        }
    }
    if !missing.is_empty() {
        missing.sort();
        missing.dedup();
        return Err(ApiError::bad_request(format!(
            "required config field(s) missing for type '{}': {}",
            descriptor.name,
            missing.join(", "),
        )));
    }

    Ok((public, secret))
}

/// Compose the launcher-deterministic vault path for a given version.
///
/// Exposed so integration tests can derive the same path the handlers write
/// to instead of re-spelling the `aithericon/resources/{ws}/{id}/v{n}` literal.
pub fn vault_path_for(workspace_id: Uuid, resource_id: Uuid, version: i32) -> String {
    format!("aithericon/resources/{workspace_id}/{resource_id}/v{version}")
}

/// Persist one resource version: insert the `resource_versions` row, then
/// write the secret half to the secret backend. If the Vault write fails the
/// just-inserted version row is rolled back so the parent's `latest_version`
/// stays consistent with what's actually retrievable.
///
/// `extra_rollback` runs additional cleanup on EITHER failure (version-insert
/// or Vault write) before the error is returned — `create_resource` uses it to
/// also delete the freshly-laid `resources` row so a retry with the same path
/// doesn't 409 against a half-created resource. update/rotate pass a no-op.
async fn write_resource_version<F, Fut>(
    db: &sqlx::PgPool,
    secret_store: &dyn aithericon_resources::ResourceSecretStore,
    resource_id: Uuid,
    version: i32,
    vault_path: &str,
    public: &JsonMap<String, Value>,
    secret: &JsonMap<String, Value>,
    principal_id: Uuid,
    extra_rollback: F,
) -> Result<(), ApiError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    // Record which secret fields actually got a value, so the resolver only
    // emits `{{secret:…#field}}` templates for secrets that exist in Vault.
    // Without this an optional secret left unset (e.g. a `loki` token) would
    // be templated and fail firing-time resolution. See
    // [`crate::petri::resource_resolver::SECRET_KEYS_MARKER`].
    let mut public_config = public.clone();
    let mut secret_keys: Vec<String> = secret.keys().cloned().collect();
    secret_keys.sort();
    public_config.insert(
        crate::petri::resource_resolver::SECRET_KEYS_MARKER.to_string(),
        Value::Array(secret_keys.into_iter().map(Value::String).collect()),
    );

    let insert_version = sqlx::query(
        "INSERT INTO resource_versions \
            (resource_id, version, vault_path, public_config, created_by) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(resource_id)
    .bind(version)
    .bind(vault_path)
    .bind(Value::Object(public_config))
    .bind(principal_id)
    .execute(db)
    .await;
    if let Err(e) = insert_version {
        extra_rollback().await;
        return Err(ApiError::internal(e.to_string()));
    }

    // Vault write last. On failure roll back the version row (and whatever
    // `extra_rollback` owns) so the next attempt sees a clean slate.
    if let Err(e) = secret_store.put_version(vault_path, secret).await {
        let _ =
            sqlx::query("DELETE FROM resource_versions WHERE resource_id = $1 AND version = $2")
                .bind(resource_id)
                .bind(version)
                .execute(db)
                .await;
        extra_rollback().await;
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            format!("secret backend write failed: {e}"),
        ));
    }

    Ok(())
}

/// Audit-row helper: every successful write goes through this so the
/// row shape stays consistent across endpoints.
async fn write_audit(
    db: &sqlx::PgPool,
    resource_id: Uuid,
    version: i32,
    principal_id: Uuid,
    action: AuditAction,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO resource_audit \
            (resource_id, resource_version, principal_id, action, site) \
         VALUES ($1, $2, $3, $4, 'api')",
    )
    .bind(resource_id)
    .bind(version)
    .bind(principal_id)
    .bind(action.as_str())
    .execute(db)
    .await?;
    Ok(())
}

/// For each `kv`-style row, fetch the latest version's `public_config` and
/// extract the user-supplied `__kv_keys` list. Returns a map keyed by
/// `resource_id` so callers can populate `ResourceSummary.dynamic_keys`
/// in one batched pass per page (avoids the N+1 query that a per-row
/// fetch would create).
///
/// Non-dynamic rows are skipped — they have `dynamic_keys: None` and the
/// picker drives off the descriptor's static field lists.
async fn fetch_dynamic_keys(
    db: &sqlx::PgPool,
    rows: &[ResourceRow],
) -> Result<std::collections::HashMap<Uuid, Vec<String>>, ApiError> {
    let dynamic_pairs: Vec<(Uuid, i32)> = rows
        .iter()
        .filter_map(|r| {
            lookup(&r.resource_type)
                .filter(|d| d.dynamic_fields)
                .map(|_| (r.id, r.latest_version))
        })
        .collect();
    if dynamic_pairs.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let ids: Vec<Uuid> = dynamic_pairs.iter().map(|(id, _)| *id).collect();
    let versions: Vec<i32> = dynamic_pairs.iter().map(|(_, v)| *v).collect();
    // One round-trip for every kv row on the page. UNNEST pairs each id
    // with its latest version so we don't accidentally read a stale
    // earlier version when a rotation is mid-flight.
    let rows: Vec<(Uuid, Value)> = sqlx::query_as(
        "SELECT resource_id, public_config FROM resource_versions \
         WHERE (resource_id, version) IN \
         (SELECT * FROM UNNEST($1::uuid[], $2::int4[]))",
    )
    .bind(&ids)
    .bind(&versions)
    .fetch_all(db)
    .await?;
    let mut out = std::collections::HashMap::with_capacity(rows.len());
    for (id, public_config) in rows {
        let keys = public_config
            .get("__kv_keys")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default();
        out.insert(id, keys);
    }
    Ok(out)
}

/// Build `ResourceSummary`s from raw rows + the dynamic-keys side-channel
/// in one place so every list-style endpoint stays in lockstep.
fn rows_to_summaries(
    rows: Vec<ResourceRow>,
    dyn_keys: &std::collections::HashMap<Uuid, Vec<String>>,
) -> Vec<ResourceSummary> {
    rows.into_iter()
        .map(|r| {
            let mut s = ResourceSummary::from(r);
            if let Some(keys) = dyn_keys.get(&s.id) {
                s.dynamic_keys = Some(keys.clone());
            }
            s
        })
        .collect()
}

/// `GET /api/v1/resources` — paginated list, optionally filtered by type.
#[utoipa::path(
    get,
    path = "/api/v1/resources",
    params(ListResourcesQuery),
    responses(
        (status = 200, description = "Paginated list of resources", body = PaginatedResponse<ResourceSummary>),
    ),
    tag = "resources",
)]
pub async fn list_resources(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListResourcesQuery>,
) -> Result<Json<PaginatedResponse<ResourceSummary>>, ApiError> {
    // docs/20 §2: when `?scope=` is present, resolve the downward-visible,
    // most-specific-wins set across the binding context's owner scopes. Absent
    // scope keeps the legacy flat `workspace_id` filter.
    if let Some(scope_raw) = params.scope.as_deref() {
        return list_resources_scoped(&state, &user, &params, scope_raw).await;
    }

    let workspace_id = params
        .workspace_id
        .unwrap_or_else(|| caller_workspace(&user));
    let offset = (params.page - 1) * params.per_page;

    let (rows, total) = if let Some(ref ty) = params.resource_type {
        let rows = sqlx::query_as::<_, ResourceRow>(
            "SELECT * FROM resources \
             WHERE workspace_id = $1 AND resource_type = $2 AND deleted_at IS NULL \
             ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(workspace_id)
        .bind(ty)
        .bind(params.per_page)
        .bind(offset)
        .fetch_all(&state.db)
        .await?;
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM resources \
             WHERE workspace_id = $1 AND resource_type = $2 AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .bind(ty)
        .fetch_one(&state.db)
        .await?;
        (rows, total)
    } else {
        let rows = sqlx::query_as::<_, ResourceRow>(
            "SELECT * FROM resources \
             WHERE workspace_id = $1 AND deleted_at IS NULL \
             ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(workspace_id)
        .bind(params.per_page)
        .bind(offset)
        .fetch_all(&state.db)
        .await?;
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM resources \
             WHERE workspace_id = $1 AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_one(&state.db)
        .await?;
        (rows, total)
    };

    let dyn_keys = fetch_dynamic_keys(&state.db, &rows).await?;
    Ok(Json(PaginatedResponse {
        items: rows_to_summaries(rows, &dyn_keys),
        total,
        page: params.page,
        per_page: params.per_page,
    }))
}

/// Scope-resolved resource list (docs/20 §2). Fetches every resource owned by a
/// scope in the binding context's downward-visible set, applies
/// most-specific-wins (`template > project > workspace`), then optional
/// type/folder filters. Used when `GET /api/v1/resources` carries `?scope=`.
async fn list_resources_scoped(
    state: &AppState,
    user: &AuthUser,
    params: &ListResourcesQuery,
    scope_raw: &str,
) -> Result<Json<PaginatedResponse<ResourceSummary>>, ApiError> {
    use crate::models::asset::ScopeKind;
    use crate::scope::{self, Scope, ScopedItem};

    // Parse the scope token: `workspace`, `project:<uuid>`, `template:<uuid>`.
    let (kind, scope_id) = {
        let raw = scope_raw.trim();
        if raw.is_empty() || raw == "workspace" {
            (
                ScopeKind::Workspace,
                params
                    .workspace_id
                    .unwrap_or_else(|| caller_workspace(user)),
            )
        } else {
            let (k, ids) = raw.split_once(':').ok_or_else(|| {
                ApiError::bad_request(format!(
                    "invalid scope '{raw}' — expected `workspace`, `project:<uuid>`, or `template:<uuid>`"
                ))
            })?;
            let kind = ScopeKind::from_db(k)
                .ok_or_else(|| ApiError::bad_request(format!("unknown scope kind '{k}'")))?;
            let id = Uuid::parse_str(ids)
                .map_err(|_| ApiError::bad_request(format!("scope id '{ids}' is not a uuid")))?;
            (kind, id)
        }
    };

    let visible = scope::visible_scopes_for(&state.db, kind, scope_id).await?;

    // Gather candidates owned by any visible scope.
    let mut kinds: Vec<String> = Vec::new();
    let mut ids: Vec<Uuid> = Vec::new();
    if let Some(ws) = visible.workspace {
        kinds.push("workspace".to_string());
        ids.push(ws);
    }
    for p in &visible.projects {
        kinds.push("project".to_string());
        ids.push(*p);
    }
    if let Some(t) = visible.template {
        kinds.push("template".to_string());
        ids.push(t);
    }
    if kinds.is_empty() {
        return Ok(Json(PaginatedResponse {
            items: Vec::new(),
            total: 0,
            page: params.page,
            per_page: params.per_page,
        }));
    }

    let rows = if let Some(ref ty) = params.resource_type {
        sqlx::query_as::<_, ResourceRow>(
            "SELECT * FROM resources \
             WHERE deleted_at IS NULL AND resource_type = $3 \
               AND (scope_kind, scope_id) IN (SELECT * FROM UNNEST($1::text[], $2::uuid[]))",
        )
        .bind(&kinds)
        .bind(&ids)
        .bind(ty)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, ResourceRow>(
            "SELECT * FROM resources \
             WHERE deleted_at IS NULL \
               AND (scope_kind, scope_id) IN (SELECT * FROM UNNEST($1::text[], $2::uuid[]))",
        )
        .bind(&kinds)
        .bind(&ids)
        .fetch_all(&state.db)
        .await?
    };

    let items: Vec<ScopedItem<ResourceRow>> = rows
        .into_iter()
        .filter_map(|r| {
            let kind = ScopeKind::from_db(&r.scope_kind)?;
            let sid = r.scope_id?;
            Some(ScopedItem {
                scope: Scope { kind, id: sid },
                ref_key: r.path.clone(),
                item: r,
            })
        })
        .collect();

    let resolved = scope::resolve_visible(&visible, items)
        .map_err(|c| ApiError::conflict(c.to_string()))?;

    // Optional folder prefix filter on display_path.
    let mut winning: Vec<ResourceRow> = resolved
        .into_values()
        .map(|si| si.item)
        .filter(|r| match params.folder.as_deref() {
            None => true,
            Some(prefix) if prefix.is_empty() => true,
            Some(prefix) => r
                .display_path
                .as_deref()
                .map(|dp| dp == prefix || dp.starts_with(&format!("{prefix}/")))
                .unwrap_or(false),
        })
        .collect();
    winning.sort_by(|a, b| a.path.cmp(&b.path));

    let total = winning.len() as i64;
    let offset = ((params.page - 1).max(0) * params.per_page).max(0) as usize;
    let per = params.per_page.max(0) as usize;
    let page_rows: Vec<ResourceRow> = winning.into_iter().skip(offset).take(per).collect();

    let dyn_keys = fetch_dynamic_keys(&state.db, &page_rows).await?;
    Ok(Json(PaginatedResponse {
        items: rows_to_summaries(page_rows, &dyn_keys),
        total,
        page: params.page,
        per_page: params.per_page,
    }))
}

/// `GET /api/v1/resources/types` — registry introspection. Powers the
/// frontend picker's type list and the schema-driven create form.
#[utoipa::path(
    get,
    path = "/api/v1/resources/types",
    responses(
        (status = 200, description = "Registered resource types", body = Vec<ResourceTypeInfo>),
    ),
    tag = "resources",
)]
pub async fn list_resource_types() -> Json<Vec<ResourceTypeInfo>> {
    let infos: Vec<ResourceTypeInfo> = all()
        .iter()
        .map(|d| ResourceTypeInfo {
            name: d.name.to_string(),
            display_name: d.display_name.to_string(),
            icon: d.icon.to_string(),
            oauth_provider: d.oauth_provider.map(str::to_string),
            secret_fields: d.secret_fields.iter().map(|s| (*s).to_string()).collect(),
            public_fields: d.public_fields.iter().map(|s| (*s).to_string()).collect(),
            schema: schema_json_cached(d).clone(),
            dynamic_fields: d.dynamic_fields,
        })
        .collect();
    Json(infos)
}

/// `POST /api/v1/resources` — create a logical resource and its v1 row.
#[utoipa::path(
    post,
    path = "/api/v1/resources",
    request_body = CreateResourceRequest,
    responses(
        (status = 201, description = "Resource created", body = ResourceSummary),
        (status = 400, description = "Validation failure", body = ErrorResponse),
        (status = 409, description = "Path already exists in workspace", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
        (status = 502, description = "Secret backend write failed", body = ErrorResponse),
    ),
    tag = "resources",
)]
pub async fn create_resource(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateResourceRequest>,
) -> Result<(StatusCode, Json<ResourceSummary>), ApiError> {
    let principal_id = user.subject_as_uuid();
    let workspace_id = req.workspace_id.unwrap_or_else(|| caller_workspace(&user));
    let summary = create_resource_internal(&state, &req, workspace_id, principal_id).await?;
    Ok((StatusCode::CREATED, Json(summary)))
}

/// Core resource-creation flow, callable without an HTTP `AuthUser`. Used by
/// [`create_resource`] (which derives `workspace_id`/`principal_id` from the
/// session) and by the demo seeder, which provisions resource fixtures as the
/// seeder principal. `workspace_id` and `principal_id` are passed explicitly
/// so the caller owns scoping + attribution; the validation, Vault write, ACL
/// grant, audit row, and pool-net hook are identical to the HTTP path.
pub(crate) async fn create_resource_internal(
    state: &AppState,
    req: &CreateResourceRequest,
    workspace_id: Uuid,
    principal_id: Uuid,
) -> Result<ResourceSummary, ApiError> {
    if !IDENT_REGEX.is_match(&req.path) {
        return Err(ApiError::bad_request(format!(
            "path '{}' must be a snake_case identifier (e.g. `local_pg`): \
             lowercase letter first, then letters / digits / underscores. \
             Resources are referenced in workflow code as `<path>.<field>`, \
             so the path itself must be a valid Python identifier.",
            req.path
        )));
    }
    let descriptor = descriptor_or_400(&req.resource_type)?;
    let (public, secret) = split_config(descriptor, req.config.clone())?;
    validate_datacenter_connection(&req.resource_type, &public, &secret)?;

    let resource_id = Uuid::new_v4();
    let version = 1;
    let vault_path = vault_path_for(workspace_id, resource_id, version);
    let display_name = req
        .display_name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| req.path.clone());

    // Lay down `resources` first — its UNIQUE(workspace_id, path) constraint
    // is the canonical conflict gate.
    // docs/20 §2: every resource carries a polymorphic owner. v1 create is
    // always workspace-scoped (scope_id = workspace_id); the project/template
    // scopes are authored later. The transitional `workspace_id` column is kept
    // in lockstep so legacy reads + the old unique constraint still work.
    let insert_resource = sqlx::query(
        "INSERT INTO resources \
            (id, workspace_id, path, resource_type, display_name, latest_version, created_by, \
             scope_kind, scope_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'workspace', $2)",
    )
    .bind(resource_id)
    .bind(workspace_id)
    .bind(&req.path)
    .bind(&req.resource_type)
    .bind(&display_name)
    .bind(version)
    .bind(principal_id)
    .execute(&state.db)
    .await;
    if let Err(e) = insert_resource {
        // Unique-violation on (workspace_id, path) → 409.
        if let Some(db_err) = e.as_database_error() {
            if db_err.is_unique_violation() {
                return Err(ApiError::conflict(format!(
                    "resource path '{}' already exists in this workspace",
                    req.path
                )));
            }
        }
        return Err(ApiError::internal(e.to_string()));
    }

    // Then the v1 row + Vault write. On either failure also delete the
    // `resources` row we just laid so a retry with the same path doesn't get a
    // 409 from a half-created resource.
    write_resource_version(
        &state.db,
        state.resource_store.as_ref(),
        resource_id,
        version,
        &vault_path,
        &public,
        &secret,
        principal_id,
        || async {
            let _ = sqlx::query("DELETE FROM resources WHERE id = $1")
                .bind(resource_id)
                .execute(&state.db)
                .await;
        },
    )
    .await?;

    // Grant the creator `read` so the resolver works out of the box.
    let _ = sqlx::query(
        "INSERT INTO resource_acl \
            (resource_id, principal_id, principal_kind, permission, granted_by) \
         VALUES ($1, $2, 'user', 'read', $3) \
         ON CONFLICT DO NOTHING",
    )
    .bind(resource_id)
    .bind(principal_id)
    .bind(principal_id)
    .execute(&state.db)
    .await;

    write_audit(
        &state.db,
        resource_id,
        version,
        principal_id,
        AuditAction::Create,
    )
    .await?;

    // R3/R4b: if this is a pool-backed kind (token_pool / datacenter), deploy
    // its backing net (idempotent, engine-down-tolerant). Runs after the
    // resource is durably persisted.
    ensure_pool_net_for_kind(
        state,
        &req.resource_type,
        workspace_id,
        resource_id,
        version,
        &public,
    )
    .await;

    let dynamic_keys = extract_kv_keys(&public);
    let summary = ResourceSummary {
        id: resource_id,
        path: req.path.clone(),
        resource_type: req.resource_type.clone(),
        display_name,
        latest_version: version,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        dynamic_keys,
    };
    Ok(summary)
}

/// R3/R4b hook: for a pool-backed resource kind, (re)deploy its backing net
/// `pool-<id>` to the engine. No-op for every other kind. Idempotent +
/// engine-down-tolerant. Called on create AND version bump so a config change
/// (capacity, allocator url/token) re-deploys the net.
///
/// - `token_pool` → [`crate::petri::pool_net::ensure_token_pool_net_deployed`]
///   (capacity from `public_config`).
/// - `datacenter` → [`crate::petri::pool_net::ensure_datacenter_adapter_deployed`]
///   (`allocator_url` from `public_config`; the token as a
///   `{{secret:<vault_path>#token}}` template the engine resolves at fire time —
///   `vault_path` from `(workspace_id, resource_id, version)`).
async fn ensure_pool_net_for_kind(
    state: &AppState,
    resource_type: &str,
    workspace_id: Uuid,
    resource_id: Uuid,
    version: i32,
    public: &JsonMap<String, Value>,
) {
    match resource_type {
        "token_pool" => {
            // `capacity` is a required public field of the TokenPool kind (R1),
            // so `split_config` guarantees it is present + a u32-shaped number.
            // Defend against a malformed blob by skipping (best-effort deploy).
            let Some(capacity) = public
                .get("capacity")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32)
            else {
                tracing::warn!(
                    %resource_id,
                    "token_pool resource has no numeric `capacity` in public_config; skipping pool-net deploy"
                );
                return;
            };
            crate::petri::pool_net::ensure_token_pool_net_deployed(
                &state.petri,
                resource_id,
                capacity,
            )
            .await;
        }
        "datacenter" => {
            // `scheduler_flavor` (public field) is the discriminant the connection
            // builder reads: `"slurm"` → SSH salloc/scancel, `"nomad"` → Nomad
            // API, `"http"` (default) → POST/DELETE against `allocator_url`. The
            // per-flavor connection fields all ride ON the resource (docs/16 §1).
            // Secret fields (`token`/`ssh_key`/`nomad_token`) become
            // `{{secret:<vault_path>#<field>}}` templates the engine resolves at
            // fire time — the shared `from_public_config` builds the same shape
            // the B-staging resolver uses, so they can't drift.
            let vault_path = vault_path_for(workspace_id, resource_id, version);
            let Some(conn) = crate::petri::pool_net::DatacenterConnection::from_public_config(
                resource_id,
                version,
                &vault_path,
                public,
            ) else {
                tracing::warn!(
                    %resource_id,
                    "datacenter resource is missing its flavor's required connection field in \
                     public_config; skipping adapter-net deploy (R1 publish/create validation \
                     is the authoritative gate)"
                );
                return;
            };

            crate::petri::pool_net::ensure_datacenter_adapter_deployed(&state.petri, &conn).await;
        }
        _ => {}
    }
}

/// Pull the `__kv_keys` array out of a `public_config` blob. Returns
/// `None` for typed resources (no sentinel present); `Some(...)` for
/// `kv` resources. Shared by every handler that emits a fresh
/// `ResourceSummary` after writing a version.
fn extract_kv_keys(public: &JsonMap<String, Value>) -> Option<Vec<String>> {
    public.get(KV_KEYS_FIELD)?.as_array().map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    })
}

/// `GET /api/v1/resources/{id}` — admin view. Secret fields are listed by
/// name only; values never leave Vault on the read path.
#[utoipa::path(
    get,
    path = "/api/v1/resources/{id}",
    params(("id" = Uuid, Path, description = "Resource id")),
    responses(
        (status = 200, description = "Resource detail", body = ResourceDetail),
        (status = 404, description = "Resource not found", body = ErrorResponse),
    ),
    tag = "resources",
)]
pub async fn get_resource(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ResourceDetail>, ApiError> {
    let row = sqlx::query_as::<_, ResourceRow>(
        "SELECT * FROM resources WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("resource not found"))?;

    let version = sqlx::query_as::<_, ResourceVersionRow>(
        "SELECT * FROM resource_versions WHERE resource_id = $1 AND version = $2",
    )
    .bind(row.id)
    .bind(row.latest_version)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::internal("latest_version row missing — DB inconsistent"))?;

    let descriptor = descriptor_or_400(&row.resource_type)?;
    let detail = ResourceDetail {
        id: row.id,
        path: row.path,
        resource_type: row.resource_type,
        display_name: row.display_name,
        latest_version: row.latest_version,
        created_at: row.created_at,
        updated_at: row.updated_at,
        public_config: version.public_config,
        redacted_secret_fields: descriptor
            .secret_fields
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
    };
    Ok(Json(detail))
}

/// `PUT /api/v1/resources/{id}` — update display_name and/or config. Setting
/// `config` bumps `latest_version` and writes a fresh vault_path; name-only
/// updates do not.
#[utoipa::path(
    put,
    path = "/api/v1/resources/{id}",
    params(("id" = Uuid, Path, description = "Resource id")),
    request_body = UpdateResourceRequest,
    responses(
        (status = 200, description = "Resource updated", body = ResourceSummary),
        (status = 400, description = "Validation failure", body = ErrorResponse),
        (status = 404, description = "Resource not found", body = ErrorResponse),
        (status = 502, description = "Secret backend write failed", body = ErrorResponse),
    ),
    tag = "resources",
)]
pub async fn update_resource(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateResourceRequest>,
) -> Result<Json<ResourceSummary>, ApiError> {
    let row = sqlx::query_as::<_, ResourceRow>(
        "SELECT * FROM resources WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("resource not found"))?;

    if req.display_name.is_none() && req.config.is_none() {
        return Err(ApiError::bad_request(
            "update body must set at least one of `display_name` or `config`",
        ));
    }

    let principal_id = user.subject_as_uuid();
    let mut latest_version = row.latest_version;
    let mut display_name = row.display_name.clone();
    let mut new_kv_keys: Option<Vec<String>> = None;

    if let Some(name) = req.display_name {
        let trimmed = name.trim().to_string();
        if trimmed.is_empty() {
            return Err(ApiError::bad_request("display_name cannot be empty"));
        }
        sqlx::query("UPDATE resources SET display_name = $1, updated_at = NOW() WHERE id = $2")
            .bind(&trimmed)
            .bind(row.id)
            .execute(&state.db)
            .await?;
        display_name = trimmed;
    }

    if let Some(config) = req.config {
        let descriptor = descriptor_or_400(&row.resource_type)?;
        let (public, secret) = split_config(descriptor, config)?;

        latest_version = row.latest_version + 1;
        let vault_path = vault_path_for(row.workspace_id, row.id, latest_version);

        write_resource_version(
            &state.db,
            state.resource_store.as_ref(),
            row.id,
            latest_version,
            &vault_path,
            &public,
            &secret,
            principal_id,
            || async {},
        )
        .await?;

        sqlx::query("UPDATE resources SET latest_version = $1, updated_at = NOW() WHERE id = $2")
            .bind(latest_version)
            .bind(row.id)
            .execute(&state.db)
            .await?;

        write_audit(
            &state.db,
            row.id,
            latest_version,
            principal_id,
            AuditAction::Update,
        )
        .await?;

        // R3/R4b: re-deploy the backing net on a pool-kind version bump so a
        // config change (capacity / allocator url+token) takes effect
        // (idempotent, engine-down-tolerant).
        ensure_pool_net_for_kind(
            &state,
            &row.resource_type,
            row.workspace_id,
            row.id,
            latest_version,
            &public,
        )
        .await;

        new_kv_keys = extract_kv_keys(&public);
    }

    // Surface the kv keys even when the config wasn't touched — the picker
    // expects the field to track current state, not just the delta on this
    // request.
    let dynamic_keys = if new_kv_keys.is_some() {
        new_kv_keys
    } else if lookup(&row.resource_type)
        .map(|d| d.dynamic_fields)
        .unwrap_or(false)
    {
        fetch_dynamic_keys(&state.db, std::slice::from_ref(&row))
            .await?
            .remove(&row.id)
    } else {
        None
    };

    Ok(Json(ResourceSummary {
        id: row.id,
        path: row.path,
        resource_type: row.resource_type,
        display_name,
        latest_version,
        created_at: row.created_at,
        updated_at: Utc::now(),
        dynamic_keys,
    }))
}

/// `DELETE /api/v1/resources/{id}` — soft delete. Preserves
/// `resource_versions` rows + Vault paths so already-pinned instances keep
/// resolving.
#[utoipa::path(
    delete,
    path = "/api/v1/resources/{id}",
    params(("id" = Uuid, Path, description = "Resource id")),
    responses(
        (status = 204, description = "Resource soft-deleted"),
        (status = 404, description = "Resource not found", body = ErrorResponse),
    ),
    tag = "resources",
)]
pub async fn delete_resource(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let row = sqlx::query_as::<_, ResourceRow>(
        "SELECT * FROM resources WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("resource not found"))?;

    sqlx::query("UPDATE resources SET deleted_at = NOW(), updated_at = NOW() WHERE id = $1")
        .bind(row.id)
        .execute(&state.db)
        .await?;

    write_audit(
        &state.db,
        row.id,
        row.latest_version,
        user.subject_as_uuid(),
        AuditAction::Delete,
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/resources/{id}/rotate` — write a new version. Identical to
/// `update_resource` with only `config` set, plus a different audit verb.
#[utoipa::path(
    post,
    path = "/api/v1/resources/{id}/rotate",
    params(("id" = Uuid, Path, description = "Resource id")),
    request_body = RotateResourceRequest,
    responses(
        (status = 200, description = "Resource rotated to a new version", body = ResourceSummary),
        (status = 400, description = "Validation failure", body = ErrorResponse),
        (status = 404, description = "Resource not found", body = ErrorResponse),
        (status = 502, description = "Secret backend write failed", body = ErrorResponse),
    ),
    tag = "resources",
)]
pub async fn rotate_resource(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<RotateResourceRequest>,
) -> Result<Json<ResourceSummary>, ApiError> {
    let row = sqlx::query_as::<_, ResourceRow>(
        "SELECT * FROM resources WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("resource not found"))?;

    let descriptor = descriptor_or_400(&row.resource_type)?;
    let (public, secret) = split_config(descriptor, req.config)?;
    validate_datacenter_connection(&row.resource_type, &public, &secret)?;

    let principal_id = user.subject_as_uuid();
    let new_version = row.latest_version + 1;
    let vault_path = vault_path_for(row.workspace_id, row.id, new_version);

    write_resource_version(
        &state.db,
        state.resource_store.as_ref(),
        row.id,
        new_version,
        &vault_path,
        &public,
        &secret,
        principal_id,
        || async {},
    )
    .await?;

    sqlx::query("UPDATE resources SET latest_version = $1, updated_at = NOW() WHERE id = $2")
        .bind(new_version)
        .bind(row.id)
        .execute(&state.db)
        .await?;

    write_audit(
        &state.db,
        row.id,
        new_version,
        principal_id,
        AuditAction::Rotate,
    )
    .await?;

    let dynamic_keys = extract_kv_keys(&public);
    Ok(Json(ResourceSummary {
        id: row.id,
        path: row.path,
        resource_type: row.resource_type,
        display_name: row.display_name,
        latest_version: new_version,
        created_at: row.created_at,
        updated_at: Utc::now(),
        dynamic_keys,
    }))
}

/// `GET /api/v1/resources/{id}/audit` — paginated audit trail for a resource.
#[utoipa::path(
    get,
    path = "/api/v1/resources/{id}/audit",
    params(
        ("id" = Uuid, Path, description = "Resource id"),
        ListResourceAuditQuery
    ),
    responses(
        (status = 200, description = "Paginated audit entries", body = PaginatedResponse<ResourceAuditEntry>),
        (status = 404, description = "Resource not found", body = ErrorResponse),
    ),
    tag = "resources",
)]
pub async fn list_resource_audit(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(params): Query<ListResourceAuditQuery>,
) -> Result<Json<PaginatedResponse<ResourceAuditEntry>>, ApiError> {
    // Soft-delete tolerance: audit trail is still queryable for deleted
    // resources (compliance), so we don't filter `deleted_at`.
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM resources WHERE id = $1)")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    if !exists {
        return Err(ApiError::not_found("resource not found"));
    }

    let offset = (params.page - 1) * params.per_page;
    let rows = sqlx::query_as::<_, ResourceAuditEntry>(
        "SELECT id, resource_id, resource_version, action, principal_id, site, \
                instance_id, step_id, occurred_at \
         FROM resource_audit WHERE resource_id = $1 \
         ORDER BY occurred_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(id)
    .bind(params.per_page)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM resource_audit WHERE resource_id = $1")
            .bind(id)
            .fetch_one(&state.db)
            .await?;

    Ok(Json(PaginatedResponse {
        items: rows,
        total,
        page: params.page,
        per_page: params.per_page,
    }))
}
