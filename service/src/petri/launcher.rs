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
//! ## Resources at launch
//!
//! Workspace resources are resolved + spliced into the AIR at **publish
//! time** by the publish handler — the AIR persisted in
//! `workflow_template_versions.air_json` already carries baked-in
//! `__resources` declarations on every prepare transition that needs them.
//! The launcher therefore does not touch resources, takes no bindings, and
//! is workspace-agnostic. Pinning by `resource_id @ latest_version` happens
//! once, in the compiler, and survives every subsequent launch unchanged.
//! The `workflow_instances.resource_pins` JSONB column kept its shape — but
//! it is now populated lazily by replay/debug tooling, not by the launcher
//! (which has no map to write).

use std::collections::HashMap;

use petri_api_types::DispatchOptions;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::instance::{StartToken, WorkflowInstance};
use crate::models::template::{WorkflowGraph, WorkflowTemplate};
use crate::petri::binding::{resolve_effective_bindings, BindingError, BoundSlot};
use crate::petri::client::PetriClient;
use crate::petri::instance::{
    deploy_instance, parameterize_air, parameterize_for_place, ParameterizeError,
    ParameterizeForPlaceError,
};
use crate::AppState;

/// Why a launch failed. Each caller maps these to its own surface:
/// `create_instance` turns [`LaunchError::Parameterize`] into a 400 and
/// [`LaunchError::Deploy`] into a 502; `fire_spawn` folds them into
/// `TriggerError::InstanceFailed`. The launcher itself is surface-agnostic.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// `parameterize_air` rejected the start tokens (missing/unknown/duplicate
    /// start block, wrong field kind, ...). No row was inserted.
    #[error(transparent)]
    Parameterize(#[from] ParameterizeError),

    /// `parameterize_for_place` rejected the pre-AIR direct-place seeding
    /// (place id not found in AIR, or AIR has no `places` array).
    #[error(transparent)]
    ParameterizeForPlace(#[from] ParameterizeForPlaceError),

    /// The instance row could not be inserted. Nothing was deployed.
    #[error("instance row insert failed: {0}")]
    Database(String),

    /// petri-lab deploy failed. The just-inserted row has already been rolled
    /// back so the lifecycle listener never observes a never-deployed
    /// instance.
    #[error("deploy failed: {0}")]
    Deploy(String),
}

/// What the caller wants run.
///
/// Two variants, one per authoring path:
/// - [`LaunchSpec::Templated`] — graph-authored template (`Start` blocks,
///   typed ports, payload-mapping validated at the launcher boundary). The
///   path the visual editor produces; consumed by `create_instance` and
///   by graph-authored triggers in `fire_spawn`.
/// - [`LaunchSpec::PreAir`] — clinic-style headless template. The trigger
///   names an AIR place id directly (no `Start`, no graph-level port
///   shape). Consumed by `fire_spawn` when the trigger record carries
///   `air_target_place_id`. Per
///   `feedback_no_mode_framing_for_the_direction` this is a first-class
///   variant, not an `Option<&WorkflowGraph>` mode-flag on the templated
///   path.
pub enum LaunchSpec<'a> {
    Templated {
        instance_id: Uuid,
        net_id: String,
        template_id: Uuid,
        template_version: i32,
        created_by: Uuid,
        /// Audit-only blob stored on the instance row (not merged into tokens).
        metadata: Value,
        air_json: &'a Value,
        graph: &'a WorkflowGraph,
        start_tokens: &'a [StartToken],
        /// Categorizes the instance. `None` ⇒ `'live'` (the historical default).
        /// `Some("draft")` for user-initiated experiments; `Some("test_run")` is
        /// reserved for the template-test runner.
        mode: Option<&'a str>,
        /// Set when `mode == "test_run"`. Forwards into the instance row so the
        /// run can be reconciled with its originating `template_tests` row.
        test_id: Option<Uuid>,
        /// Per-run ablation envelope (#126.2): `skip_mask` +
        /// `stage_overrides` threaded into the engine's
        /// `LoadScenarioRequest`. Defaults to empty on caller side via
        /// `DispatchOptions::default()` when the create-instance handler
        /// does not surface ablation.
        dispatch_options: DispatchOptions,
        /// Submitter-supplied net-level parameter bag (tenant propagation
        /// D1-A). Threaded into the engine's `LoadScenarioRequest.net_parameters`
        /// where it is stored via `set_net_parameters` on the spawned net's
        /// service. Opaque, generic infra — the firing path reads it for
        /// `$params.` resolution and pre-dispatch metadata (e.g. `tenant_id`).
        /// `None` when the caller surfaces no parameters.
        net_parameters: Option<Value>,
        /// First-class tenant (workspace) identifier for this net instance
        /// (multi-tenancy). Threaded into the engine's
        /// `LoadScenarioRequest.workspace_id` so every NATS subject/stream/KV
        /// the engine creates for this net carries a `{workspace_id}` segment.
        /// `None` ⇒ the engine routes on its reserved `"default"` sentinel.
        workspace_id: Option<String>,
        /// Per-run compiled `(graph, interface_json)` captured onto the instance
        /// row for a DRAFT dev-run, so the instance UI renders what actually
        /// ran instead of the template's stale (pre-publish) columns. `None`
        /// for live/test_run runs, which read the immutable published template
        /// version. See migration `20240185000000`.
        graph_snapshot: Option<Value>,
        interface_snapshot: Option<Value>,
    },
    PreAir {
        instance_id: Uuid,
        net_id: String,
        template_id: Uuid,
        template_version: i32,
        created_by: Uuid,
        metadata: Value,
        air_json: &'a Value,
        /// The AIR place id whose `initial_tokens` will be seeded with the
        /// supplied token + system fields. Resolved at the trigger
        /// boundary from the Trigger node's `air_target_place_id`.
        air_target_place_id: &'a str,
        /// Opaque payload. Clinic AIR transitions consume opaque tokens
        /// (task_kind / required_capabilities / system_prompt live in
        /// `transition.logic.config`); no port-shape validation here.
        token: &'a Value,
        /// Per-run ablation envelope (#126.2). Surfaced by the
        /// trigger-fire boundary so research-harness ablation flows
        /// through trigger-fired runs identically to the prior
        /// scenario-submit path.
        dispatch_options: DispatchOptions,
        /// Submitter-supplied net-level parameter bag (tenant propagation
        /// D1-A). Threaded into the engine's `LoadScenarioRequest.net_parameters`.
        /// See the `Templated` variant for full semantics.
        net_parameters: Option<Value>,
        /// First-class tenant (workspace) identifier for this net instance.
        /// See the `Templated` variant for full semantics.
        workspace_id: Option<String>,
    },
}

/// Owns the deploy-an-instance sequence. Behavior-identical to the code that
/// was inlined in `create_instance` and `fire_spawn` — pure relocation, now
/// extended with the pre-AIR variant.
#[derive(Clone, Copy)]
pub struct InstanceLauncher<'a> {
    db: &'a PgPool,
    petri: &'a PetriClient,
}

impl<'a> InstanceLauncher<'a> {
    pub fn new(db: &'a PgPool, petri: &'a PetriClient) -> Self {
        Self { db, petri }
    }

    /// Parameterize, insert the row, deploy, and roll the row back if the
    /// deploy fails. Returns the persisted instance on success.
    ///
    /// Ordering is load-bearing and preserved exactly: the row is inserted
    /// *before* the deploy so the lifecycle listener can find it if the net
    /// completes before this returns; a deploy failure deletes the row before
    /// the error propagates so lifecycle never sees a phantom.
    pub async fn launch(&self, spec: LaunchSpec<'_>) -> Result<WorkflowInstance, LaunchError> {
        // Per-variant: parameterize and capture the row-write + deploy inputs
        // in a single tuple so the DB-write / deploy / rollback tail is shared
        // byte-for-byte across both paths (the launcher's load-bearing
        // invariant — see the doc-comment above).
        let (
            mut parameterized,
            instance_id,
            net_id,
            template_id,
            template_version,
            created_by,
            metadata,
            dispatch_options,
            net_parameters,
            workspace_id,
            mode,
            test_id,
            graph_snapshot,
            interface_snapshot,
        ) = match spec {
            LaunchSpec::Templated {
                instance_id,
                net_id,
                template_id,
                template_version,
                created_by,
                metadata,
                air_json,
                graph,
                start_tokens,
                mode,
                test_id,
                dispatch_options,
                net_parameters,
                workspace_id,
                graph_snapshot,
                interface_snapshot,
            } => {
                let parameterized = parameterize_air(
                    air_json,
                    instance_id,
                    template_id,
                    template_version,
                    created_by,
                    graph,
                    start_tokens,
                )?;
                (
                    parameterized,
                    instance_id,
                    net_id,
                    template_id,
                    template_version,
                    created_by,
                    metadata,
                    dispatch_options,
                    net_parameters,
                    workspace_id,
                    mode,
                    test_id,
                    graph_snapshot,
                    interface_snapshot,
                )
            }
            LaunchSpec::PreAir {
                instance_id,
                net_id,
                template_id,
                template_version,
                created_by,
                metadata,
                air_json,
                air_target_place_id,
                token,
                dispatch_options,
                net_parameters,
                workspace_id,
            } => {
                let parameterized = parameterize_for_place(
                    air_json,
                    instance_id,
                    template_id,
                    template_version,
                    created_by,
                    air_target_place_id,
                    token,
                )?;
                (
                    parameterized,
                    instance_id,
                    net_id,
                    template_id,
                    template_version,
                    created_by,
                    metadata,
                    dispatch_options,
                    net_parameters,
                    workspace_id,
                    // Pre-AIR triggers are headless service calls — no
                    // template-test runner / experiment-mode framing applies.
                    None,
                    None,
                    // Pre-AIR runs are headless clinic templates with a frozen
                    // published AIR — no live-Y.Doc draft to snapshot.
                    None,
                    None,
                )
            }
        };

        // Asset version-pinning (docs/20 §6). The publish handler stashed the
        // `{alias -> {asset_id, version}}` pin map as a `__asset_pins` sidecar
        // on the AIR; capture it into `workflow_instances.asset_pins` so asset
        // edits after launch don't retroactively change a running instance, and
        // strip it from the AIR so the engine never sees the sidecar. Mirrors
        // `resource_pins` — the authoritative pin is already baked into the
        // spliced `__assets` records the AIR carries; this column is the
        // launch-time/replay record. `{}` when the template binds no assets.
        let asset_pins = parameterized
            .as_object_mut()
            .and_then(|o| o.remove("__asset_pins"))
            .unwrap_or_else(|| serde_json::json!({}));

        let mode_str = mode.unwrap_or("live");
        let instance = sqlx::query_as::<_, WorkflowInstance>(
            r#"
            INSERT INTO workflow_instances (id, template_id, template_version, net_id, status, created_by, updated_by, started_at, metadata, mode, test_id, asset_pins, graph_snapshot, interface_snapshot)
            VALUES ($1, $2, $3, $4, 'running', $5, $5, NOW(), $6, $7, $8, $9, $10, $11)
            RETURNING *
            "#,
        )
        .bind(instance_id)
        .bind(template_id)
        .bind(template_version)
        .bind(&net_id)
        .bind(created_by)
        .bind(&metadata)
        .bind(mode_str)
        .bind(test_id)
        .bind(&asset_pins)
        .bind(&graph_snapshot)
        .bind(&interface_snapshot)
        .fetch_one(self.db)
        .await
        .map_err(|e| {
            tracing::error!("failed to insert instance: {e}");
            LaunchError::Database(e.to_string())
        })?;

        if let Err(e) = deploy_instance(
            self.petri,
            &net_id,
            &parameterized,
            dispatch_options,
            net_parameters,
            workspace_id,
        )
        .await
        {
            tracing::error!("failed to deploy instance to petri-lab: {e}");
            // Roll the row back so lifecycle never observes a phantom /
            // never-deployed instance.
            let _ = sqlx::query("DELETE FROM workflow_instances WHERE id = $1")
                .bind(instance_id)
                .execute(self.db)
                .await;
            return Err(LaunchError::Deploy(e.to_string()));
        }

        Ok(instance)
    }
}

/// Why binding-aware AIR preparation failed. Mapped by `create_instance` to a
/// 400 (bad override / type mismatch) or 422 (run-gate: required slot unbound).
#[derive(Debug, thiserror::Error)]
pub enum PrepareBindingsError {
    /// One or more REQUIRED requirement slots resolved by no binding tier. The
    /// caller must supply a binding (per-instance, per-workspace default, or a
    /// platform/home resource of the matching type). Carries the actionable
    /// `(slot_key, resource_type)` list.
    #[error("{} required resource binding(s) are unbound: {}", .0.len(), describe_unbound(.0))]
    Unbound(Vec<(String, String)>),

    /// An offered/auto-bound resource doesn't exist, isn't visible, or its type
    /// doesn't match the slot — surfaced verbatim from the binding resolver.
    #[error(transparent)]
    Binding(#[from] BindingError),
}

fn describe_unbound(slots: &[(String, String)]) -> String {
    slots
        .iter()
        .map(|(k, ty)| format!("'{k}' (type '{ty}')"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Binding-aware AIR preparation — the launcher's run-time resource/pool
/// parameterization (Phase C).
///
/// ## Back-compat contract
///
/// A template whose `requirements_json` is NULL/empty has NO derived slots:
/// [`resolve_effective_bindings`] returns an empty result and this function
/// returns the AIR **byte-for-byte unchanged**. Such templates launch exactly
/// as they did before this feature — the caller can pass the persisted baseline
/// `air_json` straight through.
///
/// For a template WITH a manifest, this:
/// 1. Resolves an effective binding per slot (precedence: per-instance override
///    → per-workspace default → platform auto-bind → home-workspace baseline).
/// 2. RUN-GATEs: if any REQUIRED slot is unbound, returns
///    [`PrepareBindingsError::Unbound`] (the caller maps it to 422) — nothing
///    is deployed.
/// 3. For each slot whose effective resource DIFFERS from the baked baseline
///    (tiers 1–3), rewrites the AIR using the manifest's recorded addresses:
///    every `pool-{old_id}` net id → `pool-{new_id}` (from
///    `SlotAirAddresses::net_ids`), and re-resolves + re-splices the
///    `__resources` envelope under `SlotAirAddresses::resource_keys`.
/// 4. When the effective resource is a `scope_kind = 'platform'` pool, FIRST
///    re-deploys that resource's pool net `pool-{resource_id}` stamped with the
///    TENANT `workspace_id` (not `PLATFORM_SCOPE_ID`) so the engine's
///    intra-workspace bridge gate + NATS subjects line up (the no-engine-change
///    platform-bridge fix; see
///    [`crate::handlers::resources::redeploy_pool_net_for_workspace`]).
///
/// Returns the (possibly rewritten) AIR ready for the launcher's parameterize +
/// deploy. `workspace_id` is the LAUNCHING tenant (the template's workspace for
/// a user launch). `overrides` is the caller's per-instance `slot_key ->
/// resource_id` map (empty when none supplied).
pub async fn prepare_air_with_bindings(
    state: &AppState,
    template: &WorkflowTemplate,
    workspace_id: Uuid,
    principal: Uuid,
    overrides: &HashMap<String, Uuid>,
    air: Value,
) -> Result<Value, PrepareBindingsError> {
    let resolved =
        resolve_effective_bindings(state, template, workspace_id, principal, overrides).await?;

    // Run-gate: a required slot with no binding tier blocks the launch.
    if !resolved.is_launchable() {
        let slots = resolved
            .unbound_required
            .iter()
            .map(|s| (s.key.clone(), s.resource_type.clone()))
            .collect();
        return Err(PrepareBindingsError::Unbound(slots));
    }

    // Self-heal the workspace-owned (tier-4 baseline) pool nets this instance
    // bridges to BEFORE any fast-path return. Each `pool-<resource_id>` is
    // re-`ensure`d under the launching workspace: a no-op for a healthy running
    // pool, a wake (+ workspace re-stamp) for a hibernated one, and a full
    // re-deploy for a pool the engine lost (NATS wiped / engine was down at
    // resource create). Without this a drifted/lost workspace pool fails the
    // instance's to-Running gate with `BRIDGE_TARGET_NET_MISSING`. Idempotent
    // and engine-down-tolerant, so it is safe on the common path.
    for resource_id in &resolved.baseline_pools {
        crate::handlers::resources::ensure_pool_net_for_launch(state, *resource_id, workspace_id)
            .await;
    }

    // No tier-1–3 substitution → byte-identical baseline AIR (the common
    // case: every slot satisfied by the home-workspace baseline already baked).
    if resolved.substitutions.is_empty() {
        return Ok(air);
    }

    // Parse the manifest once for the per-slot AIR addresses. (The resolver
    // already validated it decodes; a None here means it changed under us — be
    // defensive and skip substitution rather than launch a half-rewritten AIR.)
    let Some(manifest) = template
        .requirements_json
        .as_ref()
        .and_then(|raw| serde_json::from_value::<crate::compiler::RequirementsManifest>(raw.clone()).ok())
    else {
        return Ok(air);
    };

    let mut air = air;
    for (slot_key, bound) in &resolved.substitutions {
        let Some(addresses) = manifest.air_addresses.get(slot_key) else {
            continue;
        };

        // Platform pool rebind (no-engine-change bridge fix): materialize the
        // platform pool net under the tenant workspace BEFORE we point the
        // instance net's bridge at `pool-{new_id}`.
        if bound.is_platform && !addresses.net_ids.is_empty() {
            crate::handlers::resources::redeploy_pool_net_for_workspace(
                state,
                bound.resource_id,
                workspace_id,
            )
            .await;
        } else if !addresses.net_ids.is_empty() {
            // Substituted (tier-1–3) WORKSPACE-owned pool: ensure `pool-{new}`
            // exists under the launching workspace too — the rewrite below points
            // the bridge at it, so the same self-heal the baseline pools get
            // applies (a tier-2 default / tier-1 override can name a pool the
            // engine never deployed or has since lost).
            crate::handlers::resources::ensure_pool_net_for_launch(
                state,
                bound.resource_id,
                workspace_id,
            )
            .await;
        }

        // (a) Rewrite every baked `pool-{old_id}` net id → `pool-{new_id}`.
        air = rewrite_pool_net_ids(air, &addresses.net_ids, bound.resource_id);

        // (b) Re-resolve the effective resource and INJECT a per-key override
        //     reassignment into the marker-anchored baseline `__resources`
        //     declaration (NOT a re-splice — the baseline `let` already exists).
        if !addresses.resource_keys.is_empty() {
            air = inject_resource_overrides(
                state,
                workspace_id,
                principal,
                bound,
                &addresses.resource_keys,
                air,
            )
            .await?;
        }
    }

    Ok(air)
}

/// Rewrite every occurrence of the baked `pool-{old_id}` net ids to
/// `pool-{new_id}` across the whole AIR (bridge subjects, spawn refs, backing
/// references). Walks the JSON string-replacing on the deterministic id form,
/// so any field carrying the net id — wherever it appears — is updated in one
/// pass. The new id is `pool-{effective_resource_id}`.
fn rewrite_pool_net_ids(air: Value, old_net_ids: &[String], new_resource_id: Uuid) -> Value {
    use crate::compiler::well_known::pool_net_id;
    let new_id = pool_net_id(new_resource_id);
    // Serialize → string-replace each old id → deserialize. The ids are
    // `pool-{uuid}` (no substring-collision risk: a UUID can't be a prefix of
    // another id form), so a literal replace is safe and covers every field.
    let Ok(mut s) = serde_json::to_string(&air) else {
        return air;
    };
    for old in old_net_ids {
        if old == &new_id {
            continue;
        }
        s = s.replace(old, &new_id);
    }
    serde_json::from_str(&s).unwrap_or(air)
}

/// Re-resolve the effective resource and INJECT a per-key override reassignment
/// into every prepare transition's marker-anchored baseline `__resources`
/// declaration.
///
/// The baseline AIR carries (for templates published after the marker change) a
///
/// ```text
/// //__AITH_RES_BEGIN__
/// let __resources = #{ "<key>": #{ ...home-baseline... }, ... };
/// //__AITH_RES_END__
/// ...reads of __resources["<key>"]...
/// ```
///
/// block per prepare transition. For each affected key we resolve the EFFECTIVE
/// resource's envelope and insert, immediately BEFORE [`RES_END_MARKER`], one
///
/// ```text
/// __resources["<key>"] = #{ ...effective... };
/// ```
///
/// statement. Rhai executes top-to-bottom, so the reassignment runs after the
/// `let __resources = …;` and before the reads that follow the end marker,
/// overriding exactly that key with the effective resource's public values +
/// secret templates while leaving every other key's baseline entry intact.
///
/// A transition with no end marker (a pre-marker / NULL-manifest template) is
/// never reached here: such templates have no `requirements_json` and so never
/// resolve a substitution. We still guard defensively (skip transitions lacking
/// the marker or not referencing the key) so a partial/legacy AIR is left
/// byte-for-byte unchanged rather than half-rewritten.
async fn inject_resource_overrides(
    state: &AppState,
    workspace_id: Uuid,
    principal: Uuid,
    bound: &BoundSlot,
    resource_keys: &[String],
    air: Value,
) -> Result<Value, PrepareBindingsError> {
    use crate::compiler::resource_refs::{KnownResource, KnownResources};
    use crate::petri::resource_resolver::build_one_resource_literal;

    // Resolve the effective resource's envelope under the slot's key(s).
    let mut known: KnownResources = KnownResources::new();
    for key in resource_keys {
        known.insert(
            key.clone(),
            KnownResource {
                id: bound.resource_id,
                type_name: bound.resource_type.clone(),
                latest_version: bound.version,
                public_config: serde_json::Value::Null,
            },
        );
    }
    let envelope = state
        .resource_resolver
        .resolve_known(workspace_id, principal, &known, None)
        .await
        .map_err(|e| PrepareBindingsError::Binding(BindingError::Db(e.to_string())))?;

    // Pre-build the per-key reassignment literals from the effective envelope.
    // A key with no envelope subtree (resolver dropped it) contributes nothing.
    let mut overrides: Vec<(String, String)> = Vec::with_capacity(resource_keys.len());
    for key in resource_keys {
        if let Some(literal) = build_one_resource_literal(&envelope, key) {
            overrides.push((key.clone(), literal));
        }
    }
    if overrides.is_empty() {
        return Ok(air);
    }

    Ok(splice_overrides_into_air(air, &overrides))
}

/// Pure (DB-free) AIR rewrite: insert a `__resources["<key>"] = <literal>;`
/// reassignment immediately BEFORE the [`RES_END_MARKER`] in every prepare
/// transition whose marker-anchored source references that key. `overrides` is
/// the pre-resolved `(key, rhai_map_literal)` list. Transitions without the end
/// marker (pre-marker / NULL-manifest AIR) are left byte-for-byte unchanged.
///
/// Split out from [`inject_resource_overrides`] so the string-level rewrite is
/// unit-testable without an `AppState`/DB (the envelope is stubbed by building
/// the literal directly via `build_one_resource_literal`).
fn splice_overrides_into_air(air: Value, overrides: &[(String, String)]) -> Value {
    use crate::petri::resource_resolver::RES_END_MARKER;

    let mut air = air;
    let Some(transitions) = air.get_mut("transitions").and_then(|t| t.as_array_mut()) else {
        return air;
    };

    for t in transitions {
        let Some(t_obj) = t.as_object_mut() else {
            continue;
        };
        // Same prepare-transition detection splice_resources_into_air uses.
        let is_prepare = t_obj
            .get("id")
            .and_then(Value::as_str)
            .map(crate::compiler::borrow::apply::has_prepare_transition_suffix)
            .unwrap_or(false);
        if !is_prepare {
            continue;
        }
        let Some(logic_obj) = t_obj.get_mut("logic").and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(source) = logic_obj.get("source").and_then(Value::as_str) else {
            continue;
        };
        // Only marker-anchored sources can take an override injection.
        if !source.contains(RES_END_MARKER) {
            continue;
        }

        // Build the reassignment block for the keys this transition references.
        let mut block = String::new();
        for (key, literal) in overrides {
            let references = source.contains(&format!("__resources[\"{key}\"]"))
                || source.contains(&format!("__resources['{key}']"));
            if !references {
                continue;
            }
            block.push_str(&format!("__resources[\"{key}\"] = {literal};\n"));
        }
        if block.is_empty() {
            continue;
        }

        // Insert the block immediately BEFORE the end marker (after the baseline
        // `let __resources = …;`). Replace only the FIRST occurrence so a body
        // that happens to contain the marker literal later isn't disturbed.
        let new_source = source.replacen(RES_END_MARKER, &format!("{block}{RES_END_MARKER}"), 1);
        logic_obj.insert("source".to_string(), Value::String(new_source));
    }

    air
}

#[cfg(test)]
mod tests {
    //! Pure-logic tests for the launcher's binding-aware AIR rewrite. The full
    //! `prepare_air_with_bindings` run-gate + re-splice needs an `AppState` + a
    //! live Postgres and is exercised in
    //! `service/tests/resource_bindings_e2e.rs`; here we cover the deterministic
    //! `pool-{old}` → `pool-{new}` rewrite and the unbound-slot error message.

    use super::*;
    use crate::compiler::well_known::pool_net_id;

    #[test]
    fn rewrite_pool_net_ids_substitutes_old_to_new_everywhere() {
        let old_id = Uuid::new_v4();
        let new_id = Uuid::new_v4();
        let old_net = pool_net_id(old_id);
        let new_net = pool_net_id(new_id);

        // The baked net id appears in several AIR fields the launcher must rewrite.
        let air = serde_json::json!({
            "places": [
                {
                    "id": "p_step_claim_out",
                    "bridge_out": { "target_net_id": old_net, "target_place_name": "claim_inbox" }
                }
            ],
            "transitions": [
                { "id": "t_x", "spawn_ref": old_net }
            ],
            "unrelated": "pool-not-a-uuid-keepme"
        });

        let rewritten = rewrite_pool_net_ids(air, std::slice::from_ref(&old_net), new_id);

        assert_eq!(
            rewritten["places"][0]["bridge_out"]["target_net_id"], new_net,
            "bridge target net id rewritten"
        );
        assert_eq!(
            rewritten["transitions"][0]["spawn_ref"], new_net,
            "spawn ref rewritten"
        );
        assert_eq!(
            rewritten["unrelated"], "pool-not-a-uuid-keepme",
            "non-matching strings untouched"
        );
        // No occurrence of the old id survives anywhere.
        let serialized = serde_json::to_string(&rewritten).unwrap();
        assert!(
            !serialized.contains(&old_net),
            "no old pool net id should remain"
        );
    }

    #[test]
    fn rewrite_pool_net_ids_noop_when_old_equals_new() {
        let id = Uuid::new_v4();
        let net = pool_net_id(id);
        let air = serde_json::json!({ "t": { "target_net_id": net } });
        let before = air.clone();
        let after = rewrite_pool_net_ids(air, std::slice::from_ref(&net), id);
        assert_eq!(after, before, "rewriting to the same id is a no-op");
    }

    #[test]
    fn rewrite_pool_net_ids_empty_address_list_is_noop() {
        let new_id = Uuid::new_v4();
        let air = serde_json::json!({ "x": "pool-abc" });
        let before = air.clone();
        let after = rewrite_pool_net_ids(air, &[], new_id);
        assert_eq!(after, before);
    }

    #[test]
    fn override_injection_replaces_baseline_key_before_end_marker() {
        use crate::petri::resource_resolver::{
            build_one_resource_literal, RES_BEGIN_MARKER, RES_END_MARKER,
        };

        // A prepare transition carrying a marker-wrapped baseline `let
        // __resources` decl (the OLD dsn) plus a read of `__resources["main_db"]`.
        let baseline_source = format!(
            "{begin}\nlet __resources = #{{ \"main_db\": #{{ \"dsn\": \"OLD\" }} }};\n{end}\n\
             job_inputs.push(__resources[\"main_db\"]);",
            begin = RES_BEGIN_MARKER,
            end = RES_END_MARKER,
        );
        let air = serde_json::json!({
            "transitions": [
                {
                    "id": "t_step_prepare",
                    "logic": { "type": "Rhai", "source": baseline_source }
                }
            ]
        });

        // Stub the EFFECTIVE envelope (a NEW dsn) and build the override literal
        // via the (b) helper directly — no DB needed.
        let envelope = serde_json::json!({ "main_db": { "dsn": "NEW" } });
        let literal =
            build_one_resource_literal(&envelope, "main_db").expect("literal builds");
        let overrides = vec![("main_db".to_string(), literal)];

        let rewritten = splice_overrides_into_air(air, &overrides);
        let src = rewritten["transitions"][0]["logic"]["source"]
            .as_str()
            .unwrap();

        // The OLD baseline decl is still present (the `let` is untouched)...
        assert!(
            src.contains("\"dsn\": \"OLD\""),
            "baseline decl preserved: {src}"
        );
        // ...but an override reassignment with the NEW value is injected.
        assert!(
            src.contains("__resources[\"main_db\"] = #{ \"dsn\": \"NEW\" };"),
            "override reassignment injected: {src}"
        );
        // The override lands BEFORE the end marker (so it runs before the reads
        // that follow it), and after the baseline `let`.
        let assign_at = src
            .find("__resources[\"main_db\"] = #{")
            .expect("assignment present");
        let end_at = src.find(RES_END_MARKER).expect("end marker present");
        let let_at = src.find("let __resources").expect("baseline let present");
        assert!(let_at < assign_at, "override after baseline let");
        assert!(assign_at < end_at, "override before end marker");
    }

    #[test]
    fn override_injection_noop_without_end_marker() {
        // A pre-marker / NULL-manifest prepare transition (no end marker) is left
        // byte-for-byte unchanged — back-compat is sacred.
        let air = serde_json::json!({
            "transitions": [
                {
                    "id": "t_step_prepare",
                    "logic": {
                        "type": "Rhai",
                        "source": "let __resources = #{ \"main_db\": #{ \"dsn\": \"OLD\" } };\n\
                                   job_inputs.push(__resources[\"main_db\"]);"
                    }
                }
            ]
        });
        let before = air.clone();
        let overrides = vec![("main_db".to_string(), "#{ \"dsn\": \"NEW\" }".to_string())];
        let after = splice_overrides_into_air(air, &overrides);
        assert_eq!(after, before, "no end marker → no rewrite");
    }

    #[test]
    fn describe_unbound_lists_slot_keys_and_types() {
        let msg = describe_unbound(&[
            ("prod_gpu".to_string(), "capacity".to_string()),
            ("main_db".to_string(), "postgres".to_string()),
        ]);
        assert!(msg.contains("'prod_gpu' (type 'capacity')"));
        assert!(msg.contains("'main_db' (type 'postgres')"));
    }

    #[test]
    fn unbound_error_display_counts_and_describes() {
        let err = PrepareBindingsError::Unbound(vec![(
            "prod_gpu".to_string(),
            "capacity".to_string(),
        )]);
        let s = err.to_string();
        assert!(s.contains("1 required resource binding(s) are unbound"), "got: {s}");
        assert!(s.contains("'prod_gpu' (type 'capacity')"), "got: {s}");
    }
}
