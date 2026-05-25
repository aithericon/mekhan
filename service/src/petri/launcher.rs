//! Instance-launch seam.
//!
//! `handlers::instances::create_instance` (user POST) and
//! `triggers::dispatcher::fire_spawn` (a Spawn trigger firing) both ran the
//! identical sequence: parameterize the template's AIR, INSERT the
//! `workflow_instances` row *before* deploying (so the lifecycle listener can
//! find it if the net finishes first), deploy to petri-lab, and on a deploy
//! failure DELETE the row so lifecycle never observes a phantom.
//!
//! That ordering — and especially the rollback-on-deploy-failure invariant —
//! lived twice, once in an HTTP handler and once in the trigger dispatcher.
//! The dispatcher additionally reached directly into the `petri::instance`
//! free functions. [`InstanceLauncher`] owns the sequence once; both callers
//! depend on this seam instead of re-implementing it.
//!
//! ## Phase B.7 — resource binding step
//!
//! For workflows that declare `resources: { alias: type }`, the launcher now
//! runs three extra stages between `parameterize_air` and the deploy:
//!
//! 1. **Bind aliases.** Each `(alias -> path)` from the caller is resolved
//!    against the `resources` table (workspace-scoped, soft-delete filtered)
//!    into a `(resource_id, latest_version)` pin. Missing paths and missing
//!    bindings are caller errors (400).
//! 2. **Resolve.** [`ResourceResolver::resolve`] turns the pins into a JSON
//!    envelope — public fields inline + `{{secret:resources/.../#field}}`
//!    templates. Audit rows are written transactionally.
//! 3. **Splice into AIR.** Every prepare transition whose Rhai logic
//!    references one of the resource aliases gets a `let __resources = #{
//!    ... };` declaration inserted at the top. The borrow-apply step already
//!    emitted `job_inputs.push(... __resources["<alias>"] ...)` snippets for
//!    each alias the Python source uses (B.8), so the splice closes the
//!    loop. The instance row's new `resource_pins` JSONB column captures
//!    `{ alias: { resource_id, version } }` so future replay / debugging
//!    has the exact pin without re-querying.
//!
//! Pins are immutable for the instance's lifetime. Rotation after launch
//! affects new instances only, never running ones.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Map as JsonMap, Value};
use sqlx::PgPool;
use uuid::Uuid;

use aithericon_resources::ResourcePin;

use crate::models::instance::{StartToken, WorkflowInstance};
use crate::models::template::WorkflowGraph;
use crate::petri::client::PetriClient;
use crate::petri::instance::{deploy_instance, parameterize_air, ParameterizeError};
use crate::petri::resource_resolver::{
    AuditAction, AuditContext, ResolverError, ResourceResolver,
};

/// Why a launch failed. Each caller maps these to its own surface:
/// `create_instance` turns [`LaunchError::Parameterize`] / [`LaunchError::Resource`]
/// into a 400 and [`LaunchError::Deploy`] into a 502; `fire_spawn` folds them
/// into `TriggerError::InstanceFailed`. The launcher itself is surface-agnostic.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// `parameterize_air` rejected the start tokens (missing/unknown/duplicate
    /// start block, wrong field kind, ...). No row was inserted.
    #[error(transparent)]
    Parameterize(#[from] ParameterizeError),

    /// Phase B.7 — resource binding / resolution failed. No row was
    /// inserted, no deploy attempted. Caller-facing (400).
    #[error(transparent)]
    Resource(#[from] ResourceBindError),

    /// The instance row could not be inserted. Nothing was deployed.
    #[error("instance row insert failed: {0}")]
    Database(String),

    /// petri-lab deploy failed. The just-inserted row has already been rolled
    /// back so the lifecycle listener never observes a never-deployed
    /// instance.
    #[error("deploy failed: {0}")]
    Deploy(String),
}

/// Phase B.7 errors. Surfaced via [`LaunchError::Resource`].
#[derive(Debug, thiserror::Error)]
pub enum ResourceBindError {
    /// The workflow declared `resources: { db: postgres }` but the caller
    /// didn't supply a `resource_bindings.db` entry.
    #[error("workflow declares resource alias '{alias}' but no binding was supplied")]
    MissingResourceBinding { alias: String },

    /// The caller supplied a binding for an alias the workflow doesn't
    /// declare. Reject (rather than silently drop) so typos in the
    /// `resource_bindings` map surface immediately.
    #[error(
        "binding for unknown resource alias '{alias}' — \
         template does not declare this alias"
    )]
    UnknownResourceAlias { alias: String },

    /// `bind_aliases` couldn't find a live `resources` row matching
    /// `(workspace_id, path)`. Either a typo or the resource was
    /// soft-deleted.
    #[error("resource path '{path}' not found for alias '{alias}'")]
    ResourcePathNotFound { alias: String, path: String },

    /// Workflow declares resources but no workspace context was supplied
    /// to the launch. Indicates a wiring bug at the call site (handlers /
    /// dispatcher) — the API surface should always carry a workspace.
    #[error(
        "workflow declares {alias_count} resource alias(es) but no workspace_id \
         was supplied — cannot resolve"
    )]
    MissingWorkspace { alias_count: usize },

    #[error(transparent)]
    Resolver(#[from] ResolverError),

    #[error("database error during resource bind: {0}")]
    Database(String),
}

/// What the caller wants run. `created_by` and `metadata` are the only inputs
/// that genuinely differ between the user-POST and trigger-fire paths, so they
/// stay parameters; everything else (parameterize → insert → deploy →
/// rollback) is owned by the launcher.
pub struct LaunchSpec<'a> {
    pub instance_id: Uuid,
    pub net_id: String,
    pub template_id: Uuid,
    pub template_version: i32,
    pub created_by: Uuid,
    /// Audit-only blob stored on the instance row (not merged into tokens).
    pub metadata: Value,
    pub air_json: &'a Value,
    pub graph: &'a WorkflowGraph,
    pub start_tokens: &'a [StartToken],
    /// Phase B.7 — caller-supplied `alias -> resource_path` map. Empty for
    /// workflows that don't declare any resources. Validated against
    /// `graph.resources` inside `launch`: missing bindings + extraneous
    /// bindings are both 400-class errors.
    pub resource_bindings: HashMap<String, String>,
    /// Workspace context for resource binding. Required when
    /// `graph.resources` is non-empty, ignored otherwise. Threaded as
    /// `Option<Uuid>` because no `workspaces` table exists yet — current
    /// callers pass `None` for templates without resources.
    pub workspace_id: Option<Uuid>,
}

/// Owns the deploy-an-instance sequence. Behavior-identical to the code that
/// was inlined in `create_instance` and `fire_spawn` — pure relocation.
///
/// Holds an optional [`ResourceResolver`]: `None` means "this launcher was
/// constructed before the resource layer landed; only workflows with empty
/// `graph.resources` will work". The new constructor [`InstanceLauncher::with_resources`]
/// supplies a resolver; the legacy [`InstanceLauncher::new`] doesn't.
#[derive(Clone)]
pub struct InstanceLauncher<'a> {
    db: &'a PgPool,
    petri: &'a PetriClient,
    resolver: Option<Arc<ResourceResolver>>,
}

impl<'a> InstanceLauncher<'a> {
    /// Construct without a resource resolver. Launches that hit a workflow
    /// with non-empty `graph.resources` will fail with
    /// [`ResourceBindError::MissingWorkspace`] (resolver-less launchers
    /// can't resolve resources). Existing call sites use this entry — they
    /// migrate to [`with_resources`] as they touch resources.
    pub fn new(db: &'a PgPool, petri: &'a PetriClient) -> Self {
        Self {
            db,
            petri,
            resolver: None,
        }
    }

    /// Construct with a resource resolver wired in.
    pub fn with_resources(
        db: &'a PgPool,
        petri: &'a PetriClient,
        resolver: Arc<ResourceResolver>,
    ) -> Self {
        Self {
            db,
            petri,
            resolver: Some(resolver),
        }
    }

    /// Parameterize, insert the row, deploy, and roll the row back if the
    /// deploy fails. Returns the persisted instance on success.
    ///
    /// Ordering is load-bearing and preserved exactly: the row is inserted
    /// *before* the deploy so the lifecycle listener can find it if the net
    /// completes before this returns; a deploy failure deletes the row before
    /// the error propagates so lifecycle never sees a phantom.
    pub async fn launch(&self, spec: LaunchSpec<'_>) -> Result<WorkflowInstance, LaunchError> {
        let parameterized = parameterize_air(
            spec.air_json,
            spec.instance_id,
            spec.template_id,
            spec.template_version,
            spec.created_by,
            spec.graph,
            spec.start_tokens,
        )?;

        // Phase B.7 — bind aliases, resolve, splice. Each stage returns early
        // *before* the instance row is inserted so a binding error never
        // leaves a phantom row.
        let (resolved_air, resource_pins_json) =
            self.resolve_and_splice(parameterized, &spec).await?;

        let instance = sqlx::query_as::<_, WorkflowInstance>(
            r#"
            INSERT INTO workflow_instances (id, template_id, template_version, net_id, status, created_by, started_at, metadata, resource_pins)
            VALUES ($1, $2, $3, $4, 'running', $5, NOW(), $6, $7)
            RETURNING *
            "#,
        )
        .bind(spec.instance_id)
        .bind(spec.template_id)
        .bind(spec.template_version)
        .bind(&spec.net_id)
        .bind(spec.created_by)
        .bind(&spec.metadata)
        .bind(resource_pins_json.as_ref())
        .fetch_one(self.db)
        .await
        .map_err(|e| {
            tracing::error!("failed to insert instance: {e}");
            LaunchError::Database(e.to_string())
        })?;

        if let Err(e) = deploy_instance(self.petri, &spec.net_id, &resolved_air).await {
            tracing::error!("failed to deploy instance to petri-lab: {e}");
            // Roll the row back so lifecycle never observes a phantom /
            // never-deployed instance.
            let _ = sqlx::query("DELETE FROM workflow_instances WHERE id = $1")
                .bind(spec.instance_id)
                .execute(self.db)
                .await;
            return Err(LaunchError::Deploy(e.to_string()));
        }

        Ok(instance)
    }

    /// Phase B.7 internals — bind / resolve / splice. Returns the AIR with
    /// `__resources` declarations spliced into prepare transitions plus the
    /// frozen pin map for persistence. When the workflow declares no
    /// resources, this is a no-op and returns the input AIR unchanged.
    async fn resolve_and_splice<'b>(
        &self,
        air: Value,
        spec: &LaunchSpec<'b>,
    ) -> Result<(Value, Option<Value>), LaunchError> {
        // No-op path: nothing declared, nothing to do.
        if spec.graph.resources.is_empty() {
            // Surface a friendly error if the caller supplied bindings for
            // a workflow that doesn't declare resources — silent drop would
            // hide typos.
            if !spec.resource_bindings.is_empty() {
                let alias = spec.resource_bindings.keys().next().cloned().unwrap_or_default();
                return Err(LaunchError::Resource(ResourceBindError::UnknownResourceAlias {
                    alias,
                }));
            }
            return Ok((air, None));
        }

        // Workspace is required for resource resolution.
        let workspace_id = spec.workspace_id.ok_or_else(|| {
            LaunchError::Resource(ResourceBindError::MissingWorkspace {
                alias_count: spec.graph.resources.len(),
            })
        })?;
        let resolver = self.resolver.clone().ok_or_else(|| {
            LaunchError::Resource(ResourceBindError::MissingWorkspace {
                alias_count: spec.graph.resources.len(),
            })
        })?;

        // (1) Validate the caller-supplied bindings against the workflow
        //     declarations. Both directions checked: every declared alias
        //     must have a binding, and every binding must point at a
        //     declared alias.
        for alias in spec.graph.resources.keys() {
            if !spec.resource_bindings.contains_key(alias) {
                return Err(LaunchError::Resource(ResourceBindError::MissingResourceBinding {
                    alias: alias.clone(),
                }));
            }
        }
        for alias in spec.resource_bindings.keys() {
            if !spec.graph.resources.contains_key(alias) {
                return Err(LaunchError::Resource(ResourceBindError::UnknownResourceAlias {
                    alias: alias.clone(),
                }));
            }
        }

        // (2) Resolve each path to a pin (resource_id, latest_version).
        let pins = bind_aliases(self.db, workspace_id, &spec.resource_bindings)
            .await
            .map_err(LaunchError::Resource)?;

        // (3) Run the resolver — produces `{ <alias>: { ...public_inline...,
        //     ...secret_refs... } }` plus writes audit rows transactionally.
        let envelope = resolver
            .resolve(
                workspace_id,
                spec.created_by,
                &pins,
                AuditContext {
                    instance_id: Some(spec.instance_id),
                    step_id: None,
                    site: "launcher".to_string(),
                    principal_id: spec.created_by,
                    action: AuditAction::Resolve,
                },
            )
            .await
            .map_err(|e| LaunchError::Resource(ResourceBindError::Resolver(e)))?;

        // (4) Splice `let __resources = #{ ... };` into every prepare
        //     transition whose logic mentions any of the aliases.
        let aliases: Vec<&str> = spec.graph.resources.keys().map(String::as_str).collect();
        let resolved_air = splice_resources_into_air(air, &envelope, &aliases);

        // (5) Persist the frozen pins. Convert to a flat
        //     `{ alias: { resource_id, version } }` JSON for storage.
        let mut pin_map = JsonMap::with_capacity(pins.len());
        for (alias, pin) in &pins {
            pin_map.insert(
                alias.clone(),
                serde_json::json!({
                    "resource_id": pin.resource_id,
                    "version": pin.version,
                }),
            );
        }

        Ok((resolved_air, Some(Value::Object(pin_map))))
    }
}

/// Look up each `(alias -> path)` against `resources` and pin to
/// `latest_version`. Soft-deleted resources are invisible (NULL filter).
///
/// Exposed `pub` so the B.7 integration tests can exercise alias binding
/// without going through the full `InstanceLauncher::launch` (which requires
/// a running petri-lab). The launcher itself calls this internally during
/// `resolve_and_splice`.
pub async fn bind_aliases(
    db: &PgPool,
    workspace_id: Uuid,
    bindings: &HashMap<String, String>,
) -> Result<HashMap<String, ResourcePin>, ResourceBindError> {
    let mut out = HashMap::with_capacity(bindings.len());
    for (alias, path) in bindings {
        let row: Option<(Uuid, i32)> = sqlx::query_as(
            "SELECT id, latest_version FROM resources \
             WHERE workspace_id = $1 AND path = $2 AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .bind(path)
        .fetch_optional(db)
        .await
        .map_err(|e| ResourceBindError::Database(e.to_string()))?;

        let (resource_id, version) = row.ok_or_else(|| ResourceBindError::ResourcePathNotFound {
            alias: alias.clone(),
            path: path.clone(),
        })?;
        out.insert(
            alias.clone(),
            ResourcePin {
                resource_id,
                version,
            },
        );
    }
    Ok(out)
}

/// Splice `let __resources = #{ ... };` at the top of every prepare
/// transition whose Rhai logic references any of the workflow's resource
/// aliases. One declaration per transition with **all** referenced aliases
/// inside it — never a duplicate.
///
/// Per-transition splicing (rather than scenario-wide hoisting) is the v1
/// default because (a) the envelope is small (a handful of fields per
/// alias) and the duplication is bounded by the number of Python steps
/// that use resources, (b) any future scope semantics (per-step ACLs etc.)
/// will naturally key off the transition. Scenario-init hoisting is a v2
/// optimization.
///
/// The function is robust against repeat calls — a `__resources` declaration
/// already present in `logic.source` short-circuits the splice.
///
/// Exposed `pub` so the B.7 integration tests can verify the AIR
/// transformation without a live petri-lab roundtrip.
pub fn splice_resources_into_air(
    mut air: Value,
    envelope: &Value,
    aliases: &[&str],
) -> Value {
    // Build the Rhai literal once.
    let rhai_decl = build_resources_decl(envelope, aliases);
    if rhai_decl.is_empty() {
        return air;
    }

    let Some(transitions) = air
        .get_mut("transitions")
        .and_then(|t| t.as_array_mut())
    else {
        return air;
    };

    for t in transitions {
        let Some(t_obj) = t.as_object_mut() else {
            continue;
        };

        // Heuristic: target the prepare transition by id suffix. The two
        // shapes in use today are `<node_id>/prepare` and `t_<node_id>_prepare`;
        // either matches.
        let is_prepare = t_obj
            .get("id")
            .and_then(Value::as_str)
            .map(|id| id.ends_with("/prepare") || id.ends_with("_prepare"))
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
        let Some(source) = logic_obj.get("source").and_then(Value::as_str) else {
            continue;
        };
        let source = source.to_owned();

        // Only splice into transitions whose logic actually references an
        // alias. Avoids polluting unrelated prepare transitions.
        let references_any = aliases.iter().any(|a| source.contains(&format!("__resources[\"{a}\"]"))
            || source.contains(&format!("__resources['{a}']")));
        if !references_any {
            continue;
        }

        // Idempotent guard.
        if source.contains("let __resources") {
            continue;
        }

        let new_source = format!("{rhai_decl}\n{source}", source = source);
        logic_obj.insert("source".to_string(), Value::String(new_source));
    }

    air
}

/// Build `let __resources = #{ "alias": #{ ... }, ... };` from the
/// resolver's JSON envelope. Public fields are emitted as their literal
/// JSON form (strings, numbers, bools); secret-template strings remain
/// strings (the existing `extract_secret_keys` regex picks them up later).
fn build_resources_decl(envelope: &Value, aliases: &[&str]) -> String {
    let Value::Object(top) = envelope else {
        return String::new();
    };
    let mut entries: Vec<String> = Vec::with_capacity(aliases.len());
    for alias in aliases {
        let Some(subtree) = top.get(*alias) else {
            continue;
        };
        let Some(subtree_obj) = subtree.as_object() else {
            continue;
        };
        let mut field_entries: Vec<String> = Vec::with_capacity(subtree_obj.len());
        for (k, v) in subtree_obj {
            // `serde_json::to_string` quotes strings and renders numbers/
            // bools/null without extra ceremony — exactly what Rhai's
            // object-map literal accepts. The wrapping `#{ ... }` is Rhai-
            // specific; everything inside is JSON-clean.
            let v_lit = serde_json::to_string(v).unwrap_or_else(|_| "()".to_string());
            field_entries.push(format!("\"{}\": {}", escape_rhai_key(k), v_lit));
        }
        entries.push(format!(
            "\"{alias}\": #{{ {body} }}",
            alias = escape_rhai_key(alias),
            body = field_entries.join(", "),
        ));
    }
    if entries.is_empty() {
        return String::new();
    }
    format!("let __resources = #{{ {} }};", entries.join(", "))
}

fn escape_rhai_key(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_resources_decl_basic() {
        let env = serde_json::json!({
            "db": {
                "host": "h",
                "port": 5432,
                "password": "{{secret:resources/aaa/v1#password}}"
            }
        });
        let decl = build_resources_decl(&env, &["db"]);
        // Field order in serde_json::Map is preserved; we just check the
        // shape and the secret template makes it through.
        assert!(decl.starts_with("let __resources = #{ "));
        assert!(decl.contains("\"db\": #{"));
        assert!(decl.contains("\"host\": \"h\""));
        assert!(decl.contains("\"port\": 5432"));
        assert!(decl.contains("\"password\": \"{{secret:resources/aaa/v1#password}}\""));
        assert!(decl.ends_with(" };"));
    }

    #[test]
    fn build_resources_decl_empty_envelope_is_empty() {
        let env = serde_json::json!({});
        assert_eq!(build_resources_decl(&env, &["db"]), "");
    }

    #[test]
    fn splice_skips_non_prepare() {
        let air = serde_json::json!({
            "transitions": [
                {
                    "id": "t_x_consume",
                    "logic": { "type": "Rhai", "source": "__resources[\"db\"]" }
                }
            ]
        });
        let env = serde_json::json!({ "db": { "host": "h" } });
        let out = splice_resources_into_air(air, &env, &["db"]);
        let src = out["transitions"][0]["logic"]["source"].as_str().unwrap();
        // Non-prepare transition — no splice.
        assert!(!src.contains("let __resources"));
    }

    #[test]
    fn splice_inserts_once_per_prepare() {
        let air = serde_json::json!({
            "transitions": [
                {
                    "id": "t_step_prepare",
                    "logic": {
                        "type": "Rhai",
                        "source": "job_inputs.push(#{ \"name\": \"db.json\", \"source\": #{ \"type\": \"inline\", \"value\": __resources[\"db\"] } });"
                    }
                }
            ]
        });
        let env = serde_json::json!({ "db": { "host": "h", "port": 5432 } });
        let out = splice_resources_into_air(air, &env, &["db"]);
        let src = out["transitions"][0]["logic"]["source"].as_str().unwrap();
        assert!(src.contains("let __resources = #{"));
        assert!(src.contains("\"host\": \"h\""));
        // Idempotent — running again doesn't double-splice.
        let env2 = serde_json::json!({ "db": { "host": "h", "port": 5432 } });
        let out2 = splice_resources_into_air(out, &env2, &["db"]);
        let src2 = out2["transitions"][0]["logic"]["source"].as_str().unwrap();
        assert_eq!(src2.matches("let __resources").count(), 1);
    }
}
