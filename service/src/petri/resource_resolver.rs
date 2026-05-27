//! `ResourceResolver` — Phase B.5.
//!
//! Turns an instance-level `alias -> ResourcePin` map into a JSON envelope
//! the launcher can splice into AIR. The envelope contains:
//!
//! - **Public fields inline** — copied straight from
//!   `resource_versions.public_config`. Steps read these directly
//!   (`db.host`, `db.port`).
//! - **Secret fields as templates** — `{{secret:<vault_path>#<field>}}`.
//!   These survive serialization and are wrapped/unwrapped by the existing
//!   `aithericon-secrets` kernel without modification.
//!
//! **What this module does NOT do**:
//! - Talk to Vault. The kernel does that.
//! - Mutate AIR. The launcher does that (B.7).
//! - Wire into `AppState`. The integration chunk does that.
//!
//! A `ResourceResolver` is constructible from any `PgPool` and the test
//! suite uses this property — no other state needed.
//!
//! ## ACL model (v1)
//!
//! Until `workspaces` / `workspace_members` tables exist, access is
//! **workspace-scoped**: any authenticated principal whose request reaches
//! this resolver may read any resource in the workspace they're acting in.
//! The workspace filter on the `resources` lookup is the effective gate;
//! the `resource_acl` table is *populated* on create (so v2 has historical
//! grants to start from) but is **not consulted on the read path**.
//!
//! This is the pragmatic stopgap. The per-principal grant model behaved as
//! "only the creator can use a resource" in the absence of a sharing UI,
//! which broke real collaboration the moment a colleague published a
//! template referencing your resource. v2 will reintroduce a real check
//! here as a `UNION` of workspace membership and per-principal ACL — the
//! resolver signature does not change.
//!
//! Audit rows are still written **after** all aliases resolve and
//! **before** the envelope is returned. A single failing alias (missing
//! version, unknown type, …) aborts the whole resolve and writes no audit
//! rows — there is no half-resolved state.

use std::collections::HashMap;
use std::fmt;

use serde_json::{Map as JsonMap, Value as JsonValue};
use sqlx::{PgPool, Postgres, Transaction};
use thiserror::Error;
use uuid::Uuid;

use aithericon_resources::{registry::lookup, ResourcePin};

use crate::compiler::resource_refs::KnownResources;
use crate::models::resource::{ResourceRow, ResourceVersionRow};

/// Per-call audit attribution. The resolver writes one `resource_audit` row
/// per resolved alias using the same context, so a launch-time resolve is
/// fully attributable in one query.
#[derive(Debug, Clone)]
pub struct AuditContext {
    /// Workflow instance the resolve is happening for. `None` for
    /// non-launch callers (CRUD handlers) — in v1 those still write audit
    /// rows but with `instance_id = NULL`.
    pub instance_id: Option<Uuid>,
    /// Step-level attribution. Always `None` in v1 because pins are
    /// instance-scoped (resolve happens once per launch, not once per step).
    /// Reserved for the per-step-audit v2 path.
    pub step_id: Option<String>,
    /// `api`, `launcher`, `oauth_refresher`, … — free-form so new call sites
    /// don't need a schema change.
    pub site: String,
    /// Caller principal — typically the user launching the workflow.
    pub principal_id: Uuid,
    /// What is being recorded. Constrained by [`AuditAction`].
    pub action: AuditAction,
}

/// Closed set of audit verbs. Mirrored on the wire as snake_case strings;
/// the audit table uses `TEXT` with no CHECK so forward-compat verbs only
/// require a code change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditAction {
    Resolve,
    Create,
    Update,
    Rotate,
    Delete,
    OauthRefresh,
}

impl AuditAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resolve => "resolve",
            Self::Create => "create",
            Self::Update => "update",
            Self::Rotate => "rotate",
            Self::Delete => "delete",
            Self::OauthRefresh => "oauth_refresh",
        }
    }
}

impl fmt::Display for AuditAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// All failure modes surfaced by [`ResourceResolver::resolve`]. Wraps
/// `sqlx::Error` directly rather than mapping every DB failure into a
/// domain error — the caller's HTTP layer handles 500s uniformly.
#[derive(Debug, Error)]
pub enum ResolverError {
    /// The `(workspace_id, resource_id)` join produced no row. Either the
    /// resource never existed, was soft-deleted, or belongs to a different
    /// workspace than the one issuing the resolve.
    #[error("resource not found: {resource_id} (workspace mismatch or soft-deleted)")]
    ResourceNotFound { resource_id: Uuid },

    /// The path-keyed variant — surfaced when launchers/handlers look up
    /// resources by path rather than id (e.g. alias binding).
    #[error("resource not found at path `{path}`")]
    ResourceNotFoundAtPath { path: String },

    /// The pin pointed at a version row that doesn't exist. Should be
    /// impossible against a healthy DB (pins are taken from `latest_version`)
    /// but stays explicit because B.7 pins survive in instance JSONB
    /// indefinitely.
    #[error("resource {resource_id} has no version {version}")]
    VersionNotFound { resource_id: Uuid, version: i32 },

    /// No matching `resource_acl` row with `permission = 'read'`. The
    /// resolve is aborted; no audit rows are written.
    #[error("principal {principal_id} denied read access to resource {resource_id}")]
    AclDenied {
        resource_id: Uuid,
        principal_id: Uuid,
    },

    /// The resource row's `resource_type` doesn't match any built-in type
    /// in `aithericon_resources::registry`. Either the DB carries a stale
    /// type from before a schema migration, or someone created the row
    /// without going through the typed CRUD path.
    #[error("unknown resource type `{type_name}` — not registered in aithericon_resources")]
    UnknownResourceType { type_name: String },

    /// A required public field declared by the type descriptor is missing
    /// from `public_config`. Surfaces stale data (the type's schema changed
    /// after the resource was created) rather than silently returning a
    /// hole-shaped envelope.
    #[error("resource {resource_id} missing required public field `{missing_field}`")]
    IncompletePublicConfig {
        resource_id: Uuid,
        missing_field: String,
    },

    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Stateless service handle. Cheap to clone (Postgres pools are themselves
/// `Arc`-shaped) and `Send + Sync`.
#[derive(Clone)]
pub struct ResourceResolver {
    db: PgPool,
}

impl ResourceResolver {
    /// Construct directly from a `PgPool`. The integration chunk wires this
    /// into `AppState` and the launcher; B.5 leaves construction
    /// dependency-free so tests can stand up an instance in two lines.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// Borrow the pool — useful for tests that want to seed rows on the
    /// same database the resolver reads.
    pub fn pool(&self) -> &PgPool {
        &self.db
    }

    /// Resolve every alias in `bindings`.
    ///
    /// Behavior:
    /// 1. For each pin, run the joined `resources + resource_versions +
    ///    resource_acl` lookup in a single statement.
    /// 2. Validate the result against `aithericon_resources::registry`
    ///    (unknown type → abort).
    /// 3. Compose the per-alias subtree: public fields inline, secret fields
    ///    as `{{secret:<vault_path>#<field>}}` templates.
    /// 4. If every alias resolved cleanly, open a transaction, write
    ///    `resource_audit` rows, and commit. Any error rolls back so the
    ///    audit table never carries audit rows for failed resolves.
    /// 5. Return the combined envelope as `Object({ alias -> subtree })`.
    pub async fn resolve(
        &self,
        workspace_id: Uuid,
        principal_id: Uuid,
        bindings: &HashMap<String, ResourcePin>,
        audit_ctx: AuditContext,
    ) -> Result<JsonValue, ResolverError> {
        let mut envelope = JsonMap::with_capacity(bindings.len());
        // Capture (resource_id, version) for the audit pass once every alias
        // is known to be valid. Avoids a second round-trip per alias.
        let mut audit_targets: Vec<(Uuid, i32)> = Vec::with_capacity(bindings.len());

        for (alias, pin) in bindings {
            let subtree =
                self.resolve_one(workspace_id, principal_id, pin).await?;
            envelope.insert(alias.clone(), JsonValue::Object(subtree));
            audit_targets.push((pin.resource_id, pin.version));
        }

        self.write_audit(&audit_targets, &audit_ctx).await?;

        Ok(JsonValue::Object(envelope))
    }

    // ── private helpers ────────────────────────────────────────────────────

    /// Resolve a single pin into the per-alias subtree. All DB work for one
    /// alias happens here so the per-alias error paths in [`resolve`] are
    /// clean.
    async fn resolve_one(
        &self,
        workspace_id: Uuid,
        principal_id: Uuid,
        pin: &ResourcePin,
    ) -> Result<JsonMap<String, JsonValue>, ResolverError> {
        // (1) Load the resource row. Workspace + soft-delete filter inline so
        // a wrong-workspace or deleted resource is indistinguishable from
        // "doesn't exist" at the API surface.
        let resource: Option<ResourceRow> = sqlx::query_as::<_, ResourceRow>(
            "SELECT id, workspace_id, path, resource_type, display_name, \
                    latest_version, deleted_at, created_by, created_at, updated_at \
             FROM resources \
             WHERE id = $1 AND workspace_id = $2 AND deleted_at IS NULL",
        )
        .bind(pin.resource_id)
        .bind(workspace_id)
        .fetch_optional(&self.db)
        .await?;

        let resource = resource.ok_or(ResolverError::ResourceNotFound {
            resource_id: pin.resource_id,
        })?;

        // (2) Look the descriptor up — unknown types fail fast.
        let descriptor = lookup(&resource.resource_type).ok_or_else(|| {
            ResolverError::UnknownResourceType {
                type_name: resource.resource_type.clone(),
            }
        })?;

        // (3) ACL check: workspace-scoped trust until `workspaces` /
        // `workspace_members` land. The resource row was already filtered
        // by `workspace_id` in step (1), and the caller is by definition an
        // authenticated principal in that workspace, so we grant read here
        // without consulting `resource_acl`. The table is still populated
        // on create (see `handlers::resources::create_resource`) so v2 can
        // promote it from "auto-grant" to "additional grants on top of
        // workspace membership" without a backfill. `principal_id` is
        // unused on this path but kept in the signature so the v2
        // `UNION`-based check is a body change, not a signature change.
        let _ = principal_id;

        // (4) Load the pinned version row.
        let version: Option<ResourceVersionRow> = sqlx::query_as::<_, ResourceVersionRow>(
            "SELECT resource_id, version, vault_path, public_config, created_by, created_at \
             FROM resource_versions \
             WHERE resource_id = $1 AND version = $2",
        )
        .bind(pin.resource_id)
        .bind(pin.version)
        .fetch_optional(&self.db)
        .await?;

        let version = version.ok_or(ResolverError::VersionNotFound {
            resource_id: pin.resource_id,
            version: pin.version,
        })?;

        // (5) Compose the subtree.
        let public_obj = version.public_config.as_object().ok_or_else(|| {
            // A non-object `public_config` is corruption — the type
            // descriptor's contract guarantees keyed fields. Surface as
            // IncompletePublicConfig with a synthetic missing_field token so
            // operators can grep for it.
            ResolverError::IncompletePublicConfig {
                resource_id: pin.resource_id,
                missing_field: "<entire public_config is not a JSON object>".to_string(),
            }
        })?;

        // Dynamic-fields fast path: the descriptor has no static field
        // lists; iterate the user-defined keys stashed in
        // `public_config.__kv_keys` and emit a secret template for each.
        // All values live in Vault — there is no public-side data to
        // inline.
        if descriptor.dynamic_fields {
            let keys = public_obj
                .get("__kv_keys")
                .and_then(JsonValue::as_array)
                .ok_or_else(|| ResolverError::IncompletePublicConfig {
                    resource_id: pin.resource_id,
                    missing_field: "__kv_keys".to_string(),
                })?;
            let mut subtree = JsonMap::with_capacity(keys.len());
            for k in keys {
                let Some(name) = k.as_str() else {
                    return Err(ResolverError::IncompletePublicConfig {
                        resource_id: pin.resource_id,
                        missing_field: "<non-string entry in __kv_keys>".to_string(),
                    });
                };
                let template = format!("{{{{secret:{}#{}}}}}", version.vault_path, name);
                subtree.insert(name.to_string(), JsonValue::String(template));
            }
            return Ok(subtree);
        }

        let mut subtree = JsonMap::with_capacity(
            descriptor.public_fields.len() + descriptor.secret_fields.len(),
        );

        for field_name in descriptor.public_fields {
            if let Some(v) = public_obj.get(*field_name) {
                // `Null` is treated as present-but-unset (matches how the
                // type derives use `Option<T>`). Required-vs-optional
                // semantics live in the JSON Schema produced by schemars;
                // the resolver itself does not enforce them on the field
                // level — it only enforces that the keyed slot exists.
                subtree.insert((*field_name).to_string(), v.clone());
            } else {
                // Field absent entirely. The plan calls for omit-if-optional,
                // fail-if-required, but the type descriptor doesn't yet
                // carry per-field requiredness. Conservative choice: pass
                // through `Null`. Required-field validation belongs in the
                // CRUD handler (B.9), not the resolver.
                subtree.insert((*field_name).to_string(), JsonValue::Null);
            }
        }

        for field_name in descriptor.secret_fields {
            // {{secret:<vault_path>#<field>}} — the existing
            // `extract_secret_keys` regex captures the whole key
            // (verified in `shared/resources/tests/registry.rs`).
            let template = format!(
                "{{{{secret:{}#{}}}}}",
                version.vault_path, field_name
            );
            subtree.insert((*field_name).to_string(), JsonValue::String(template));
        }

        Ok(subtree)
    }

    /// Publish-time variant of [`resolve`]. Takes the compiler's
    /// [`KnownResources`] map directly (keyed by workspace resource name)
    /// and projects it into the `HashMap<String, ResourcePin>` shape the
    /// inner resolver expects. Returns the JSON envelope ready for splicing
    /// into the AIR — same shape as `resolve` (`{ name: { ...inline...,
    /// ...secret_refs... } }`).
    ///
    /// One audit row is written per known resource with action
    /// [`AuditAction::Resolve`] and `site = "publish"`.
    pub async fn resolve_known(
        &self,
        workspace_id: Uuid,
        principal_id: Uuid,
        known: &KnownResources,
        instance_id: Option<Uuid>,
    ) -> Result<JsonValue, ResolverError> {
        let mut bindings: HashMap<String, ResourcePin> = HashMap::with_capacity(known.len());
        for (name, info) in known {
            bindings.insert(
                name.clone(),
                ResourcePin {
                    resource_id: info.id,
                    version: info.latest_version,
                },
            );
        }
        self.resolve(
            workspace_id,
            principal_id,
            &bindings,
            AuditContext {
                instance_id,
                step_id: None,
                site: "publish".to_string(),
                principal_id,
                action: AuditAction::Resolve,
            },
        )
        .await
    }

    /// Write one audit row per resolved alias inside a single transaction.
    /// All-or-nothing: a midway failure rolls back so the audit table
    /// never partially reflects an aborted resolve.
    async fn write_audit(
        &self,
        targets: &[(Uuid, i32)],
        ctx: &AuditContext,
    ) -> Result<(), ResolverError> {
        if targets.is_empty() {
            return Ok(());
        }
        let mut tx: Transaction<'_, Postgres> = self.db.begin().await?;
        for (resource_id, version) in targets {
            sqlx::query(
                "INSERT INTO resource_audit \
                    (instance_id, step_id, resource_id, resource_version, \
                     principal_id, action, site) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(ctx.instance_id)
            .bind(ctx.step_id.as_deref())
            .bind(resource_id)
            .bind(version)
            .bind(ctx.principal_id)
            .bind(ctx.action.as_str())
            .bind(&ctx.site)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

/// Splice `let __resources = #{ ... };` at the top of every prepare
/// transition whose Rhai logic references any of the workspace resource
/// names. One declaration per transition with **all** referenced names
/// inside it — never a duplicate.
///
/// Called at publish time (not launch) so the AIR persisted in
/// `workflow_template_versions.air_json` already carries the spliced
/// envelope. Idempotent against a repeat call — a `let __resources`
/// declaration already present in `logic.source` short-circuits the splice.
pub fn splice_resources_into_air(
    mut air: JsonValue,
    envelope: &JsonValue,
    names: &[&str],
) -> JsonValue {
    let rhai_decl = build_resources_decl(envelope, names);
    if rhai_decl.is_empty() {
        return air;
    }

    let Some(transitions) = air.get_mut("transitions").and_then(|t| t.as_array_mut()) else {
        return air;
    };

    for t in transitions {
        let Some(t_obj) = t.as_object_mut() else {
            continue;
        };

        // Single source of truth in `compiler::borrow::apply::
        // has_prepare_transition_suffix` — kept aligned with the borrow
        // apply phase so a new lowering that adds a fourth prep-shape
        // can't drift between the two callers.
        let is_prepare = t_obj
            .get("id")
            .and_then(JsonValue::as_str)
            .map(crate::compiler::borrow::apply::has_prepare_transition_suffix)
            .unwrap_or(false);
        if !is_prepare {
            continue;
        }

        let Some(logic) = t_obj.get_mut("logic") else {
            continue;
        };
        let Some(logic_obj) = logic.as_object_mut() else {
            continue;
        };
        let Some(source) = logic_obj.get("source").and_then(JsonValue::as_str) else {
            continue;
        };
        let source = source.to_owned();

        // Only splice into transitions whose logic actually references a
        // known name. Avoids polluting unrelated prepare transitions.
        let references_any = names.iter().any(|n| {
            source.contains(&format!("__resources[\"{n}\"]"))
                || source.contains(&format!("__resources['{n}']"))
        });
        if !references_any {
            continue;
        }

        // Idempotent guard.
        if source.contains("let __resources") {
            continue;
        }

        let new_source = format!("{rhai_decl}\n{source}", source = source);
        logic_obj.insert("source".to_string(), JsonValue::String(new_source));
    }

    air
}

/// Build `let __resources = #{ "name": #{ ... }, ... };` from the resolver's
/// JSON envelope. Public fields are emitted as Rhai literals; secret-template
/// strings remain strings (the executor's `extract_secret_keys` regex picks
/// them up at staging time).
///
/// Optional fields that the user didn't set arrive here as `JsonValue::Null`
/// (the resolver inlines null for every declared `public_fields` slot — see
/// `resolve_one`'s loop). Rhai has no `null` keyword, so we drop null
/// **object fields** entirely (matches `Option::None` / serde
/// `skip_serializing_if` semantics) and emit Rhai's unit `()` in array
/// positions where null can't simply be removed without shifting indices.
fn build_resources_decl(envelope: &JsonValue, names: &[&str]) -> String {
    let JsonValue::Object(top) = envelope else {
        return String::new();
    };
    let mut entries: Vec<String> = Vec::with_capacity(names.len());
    for name in names {
        let Some(subtree) = top.get(*name) else {
            continue;
        };
        let Some(subtree_obj) = subtree.as_object() else {
            continue;
        };
        let mut field_entries: Vec<String> = Vec::with_capacity(subtree_obj.len());
        for (k, v) in subtree_obj {
            if v.is_null() {
                // Drop unset optional fields. Downstream readers (LLM
                // overlay_resource, Python AccessibleDict) treat missing
                // and null identically; emitting them as a Rhai literal
                // would crash the prepare transition with "'null' is a
                // reserved keyword" at parse time.
                continue;
            }
            field_entries.push(format!(
                "\"{}\": {}",
                escape_rhai_key(k),
                json_to_rhai_literal(v),
            ));
        }
        entries.push(format!(
            "\"{name}\": #{{ {body} }}",
            name = escape_rhai_key(name),
            body = field_entries.join(", "),
        ));
    }
    if entries.is_empty() {
        return String::new();
    }
    format!("let __resources = #{{ {} }};", entries.join(", "))
}

/// Render a `JsonValue` as a Rhai-parseable literal.
///
/// JSON and Rhai share syntax for booleans, numbers, strings, and arrays —
/// the only divergence is `null` (which Rhai spells `()`). For object fields
/// `build_resources_decl` filters nulls before calling here; this function
/// only sees nulls inside nested arrays / objects, where it emits `()` so the
/// surrounding positions stay correct.
fn json_to_rhai_literal(v: &JsonValue) -> String {
    match v {
        JsonValue::Null => "()".to_string(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Number(n) => n.to_string(),
        JsonValue::String(s) => {
            serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
        }
        JsonValue::Array(items) => {
            let parts: Vec<String> = items.iter().map(json_to_rhai_literal).collect();
            format!("[{}]", parts.join(", "))
        }
        JsonValue::Object(obj) => {
            let entries: Vec<String> = obj
                .iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| {
                    format!("\"{}\": {}", escape_rhai_key(k), json_to_rhai_literal(v))
                })
                .collect();
            format!("#{{ {} }}", entries.join(", "))
        }
    }
}

fn escape_rhai_key(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod splice_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_resources_decl_basic() {
        let env = json!({
            "local_pg": {
                "host": "h",
                "port": 5432,
                "password": "{{secret:resources/aaa/v1#password}}"
            }
        });
        let decl = build_resources_decl(&env, &["local_pg"]);
        assert!(decl.starts_with("let __resources = #{ "));
        assert!(decl.contains("\"local_pg\": #{"));
        assert!(decl.contains("\"host\": \"h\""));
        assert!(decl.contains("\"port\": 5432"));
        assert!(decl.contains("\"password\": \"{{secret:resources/aaa/v1#password}}\""));
        assert!(decl.ends_with(" };"));
    }

    #[test]
    fn build_resources_decl_empty_envelope_is_empty() {
        let env = json!({});
        assert_eq!(build_resources_decl(&env, &["local_pg"]), "");
    }

    /// Optional public fields the user left blank arrive at the resolver as
    /// `JsonValue::Null` (see `resolve_one`'s `else` branch). They MUST NOT
    /// be emitted as the literal `null` — Rhai has no `null` keyword and
    /// parsing the prepare transition would fail with
    /// `'null' is a reserved keyword`. Real-world trigger: an `openai`
    /// resource bound to an LLM step where `organization` is left blank.
    #[test]
    fn build_resources_decl_skips_null_object_fields() {
        let env = json!({
            "openai_prod": {
                "api_key": "{{secret:aithericon/resources/x/v1#api_key}}",
                "organization": null,
                "base_url": null,
            }
        });
        let decl = build_resources_decl(&env, &["openai_prod"]);
        assert!(
            !decl.contains("null"),
            "no `null` literal must escape into the Rhai source: {decl}"
        );
        assert!(decl.contains("\"api_key\":"));
        assert!(!decl.contains("\"organization\""));
        assert!(!decl.contains("\"base_url\""));
    }

    /// Nested null elements inside arrays cannot be dropped without shifting
    /// indices, so they round-trip as Rhai's unit `()`.
    #[test]
    fn json_to_rhai_literal_array_nulls_become_unit() {
        let v = json!([1, null, "x", null]);
        assert_eq!(json_to_rhai_literal(&v), "[1, (), \"x\", ()]");
    }

    /// Nested object nulls follow the same drop rule as top-level.
    #[test]
    fn json_to_rhai_literal_nested_object_nulls_dropped() {
        let v = json!({"outer": {"set": "v", "unset": null}});
        let s = json_to_rhai_literal(&v);
        assert!(s.contains("\"set\": \"v\""));
        assert!(!s.contains("\"unset\""));
        assert!(!s.contains("null"));
    }

    #[test]
    fn splice_skips_non_prepare() {
        let air = json!({
            "transitions": [
                {
                    "id": "t_x_consume",
                    "logic": { "type": "Rhai", "source": "__resources[\"local_pg\"]" }
                }
            ]
        });
        let env = json!({ "local_pg": { "host": "h" } });
        let out = splice_resources_into_air(air, &env, &["local_pg"]);
        let src = out["transitions"][0]["logic"]["source"].as_str().unwrap();
        assert!(!src.contains("let __resources"));
    }

    #[test]
    fn splice_inserts_once_per_prepare() {
        let air = json!({
            "transitions": [
                {
                    "id": "t_step_prepare",
                    "logic": {
                        "type": "Rhai",
                        "source": "job_inputs.push(#{ \"name\": \"local_pg.json\", \"source\": #{ \"type\": \"inline\", \"value\": __resources[\"local_pg\"] } });"
                    }
                }
            ]
        });
        let env = json!({ "local_pg": { "host": "h", "port": 5432 } });
        let out = splice_resources_into_air(air, &env, &["local_pg"]);
        let src = out["transitions"][0]["logic"]["source"].as_str().unwrap();
        assert!(src.contains("let __resources = #{"));
        assert!(src.contains("\"host\": \"h\""));
        // Idempotent — running again doesn't double-splice.
        let env2 = json!({ "local_pg": { "host": "h", "port": 5432 } });
        let out2 = splice_resources_into_air(out, &env2, &["local_pg"]);
        let src2 = out2["transitions"][0]["logic"]["source"].as_str().unwrap();
        assert_eq!(src2.matches("let __resources").count(), 1);
    }
}

