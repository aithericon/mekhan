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

use std::collections::HashSet;
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

use crate::auth::{
    apply_grant, effective_object_roles, filter_and_annotate_visible, map_to_api_error,
    require_object_role, require_role, AuthUser, ObjectKind, ObjectRef, Role,
};
use crate::models::asset::PLATFORM_SCOPE_ID;
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
/// (set by the resolver from claims). Rejects with 403 when the caller has no
/// active workspace rather than silently acting in the nil tenant. The
/// list/create endpoints accept an explicit `workspace_id` query/body field
/// that overrides this.
fn caller_workspace(user: &AuthUser) -> Result<Uuid, ApiError> {
    user.require_workspace()
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

/// The create-form key a caller sets to name a `capacity` preset (doc 23 §7).
/// Consumed + stripped by [`expand_capacity_preset`] before [`split_config`]
/// sees the blob — it is NOT a `public_config` field, so it can't survive the
/// stray-key gate. Underscore-prefixed so it can't collide with an axis name.
const CAPACITY_PRESET_KEY: &str = "preset";

/// Expand a `capacity` create config carrying a `preset` name into its locked
/// axis set, letting caller-supplied axes override the free ones, then strip
/// the `preset` key. No-op for every non-`capacity` kind and for a `capacity`
/// config that names no preset (the axes are then taken verbatim).
///
/// This is the surface refinement #1 / doc 24 §2 calls for: "presets set the
/// locked axes; the form exposes the free ones." A named preset prefills the
/// coherent combination; the caller's explicit axes win on conflict so the few
/// free axes (e.g. a worker's unit count) can be set in the same call.
fn expand_capacity_preset(resource_type: &str, config: &mut Value) -> Result<(), ApiError> {
    if resource_type != "capacity" {
        return Ok(());
    }
    let Some(map) = config.as_object_mut() else {
        // split_config will reject a non-object with its own message.
        return Ok(());
    };
    let Some(preset_val) = map.remove(CAPACITY_PRESET_KEY) else {
        return Ok(()); // axes given verbatim
    };
    let preset_name = preset_val.as_str().ok_or_else(|| {
        ApiError::bad_request("capacity `preset` must be a string naming a known preset")
    })?;
    let preset = crate::models::capacity::preset_by_name(preset_name).ok_or_else(|| {
        let known: Vec<String> = crate::models::capacity::presets()
            .into_iter()
            .map(|p| p.name)
            .collect();
        ApiError::bad_request(format!(
            "unknown capacity preset '{preset_name}' — known presets: {}",
            known.join(", ")
        ))
    })?;

    // Serialize the preset's axes to a flat JSON object, then layer the
    // caller's explicit fields ON TOP (override-the-free-axes). The axes
    // struct flattens `CapacityAmount` into `capacity_kind` (+ optional
    // `capacity_amount`), matching the wire fields of the `Capacity`
    // descriptor exactly.
    let Value::Object(axis_map) = serde_json::to_value(preset.axes)
        .map_err(|e| ApiError::internal(format!("preset serialize: {e}")))?
    else {
        return Err(ApiError::internal(
            "preset axes did not serialize to an object",
        ));
    };
    for (k, v) in axis_map {
        map.entry(k).or_insert(v);
    }
    Ok(())
}

/// Authoritative create/update/rotate validation for `capacity` resources: the
/// trait-space axes in `public_config` must parse into a coherent point in the
/// space (doc 35 §6). Rejects the consent-invariant violations (`consent` ×
/// non-`presence` liveness, `consent × partition`) with a 400; returns the
/// non-fatal scale-mismatch WARNINGS (`competing_consumer × predicate`) for
/// the caller to log. No-op for every non-`capacity` kind.
///
/// The `serde(tag = "kind")` flattening on `CapacityAmount` means the typed
/// `CapacityAxes` deserializes straight off the public half: `capacity_kind`
/// (the tag) + `capacity_amount` (the content, for `fixed`). A missing/invalid
/// axis surfaces as a 400 naming the bad field rather than a 500.
fn validate_capacity_axes(
    resource_type: &str,
    public: &JsonMap<String, Value>,
) -> Result<Vec<String>, ApiError> {
    if resource_type != "capacity" {
        return Ok(Vec::new());
    }
    let axes: crate::models::capacity::CapacityAxes =
        serde_json::from_value(Value::Object(public.clone())).map_err(|e| {
            ApiError::bad_request(format!(
                "capacity axes are malformed or missing: {e} — expected liveness / acceptance / \
                 capacity_kind (+ capacity_amount for fixed) / eligibility"
            ))
        })?;
    axes.validate()
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

/// For each `capacity` row, fetch the latest version's `public_config` so the
/// list endpoint can surface the trait-space axes the editor's deployment
/// picker discriminates on (liveness presence → runner group, seeded →
/// concurrency limit, competing_consumer → worker). Returns a map keyed by
/// `resource_id`, batched in ONE query per page (same UNNEST pairing as
/// [`fetch_dynamic_keys`], so a mid-rotation read never picks a stale version).
///
/// Non-capacity rows are skipped — they keep `public_config: None` and the list
/// stays cheap.
async fn fetch_capacity_public_config(
    db: &sqlx::PgPool,
    rows: &[ResourceRow],
) -> Result<std::collections::HashMap<Uuid, Value>, ApiError> {
    let pairs: Vec<(Uuid, i32)> = rows
        .iter()
        .filter(|r| r.resource_type == "capacity")
        .map(|r| (r.id, r.latest_version))
        .collect();
    if pairs.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let ids: Vec<Uuid> = pairs.iter().map(|(id, _)| *id).collect();
    let versions: Vec<i32> = pairs.iter().map(|(_, v)| *v).collect();
    let rows: Vec<(Uuid, Value)> = sqlx::query_as(
        "SELECT resource_id, public_config FROM resource_versions \
         WHERE (resource_id, version) IN \
         (SELECT * FROM UNNEST($1::uuid[], $2::int4[]))",
    )
    .bind(&ids)
    .bind(&versions)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Build `ResourceSummary`s from raw rows + the per-row side-channels (kv
/// dynamic keys + capacity public config) in one place so every list-style
/// endpoint stays in lockstep.
fn rows_to_summaries(
    rows: Vec<ResourceRow>,
    dyn_keys: &std::collections::HashMap<Uuid, Vec<String>>,
    capacity_public: &std::collections::HashMap<Uuid, Value>,
) -> Vec<ResourceSummary> {
    rows.into_iter()
        .map(|r| {
            let mut s = ResourceSummary::from(r);
            if let Some(keys) = dyn_keys.get(&s.id) {
                s.dynamic_keys = Some(keys.clone());
            }
            if let Some(public) = capacity_public.get(&s.id) {
                s.public_config = Some(public.clone());
            }
            s
        })
        .collect()
}

/// Stamp each summary with the caller's effective object role and DROP rows the
/// caller can't reach (a `restricted` resource with no grant is absent from the
/// role map). For non-restricted resources the workspace floor keeps every row,
/// so this only filters when privacy is in play. NOTE: `total` is computed
/// pre-filter, so it can overcount when restricted rows are hidden — acceptable
/// for v1 (restricted is opt-in and rare).
async fn annotate_resource_roles(
    state: &AppState,
    user: &AuthUser,
    workspace_id: Uuid,
    mut items: Vec<ResourceSummary>,
) -> Result<Vec<ResourceSummary>, ApiError> {
    filter_and_annotate_visible(
        &state.db,
        user,
        ObjectKind::Resource,
        workspace_id,
        &mut items,
    )
    .await
    .map_err(map_to_api_error)?;
    Ok(items)
}

/// Build annotated summaries from an ALREADY-shadow-resolved set of mixed
/// tenant + platform rows. The tenant subset goes through the ACL annotate path
/// (drops restricted rows the caller can't reach). Platform winners bypass the
/// workspace-local ACL entirely — they're globally visible — and are stamped
/// directly: `owner` for a platform admin, else `viewer`, never restricted.
///
/// `platform_ids` is the set of row ids that loaded as `scope_kind='platform'`.
async fn annotate_with_platform(
    state: &AppState,
    user: &AuthUser,
    workspace_id: Uuid,
    rows: Vec<ResourceRow>,
    platform_ids: &HashSet<Uuid>,
) -> Result<Vec<ResourceSummary>, ApiError> {
    // Split tenant vs platform winners.
    let (platform_rows, tenant_rows): (Vec<ResourceRow>, Vec<ResourceRow>) = rows
        .into_iter()
        .partition(|r| platform_ids.contains(&r.id));

    let dyn_keys = fetch_dynamic_keys(&state.db, &tenant_rows).await?;
    let capacity_public = fetch_capacity_public_config(&state.db, &tenant_rows).await?;
    let tenant_summaries = rows_to_summaries(tenant_rows, &dyn_keys, &capacity_public);
    let mut out = annotate_resource_roles(state, user, workspace_id, tenant_summaries).await?;

    // Platform winners: stamp directly, bypass the ACL filter.
    let dyn_keys = fetch_dynamic_keys(&state.db, &platform_rows).await?;
    let capacity_public = fetch_capacity_public_config(&state.db, &platform_rows).await?;
    let plat_role = if user.is_platform_admin {
        Role::Owner
    } else {
        Role::Viewer
    };
    for mut s in rows_to_summaries(platform_rows, &dyn_keys, &capacity_public) {
        s.my_effective_role = Some(plat_role.as_label().to_string());
        s.restricted = false;
        out.push(s);
    }
    Ok(out)
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

    let workspace_id = match params.workspace_id {
        Some(ws) => ws,
        None => caller_workspace(&user)?,
    };

    // UNION the caller's workspace rows with the globally-visible platform tier.
    // Shadowing + pagination happen in Rust (a tenant row of the same path wins
    // over a platform one), so SQL fetches the full candidate set unpaginated.
    let rows = if let Some(ref ty) = params.resource_type {
        sqlx::query_as::<_, ResourceRow>(
            "SELECT * FROM resources \
             WHERE (workspace_id = $1 OR scope_kind = 'platform') \
               AND resource_type = $2 AND deleted_at IS NULL \
             ORDER BY created_at DESC",
        )
        .bind(workspace_id)
        .bind(ty)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, ResourceRow>(
            "SELECT * FROM resources \
             WHERE (workspace_id = $1 OR scope_kind = 'platform') AND deleted_at IS NULL \
             ORDER BY created_at DESC",
        )
        .bind(workspace_id)
        .fetch_all(&state.db)
        .await?
    };

    // Shadow platform rows with a same-path workspace row (most-specific-wins).
    // A workspace binding context: workspace + platform visible.
    use crate::scope::{self, Scope, ScopedItem, VisibleScopes};
    let visible = VisibleScopes {
        platform: true,
        workspace: Some(workspace_id),
        folders: Vec::new(),
        template: None,
    };
    let scoped: Vec<ScopedItem<ResourceRow>> = rows
        .into_iter()
        .filter_map(|r| {
            let kind = crate::models::asset::ScopeKind::from_db(&r.scope_kind)?;
            let sid = r.scope_id?;
            Some(ScopedItem {
                scope: Scope { kind, id: sid },
                ref_key: r.path.clone(),
                item: r,
            })
        })
        .collect();
    let winning: Vec<ResourceRow> = scope::resolve_visible(&visible, scoped)
        .map_err(|c| ApiError::conflict(c.to_string()))?
        .into_values()
        .map(|si| si.item)
        .collect();

    let platform_ids: HashSet<Uuid> = winning
        .iter()
        .filter(|r| r.scope_kind == "platform")
        .map(|r| r.id)
        .collect();

    let mut items =
        annotate_with_platform(&state, &user, workspace_id, winning, &platform_ids).await?;
    items.sort_by_key(|s| std::cmp::Reverse(s.created_at));
    let total = items.len() as i64;
    let offset = ((params.page - 1).max(0) * params.per_page).max(0) as usize;
    let per = params.per_page.max(0) as usize;
    let items: Vec<ResourceSummary> = items.into_iter().skip(offset).take(per).collect();
    Ok(Json(PaginatedResponse {
        items,
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

    // Parse the scope token: `workspace`, `folder:<uuid>`, `template:<uuid>`.
    let (kind, scope_id) = {
        let raw = scope_raw.trim();
        if raw.is_empty() || raw == "workspace" {
            (
                ScopeKind::Workspace,
                match params.workspace_id {
                    Some(ws) => ws,
                    None => caller_workspace(user)?,
                },
            )
        } else {
            let (k, ids) = raw.split_once(':').ok_or_else(|| {
                ApiError::bad_request(format!(
                    "invalid scope '{raw}' — expected `workspace`, `folder:<uuid>`, or `template:<uuid>`"
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

    // Gather candidate owner scopes. Exact mode (the management browser) restricts
    // to the single requested scope, so a folder shows only resources placed in it
    // and the workspace root shows only workspace-scoped resources. Otherwise the
    // full downward-visible chain (node picker / compiler).
    let (kinds, ids): (Vec<String>, Vec<Uuid>) = if params.exact == Some(true) {
        (vec![kind.as_db().to_string()], vec![scope_id])
    } else {
        let mut kinds: Vec<String> = Vec::new();
        let mut ids: Vec<Uuid> = Vec::new();
        // Platform tier: the least-specific global fallback, shadowed by any
        // workspace/folder/template row of the same path in `resolve_visible`.
        if visible.platform {
            kinds.push("platform".to_string());
            ids.push(PLATFORM_SCOPE_ID);
        }
        if let Some(ws) = visible.workspace {
            kinds.push("workspace".to_string());
            ids.push(ws);
        }
        for p in &visible.folders {
            kinds.push("folder".to_string());
            ids.push(*p);
        }
        if let Some(t) = visible.template {
            kinds.push("template".to_string());
            ids.push(t);
        }
        (kinds, ids)
    };
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

    let resolved =
        scope::resolve_visible(&visible, items).map_err(|c| ApiError::conflict(c.to_string()))?;

    // Optional folder prefix filter on display_path.
    let mut winning: Vec<ResourceRow> = resolved
        .into_values()
        .map(|si| si.item)
        .filter(|r| match params.folder.as_deref() {
            None => true,
            Some("") => true,
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

    // Platform winners bypass the workspace-local ACL (globally visible);
    // stamp them directly. Tenant winners go through the ACL annotate path.
    let platform_ids: HashSet<Uuid> = page_rows
        .iter()
        .filter(|r| r.scope_kind == "platform")
        .map(|r| r.id)
        .collect();
    let items =
        annotate_with_platform(state, user, caller_workspace(user)?, page_rows, &platform_ids)
            .await?;
    Ok(Json(PaginatedResponse {
        items,
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
            // Surface the trait-space presets on the `capacity` type so the
            // create form can offer "worker / limit / instrument" with their
            // locked axes (doc 23 §7). No other kind has presets.
            capacity_presets: (d.name == "capacity").then(crate::models::capacity::presets),
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
    let scope_kind = req
        .scope_kind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("workspace");

    // Platform-scoped resources are curated by a platform admin and live under
    // the synthetic PLATFORM_SCOPE_ID (workspace_id = scope_id = sentinel). The
    // gate is purely the admin flag — workspace-local ACLs (require_role /
    // object_grants) would 403 everyone, so they're bypassed entirely.
    let workspace_id = if scope_kind == "platform" {
        if !user.is_platform_admin {
            return Err(ApiError::forbidden(
                "platform-scoped resource writes require platform admin",
            ));
        }
        PLATFORM_SCOPE_ID
    } else {
        match req.workspace_id {
            Some(ws) => ws,
            None => caller_workspace(&user)?,
        }
    };

    // Placement gate: you must be Editor on the scope you create into — the
    // workspace (workspace-scoped) or the owning folder/template. The platform
    // scope is gated above on the admin flag and skips the ACL path.
    match scope_kind {
        "platform" => Role::Owner,
        "workspace" => require_role(&state.db, &user, workspace_id, Role::Editor)
            .await
            .map_err(map_to_api_error)?,
        "folder" => {
            let fid = req.scope_id.ok_or_else(|| {
                ApiError::bad_request("scope_id is required for scope_kind 'folder'")
            })?;
            require_object_role(&state.db, &user, ObjectRef::folder(fid), Role::Editor)
                .await
                .map_err(map_to_api_error)?
        }
        "template" => {
            let tid = req.scope_id.ok_or_else(|| {
                ApiError::bad_request("scope_id is required for scope_kind 'template'")
            })?;
            require_object_role(&state.db, &user, ObjectRef::template(tid), Role::Editor)
                .await
                .map_err(map_to_api_error)?
        }
        // create_resource_internal rejects unknown kinds with a 400.
        _ => Role::Editor,
    };

    let mut summary = create_resource_internal(&state, &req, workspace_id, principal_id).await?;
    // The creator owns it (apply_grant in the core flow); stamp it so the
    // frontend can gate immediately without a re-fetch.
    summary.my_effective_role = Some(Role::Owner.as_label().to_string());
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
    create_resource_internal_with_id(state, req, workspace_id, principal_id, None).await
}

/// As [`create_resource_internal`], but with an OPTIONAL explicit resource id.
///
/// `id_override = None` is the normal path (a fresh `Uuid::new_v4()`). The
/// override exists for ONE narrow case: pinning a seeded resource's id to a
/// value resolved out-of-band. The only caller that uses it is
/// [`crate::worker_groups::ensure_default_worker_group`] under the e2e test
/// harness, which needs the workspace's `default` worker-group capacity id to
/// equal the partition the live dev executor is already bound to (so a
/// fresh-DB test's compiler stamps a partition a worker actually drains). It is
/// never set in production — the boot seeder always passes `None`.
pub(crate) async fn create_resource_internal_with_id(
    state: &AppState,
    req: &CreateResourceRequest,
    workspace_id: Uuid,
    principal_id: Uuid,
    id_override: Option<Uuid>,
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
    // Expand a `capacity` preset (if named) into its locked axes BEFORE the
    // split-config stray-key gate sees the blob (the `preset` key is not a
    // public_config field).
    let mut config = req.config.clone();
    expand_capacity_preset(&req.resource_type, &mut config)?;
    let (public, secret) = split_config(descriptor, config)?;
    validate_datacenter_connection(&req.resource_type, &public, &secret)?;
    // Cell validation: reject incoherent capacity axis combinations; log the
    // non-fatal scale-mismatch warnings.
    for w in validate_capacity_axes(&req.resource_type, &public)? {
        tracing::warn!(path = %req.path, "capacity axes warning: {w}");
    }

    let resource_id = id_override.unwrap_or_else(Uuid::new_v4);
    let version = 1;
    let vault_path = vault_path_for(workspace_id, resource_id, version);
    let display_name = req
        .display_name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| req.path.clone());

    // docs/20 §2: every resource carries a polymorphic owner. Placement defaults
    // to `workspace` (scope_id = workspace_id); `folder`/`template` make it
    // non-workspace-wide and become the object-ACL inheritance parent. The
    // transitional `workspace_id` column is kept in lockstep so legacy reads +
    // the old unique constraint still work.
    let scope_kind = req
        .scope_kind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("workspace");
    let scope_id = match scope_kind {
        // Platform rows store workspace_id = scope_id = PLATFORM_SCOPE_ID (the
        // caller already forced workspace_id to the sentinel + gated on the
        // admin flag).
        "platform" => PLATFORM_SCOPE_ID,
        "workspace" => workspace_id,
        "folder" | "template" => req.scope_id.ok_or_else(|| {
            ApiError::bad_request(format!(
                "scope_id is required for scope_kind '{scope_kind}'"
            ))
        })?,
        other => {
            return Err(ApiError::bad_request(format!(
                "unknown scope_kind '{other}' — expected workspace | folder | template"
            )))
        }
    };
    let restricted = req.restricted.unwrap_or(false);

    // Lay down `resources` first — its UNIQUE(scope_kind, scope_id, path)
    // constraint is the canonical conflict gate.
    let insert_resource = sqlx::query(
        "INSERT INTO resources \
            (id, workspace_id, path, resource_type, display_name, latest_version, created_by, \
             updated_by, scope_kind, scope_id, restricted) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $7, $8, $9, $10)",
    )
    .bind(resource_id)
    .bind(workspace_id)
    .bind(&req.path)
    .bind(&req.resource_type)
    .bind(&display_name)
    .bind(version)
    .bind(principal_id)
    .bind(scope_kind)
    .bind(scope_id)
    .bind(restricted)
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

    // Grant the creator `read` so the legacy resolver works out of the box.
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

    // Object-ACL: the creator owns the resource. Essential for a `restricted`
    // resource (no ws floor → without this grant the creator couldn't even read
    // back what they just made); harmless otherwise.
    let _ = apply_grant(
        &state.db,
        workspace_id,
        ObjectKind::Resource,
        resource_id,
        principal_id,
        Role::Owner,
        principal_id,
    )
    .await;

    write_audit(
        &state.db,
        resource_id,
        version,
        principal_id,
        AuditAction::Create,
    )
    .await?;

    // R3/R4b: if this resource's axes resolve to a net-backed capacity backend
    // (Tokens / Presence / Scheduler), deploy its backing net (idempotent,
    // engine-down-tolerant). Runs after the resource is durably persisted.
    ensure_pool_net_for_resource(
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
        created_by: principal_id,
        updated_by: Some(principal_id),
        dynamic_keys,
        // The list endpoint surfaces `public_config` for capacity rows (the
        // picker's discriminator); the single-row create/update/rotate returns
        // are not the picker's source, so they omit it.
        public_config: None,
        // Stamped by the HTTP create handler (the creator owns it).
        my_effective_role: None,
        restricted,
    };
    Ok(summary)
}

/// Re-provision the secret half of an already-existing resource from the given
/// config, re-writing its v1 `vault_path`.
///
/// Exists for the dev demo seeder: the secret backend in `just dev` is the
/// in-memory Vault, which is wiped on every `down`/`up` — but the Postgres
/// `resources` / `resource_versions` rows survive, so the seeder's
/// "already present → leave as-is" idempotency would otherwise leave a demo
/// resource pointing at an empty Vault entry (`secret not found …#password`).
/// Re-asserting the fixture's secret each boot self-heals that without
/// touching the DB row (config / version / ACL are left exactly as-is).
///
/// Two guards keep this from clobbering deliberate edits:
/// - No-op when the type has no secret fields (e.g. a `datacenter` with an
///   inline-only connection, or a `loki` with the token unset) — the resolver
///   never templates an absent secret (see SECRET_KEYS_MARKER), so a missing
///   Vault entry is harmless there.
/// - Only acts when the resource is still at `latest_version == 1`. A user who
///   rotated the secret through the UI has a v2+ whose value the fixture no
///   longer knows; leave that alone. The common case — a demo resource never
///   rotated — is exactly v1, and writing the fixture value back is either a
///   heal (Vault was wiped) or a no-op (identical bytes).
///
/// Best-effort: errors are returned for the caller to log, never fatal.
pub(crate) async fn reprovision_resource_secret(
    state: &AppState,
    req: &CreateResourceRequest,
    workspace_id: Uuid,
) -> Result<(), ApiError> {
    let descriptor = descriptor_or_400(&req.resource_type)?;
    // No-secret types (e.g. `capacity`, whose only config is the `preset`
    // shorthand the create path expands) carry nothing to re-provision. Bail
    // BEFORE split_config — it doesn't expand the `preset` sugar and would
    // 400 on the stray key, turning a guaranteed no-op into seed noise.
    if !descriptor.dynamic_fields && descriptor.secret_fields.is_empty() {
        return Ok(());
    }
    let (_public, secret) = split_config(descriptor, req.config.clone())?;
    if secret.is_empty() {
        return Ok(());
    }

    let row: Option<(Uuid, i32)> = sqlx::query_as(
        "SELECT id, latest_version FROM resources \
         WHERE workspace_id = $1 AND path = $2 AND deleted_at IS NULL",
    )
    .bind(workspace_id)
    .bind(&req.path)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;
    let Some((resource_id, latest_version)) = row else {
        return Ok(()); // raced away — nothing to heal
    };
    if latest_version != 1 {
        // Rotated/edited since seed — the fixture isn't the source of truth
        // for this version's secret anymore. Don't touch it.
        return Ok(());
    }

    let vault_path = vault_path_for(workspace_id, resource_id, latest_version);
    state
        .resource_store
        .put_version(&vault_path, &secret)
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                format!("secret backend write failed: {e}"),
            )
        })?;
    Ok(())
}

/// R3/R4b hook: for a pool-backed resource, (re)deploy its backing net
/// `pool-<id>` to the engine. No-op for every non-pool resource. Idempotent +
/// engine-down-tolerant. Called on create AND version bump so a config change
/// (capacity amount, allocator url/token) re-deploys the net.
///
/// This is fully axes-driven: it resolves the resource's axes through the SINGLE
/// dispatch authority ([`crate::models::capacity::axes_for_resource`] →
/// [`crate::models::capacity::CapacityBackend`]) and dispatches on the
/// [`CapacityBackend`], NOT on a `match resource_type` string switch. A
/// `capacity` parses its `public_config`; a `datacenter` returns its locked lease
/// axes (→ `Scheduler`). Every other kind resolves to `None` and is a no-op.
///
/// - [`CapacityBackend::Tokens`] → [`crate::petri::pool_net::ensure_token_pool_net_deployed`]
///   (seeded count from `capacity_amount` in `public_config`).
/// - [`CapacityBackend::Presence`] → [`crate::petri::presence_pool_net::ensure_presence_pool_net_deployed`]
///   (capacity-less — the net seeds nothing; the presence controller
///   injects/expires units at runtime).
/// - [`CapacityBackend::Scheduler`] → [`crate::petri::pool_net::ensure_datacenter_adapter_deployed`]
///   (the `datacenter` kind: `allocator_url` from `public_config`; the token as a
///   `{{secret:<vault_path>#token}}` template the engine resolves at fire time —
///   `vault_path` from `(workspace_id, resource_id, version)`).
/// - [`CapacityBackend::Queue`] → no admission net (a worker queue has no
///   per-task matcher — workers subscribe and compete on the broker).
async fn ensure_pool_net_for_resource(
    state: &AppState,
    resource_type: &str,
    workspace_id: Uuid,
    resource_id: Uuid,
    version: i32,
    public: &JsonMap<String, Value>,
) {
    use crate::models::capacity::{axes_for_resource, CapacityBackend};

    let Some(axes) = axes_for_resource(resource_type, public) else {
        // A non-pool resource is not a capacity at all.
        tracing::debug!(
            %resource_id,
            resource_type,
            "resource resolves to no admission backend; no pool-net deployed"
        );
        return;
    };

    match axes.backend() {
        CapacityBackend::Tokens => {
            // The seeded count is the `Fixed(n)` unit count flattened into
            // `capacity_amount` (the `limit` preset's free axis). Absent/malformed
            // ⇒ skip the deploy (best-effort, same posture the old path had for a
            // malformed blob).
            let Some(capacity) = public
                .get("capacity_amount")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32)
            else {
                tracing::warn!(
                    %resource_id,
                    "seeded capacity has no numeric `capacity_amount` in public_config; \
                     skipping token pool-net deploy"
                );
                return;
            };
            crate::petri::pool_net::ensure_token_pool_net_deployed(
                &state.petri,
                workspace_id,
                resource_id,
                capacity,
            )
            .await;
        }
        CapacityBackend::Presence => {
            // A presence pool is capacity-LESS — its backing net seeds nothing and
            // reads no config. mekhan's presence controller injects one unit per
            // live runner (presence_acquire) and reaps it on presence-lease expiry
            // (presence_expired). Deploy is idempotent + engine-down-tolerant.
            //
            // The `acceptance` axis selects the admission discipline (doc 35 §4):
            // `Consent` parks a match-once offer that binds on a unit-initiated claim
            // (first-claim-wins, rest implicitly rescinded); `Auto` binds eagerly via
            // the direct-assign net.
            crate::petri::presence_pool_net::ensure_presence_pool_net_deployed(
                &state.petri,
                workspace_id,
                resource_id,
                axes.acceptance,
            )
            .await;
        }
        CapacityBackend::Scheduler => {
            // The `datacenter` kind. `scheduler_flavor` (public field) is the
            // discriminant the connection builder reads: `"slurm"` → SSH
            // salloc/scancel, `"nomad"` → Nomad API, `"http"` (default) →
            // POST/DELETE against `allocator_url`. The per-flavor connection fields
            // all ride ON the resource (docs/16 §1). Secret fields
            // (`token`/`ssh_key`/`nomad_token`) become `{{secret:<vault_path>#<field>}}`
            // templates the engine resolves at fire time — the shared
            // `from_public_config` builds the same shape the B-staging resolver
            // uses, so they can't drift.
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

            crate::petri::pool_net::ensure_datacenter_adapter_deployed(
                &state.petri,
                workspace_id,
                &conn,
            )
            .await;
        }
        CapacityBackend::Queue => {
            // A competing_consumer (pull) capacity has NO admission net (it is a
            // static partition / shared work queue — no per-task matcher).
            tracing::debug!(
                %resource_id,
                resource_type,
                "resource resolves to no admission backend; no pool-net deployed"
            );
        }
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
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ResourceDetail>, ApiError> {
    let row = sqlx::query_as::<_, ResourceRow>(
        "SELECT * FROM resources WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("resource not found"))?;

    // Platform rows are globally visible — read bypasses the workspace-local
    // ACL; the caller's role is `owner` (platform admin) else `viewer`. Tenant
    // rows keep the object-ACL read gate (folder cascade + override; ws floor
    // unless restricted; ws Owner/Admin bypass). 403 for a member without access.
    let role = if row.scope_kind == "platform" {
        if user.is_platform_admin {
            Role::Owner
        } else {
            Role::Viewer
        }
    } else {
        require_object_role(&state.db, &user, ObjectRef::resource(row.id), Role::Viewer)
            .await
            .map_err(map_to_api_error)?
    };

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
        created_by: row.created_by,
        updated_by: row.updated_by,
        public_config: version.public_config,
        redacted_secret_fields: descriptor
            .secret_fields
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        my_effective_role: Some(role.as_label().to_string()),
        restricted: row.restricted,
        scope_kind: row.scope_kind,
        scope_id: row.scope_id,
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

    // Platform rows: gate on the admin flag, bypass the workspace-local object
    // ACL entirely (it would 403 everyone). Tenant rows keep the Editor gate.
    let role = if row.scope_kind == "platform" {
        if !user.is_platform_admin {
            return Err(ApiError::forbidden(
                "platform-scoped resource writes require platform admin",
            ));
        }
        Role::Owner
    } else {
        require_object_role(&state.db, &user, ObjectRef::resource(row.id), Role::Editor)
            .await
            .map_err(map_to_api_error)?
    };

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
        sqlx::query(
            "UPDATE resources SET display_name = $1, updated_at = NOW(), updated_by = $2 \
             WHERE id = $3",
        )
        .bind(&trimmed)
        .bind(principal_id)
        .bind(row.id)
        .execute(&state.db)
        .await?;
        display_name = trimmed;
    }

    if let Some(mut config) = req.config {
        let descriptor = descriptor_or_400(&row.resource_type)?;
        expand_capacity_preset(&row.resource_type, &mut config)?;
        let (public, secret) = split_config(descriptor, config)?;
        for w in validate_capacity_axes(&row.resource_type, &public)? {
            tracing::warn!(path = %row.path, "capacity axes warning: {w}");
        }

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

        sqlx::query(
            "UPDATE resources SET latest_version = $1, updated_at = NOW(), updated_by = $2 \
             WHERE id = $3",
        )
        .bind(latest_version)
        .bind(principal_id)
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

        // R3/R4b: re-deploy the backing net on a pool-resource version bump so a
        // config change (capacity amount / allocator url+token) takes effect
        // (idempotent, engine-down-tolerant).
        ensure_pool_net_for_resource(
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
        created_by: row.created_by,
        updated_by: Some(principal_id),
        dynamic_keys,
        public_config: None,
        my_effective_role: Some(role.as_label().to_string()),
        restricted: row.restricted,
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

    // Platform rows: admin-flag gated, ACL bypassed. Tenant rows keep Editor.
    if row.scope_kind == "platform" {
        if !user.is_platform_admin {
            return Err(ApiError::forbidden(
                "platform-scoped resource writes require platform admin",
            ));
        }
    } else {
        require_object_role(&state.db, &user, ObjectRef::resource(row.id), Role::Editor)
            .await
            .map_err(map_to_api_error)?;
    }

    sqlx::query(
        "UPDATE resources SET deleted_at = NOW(), updated_at = NOW(), updated_by = $1 \
         WHERE id = $2",
    )
    .bind(user.subject_as_uuid())
    .bind(row.id)
    .execute(&state.db)
    .await?;

    // Object grants are polymorphic with no FK — drop them in the delete path
    // (mirrors folders/templates/instances cleanup).
    sqlx::query(
        "DELETE FROM object_grants WHERE object_type = 'resource'::object_kind AND object_id = $1",
    )
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

/// `PATCH /api/v1/resources/{id}/scope` — reparent a resource to a different
/// owner scope (docs/20 §2). Re-authorizes on BOTH sides: Editor on the resource
/// (object ACL) to move it out, and the `create_resource` placement gate on the
/// target. The resource keeps its `workspace_id`, version history, Vault paths,
/// and object grants — only `(scope_kind, scope_id)` (the inheritance parent)
/// changes. `path` must be free in the target scope (else 409).
#[utoipa::path(
    patch,
    path = "/api/v1/resources/{id}/scope",
    params(("id" = Uuid, Path, description = "Resource id")),
    request_body = crate::models::asset::MoveScopeRequest,
    responses(
        (status = 200, description = "Resource moved", body = ResourceSummary),
        (status = 403, description = "Editor role required on source or target", body = ErrorResponse),
        (status = 404, description = "Resource not found", body = ErrorResponse),
        (status = 409, description = "path already exists in target scope", body = ErrorResponse),
    ),
    tag = "resources",
)]
pub async fn move_resource(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<crate::models::asset::MoveScopeRequest>,
) -> Result<Json<ResourceSummary>, ApiError> {
    use crate::models::asset::ScopeKind;

    let row = sqlx::query_as::<_, ResourceRow>(
        "SELECT * FROM resources WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("resource not found"))?;

    let target_kind = req.scope_kind;

    // v1 rejects any move that CROSSES the platform<->tenant boundary in either
    // direction — platform resources are created directly as platform-scoped.
    // Same-boundary moves (tenant->tenant, or platform-internal) keep existing
    // behavior.
    let src_platform = row.scope_kind == "platform";
    let dst_platform = target_kind == ScopeKind::Platform;
    if src_platform != dst_platform {
        return Err(ApiError::bad_request(
            "cannot move resources across the platform boundary",
        ));
    }

    // Source gate. Platform source: admin-flag gated, ACL bypassed. Tenant
    // source: Editor on the resource (object ACL) to move it out.
    if src_platform {
        if !user.is_platform_admin {
            return Err(ApiError::forbidden(
                "platform-scoped resource writes require platform admin",
            ));
        }
    } else {
        require_object_role(&state.db, &user, ObjectRef::resource(row.id), Role::Editor)
            .await
            .map_err(map_to_api_error)?;
    }

    // Resolve + authorize the target scope (create_resource placement rule).
    let target_id = match target_kind {
        ScopeKind::Platform => PLATFORM_SCOPE_ID,
        ScopeKind::Workspace => req.scope_id.unwrap_or(row.workspace_id),
        _ => req.scope_id.ok_or_else(|| {
            ApiError::bad_request(format!(
                "scope_id is required for scope_kind '{}'",
                target_kind.as_db()
            ))
        })?,
    };
    match target_kind {
        // Platform target reachable only on a platform-internal move (the
        // cross-boundary guard above already rejected tenant->platform); gate on
        // the admin flag, bypass the workspace-local ACL.
        ScopeKind::Platform => {
            if !user.is_platform_admin {
                return Err(ApiError::forbidden(
                    "platform-scoped resource writes require platform admin",
                ));
            }
            Role::Owner
        }
        ScopeKind::Workspace => require_role(&state.db, &user, target_id, Role::Editor)
            .await
            .map_err(map_to_api_error)?,
        ScopeKind::Folder => {
            require_object_role(&state.db, &user, ObjectRef::folder(target_id), Role::Editor)
                .await
                .map_err(map_to_api_error)?
        }
        ScopeKind::Template => require_object_role(
            &state.db,
            &user,
            ObjectRef::template(target_id),
            Role::Editor,
        )
        .await
        .map_err(map_to_api_error)?,
    };

    if (target_kind.as_db(), Some(target_id)) != (row.scope_kind.as_str(), row.scope_id) {
        let res = sqlx::query(
            "UPDATE resources SET scope_kind = $1, scope_id = $2, updated_at = NOW(), \
                 updated_by = $3 \
             WHERE id = $4 AND deleted_at IS NULL",
        )
        .bind(target_kind.as_db())
        .bind(target_id)
        .bind(user.subject_as_uuid())
        .bind(row.id)
        .execute(&state.db)
        .await;
        if let Err(e) = res {
            if let Some(db_err) = e.as_database_error() {
                if db_err.is_unique_violation() {
                    return Err(ApiError::conflict(format!(
                        "resource path '{}' already exists in the target scope",
                        row.path
                    )));
                }
            }
            return Err(ApiError::internal(e.to_string()));
        }
    }

    // Rebuild the summary at the new scope, re-stamping the caller's role
    // (non-fatal: None if the move lands it somewhere they can't read, like list).
    let fresh = sqlx::query_as::<_, ResourceRow>(
        "SELECT * FROM resources WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    let dyn_keys = fetch_dynamic_keys(&state.db, std::slice::from_ref(&fresh)).await?;
    let capacity_public =
        fetch_capacity_public_config(&state.db, std::slice::from_ref(&fresh)).await?;
    let mut summary = rows_to_summaries(vec![fresh], &dyn_keys, &capacity_public)
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::internal("resource vanished after move"))?;
    let roles = effective_object_roles(
        &state.db,
        &user,
        ObjectKind::Resource,
        caller_workspace(&user)?,
        &[id],
    )
    .await
    .map_err(map_to_api_error)?;
    summary.my_effective_role = roles.get(&id).map(|r| r.as_label().to_string());
    Ok(Json(summary))
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

    // Platform rows: admin-flag gated, ACL bypassed. Tenant rows keep Editor.
    let role = if row.scope_kind == "platform" {
        if !user.is_platform_admin {
            return Err(ApiError::forbidden(
                "platform-scoped resource writes require platform admin",
            ));
        }
        Role::Owner
    } else {
        require_object_role(&state.db, &user, ObjectRef::resource(row.id), Role::Editor)
            .await
            .map_err(map_to_api_error)?
    };

    let descriptor = descriptor_or_400(&row.resource_type)?;
    let mut config = req.config;
    expand_capacity_preset(&row.resource_type, &mut config)?;
    let (public, secret) = split_config(descriptor, config)?;
    validate_datacenter_connection(&row.resource_type, &public, &secret)?;
    for w in validate_capacity_axes(&row.resource_type, &public)? {
        tracing::warn!(path = %row.path, "capacity axes warning: {w}");
    }

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

    sqlx::query(
        "UPDATE resources SET latest_version = $1, updated_at = NOW(), updated_by = $2 \
         WHERE id = $3",
    )
    .bind(new_version)
    .bind(principal_id)
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
        created_by: row.created_by,
        updated_by: Some(principal_id),
        dynamic_keys,
        public_config: None,
        my_effective_role: Some(role.as_label().to_string()),
        restricted: row.restricted,
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
