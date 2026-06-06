//! Provisioning seam for the model-replica autoscaler (P4-L1).
//!
//! REUSES the staging plane: a replica is actuated by generating a one-shot
//! `model-replica-<row>` net that fires the engine's `stage_template` inline
//! effect — the SAME effect the job-template staging path uses — but with the
//! P3b-additive `job_type = "service"` + `replicas = <Count>` + `residency_zone`
//! fields set, so the engine's `render_parameterized_job` registers a
//! long-running Nomad **service** job at the desired Count (instead of a
//! dispatched batch job), pinned to the residency zone.
//!
//! Provision / scale / teardown are ONE path: deploy a net carrying `replicas =
//! target`. `target == 0` registers the service at Count 0 (a stop / drain). The
//! engine renderer keeps the byte-stable batch render when these fields are
//! absent (P3b regression guard), so this never perturbs the lease-executor path.
//!
//! ## GDPR fail-closed (doc 28 §11)
//!
//! A non-empty `residency_zone` is a HARD placement constraint. The engine fails
//! closed at placement (an unsatisfiable zone leaves the allocation pending,
//! never outside-zone). We ADD a mekhan-side fail-closed guard: the Slurm leg of
//! `render_parameterized_job` does not honor residency, so we REFUSE to provision
//! a non-empty zone onto a non-Nomad datacenter rather than silently placing it
//! unconstrained.

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use aithericon_resources::types::ModelAutoscalePolicy;
use aithericon_sdk::scenario::ScenarioDefinition;
use aithericon_sdk::{effects, Context, DynamicToken};

use crate::compiler::well_known;
use crate::petri::client::PetriClient;
use crate::petri::pool_net::DatacenterConnection;
use crate::petri::staging_net::resolve_datacenter_connection;

/// An actuation failure. Distinct from a *cluster* failure (which the engine
/// records as `status:"failed"` DATA on the net's success port) — this is a
/// mekhan-side refusal (config / fail-closed) or a deploy failure.
#[derive(Debug)]
pub struct ActuateError {
    pub message: String,
}

impl ActuateError {
    fn new(m: impl Into<String>) -> Self {
        Self { message: m.into() }
    }
}

impl std::fmt::Display for ActuateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Build the AIR for a one-shot model-replica actuation run. Mirrors
/// [`crate::petri::staging_net::build_staging_net`] but fires `stage_template`
/// with a `spec` carrying the P3b service-job + residency fields.
///
/// `spec` is the StageSpec-shaped JSON the engine deserializes off the request
/// token — built by [`build_replica_spec`] (the policy's `replica_spec` merged
/// with `residency_zone` / `replicas` / `job_type=service`). The echoed
/// `staging_id` (= the replica row id) correlates the effect result back to the
/// `model_replicas` row.
pub fn build_model_replica_net(
    replica_id: Uuid,
    generation: i64,
    slug: &str,
    conn: &DatacenterConnection,
    spec: Value,
) -> ScenarioDefinition {
    let net_id = well_known::model_replica_net_id(replica_id, generation);
    let flavor = conn.scheduler_flavor.as_str();
    let mut ctx = Context::new(net_id).description(format!(
        "Model-replica actuation {replica_id}: register service job '{slug}' on \
         datacenter resource {} (flavor {flavor}) via the stage_template engine effect.",
        conn.resource_id
    ));

    let effect_config = conn.effect_config();

    let start: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.state("start", "Replica Request");
    let staged: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.terminal("staged", "Actuated");
    let failed: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.terminal("failed", "Actuation Failed (fatal)");

    ctx.transition("t_stage", "Register Service Job")
        .auto_input("request", &start)
        .auto_output("staged", &staged)
        .auto_output("_error", &failed)
        .effect_with_config(effects::STAGE_TEMPLATE.handler_id, effect_config);

    ctx.seed_one(
        &start,
        DynamicToken(json!({
            "staging_id": replica_id.to_string(),
            "slug": slug,
            "spec": spec,
            "escape_hatch": {},
            "package_ref": Value::Null,
        })),
    );

    ctx.build()
}

/// Merge the policy's opaque `replica_spec` with the autoscaler-driven service
/// fields into the StageSpec JSON the engine reads. `residency_zone` is omitted
/// when empty (so the engine keeps the byte-stable no-residency render).
pub fn build_replica_spec(policy: &ModelAutoscalePolicy, target: u32) -> Value {
    // A non-object replica_spec is tolerated as "no extra resources".
    let mut spec = if policy.replica_spec.is_object() {
        policy.replica_spec.clone()
    } else {
        json!({})
    };
    let obj = spec.as_object_mut().expect("object by construction");
    obj.insert("job_type".to_string(), json!("service"));
    obj.insert("replicas".to_string(), json!(target as i64));
    if !policy.residency_zone.trim().is_empty() {
        obj.insert("residency_zone".to_string(), json!(policy.residency_zone));
    }
    spec
}

/// A stable, Nomad-safe job slug for a replica row. `model-<sanitized-model>-<8>`.
pub fn replica_slug(model_id: &str, replica_id: Uuid) -> String {
    let sanitized: String = model_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let short = &replica_id.to_string()[..8];
    format!("model-{sanitized}-{short}")
}

/// Actuate a replica row to `target` Count (provision / scale / teardown — one
/// path). Resolves the datacenter, applies the GDPR fail-closed residency guard,
/// builds + deploys a FRESH `model-replica-<row>-<generation>` net, then reaps
/// the prior generation's (now terminal) net. Returns the registered slug.
///
/// `generation` is this actuation's monotonic stamp (the loop passes
/// `last_actuated_at.timestamp_millis()`); `prev_generation` is the stamp of the
/// net last deployed for this row (`None` on first actuation). A fresh net id per
/// actuation is what makes scale/teardown actually fire — a stable-per-row id let
/// the one-shot net reach its terminal marking, after which re-POSTing the same
/// id never re-seeds `t_stage` (the P4-L1 bug). The Nomad job slug stays stable
/// across generations, so the fresh net re-registers the same service job in
/// place at the new Count.
///
/// A *deploy* failure is an `Err` (the loop records it on the row's `last_error`
/// with a `failed` status); a clean deploy returns the slug (the loop flips the
/// row to `provisioning`/`scaling`/`draining`). The terminal cluster outcome is
/// folded onto the row by the `model_replicas` projection.
#[allow(clippy::too_many_arguments)]
pub async fn actuate_replica(
    db: &PgPool,
    petri: &PetriClient,
    workspace_id: Uuid,
    replica_id: Uuid,
    generation: i64,
    prev_generation: Option<i64>,
    policy: &ModelAutoscalePolicy,
    datacenter_resource_id: Uuid,
    target: u32,
) -> Result<String, ActuateError> {
    // Resolve the target cluster connection.
    let conn = resolve_datacenter_connection(db, workspace_id, datacenter_resource_id)
        .await
        .map_err(|e| ActuateError::new(format!("resolve datacenter connection: {e}")))?
        .ok_or_else(|| {
            ActuateError::new(format!(
                "datacenter resource {datacenter_resource_id} not found in workspace, or missing \
                 its flavor's required connection field"
            ))
        })?;

    // GDPR fail-closed: residency is only honored on the Nomad renderer. Refuse
    // rather than place unconstrained on a Slurm (or other) datacenter.
    if !policy.residency_zone.trim().is_empty() && conn.scheduler_flavor != "nomad" {
        return Err(ActuateError::new(format!(
            "GDPR fail-closed: residency_zone '{}' requires a Nomad datacenter, but resource \
             {datacenter_resource_id} is flavor '{}' (which does not honor placement constraints) \
             — refusing to provision unconstrained",
            policy.residency_zone, conn.scheduler_flavor
        )));
    }

    let slug = replica_slug(&policy.model_id, replica_id);
    let spec = build_replica_spec(policy, target);
    let air = serde_json::to_value(build_model_replica_net(
        replica_id, generation, &slug, &conn, spec,
    ))
    .map_err(|e| ActuateError::new(format!("serialize model-replica net AIR: {e}")))?;
    let net_id = well_known::model_replica_net_id(replica_id, generation);

    crate::petri::instance::deploy_instance(
        petri,
        &net_id,
        &air,
        petri_api_types::DispatchOptions::default(),
        None,
    )
    .await
    .map_err(|e| ActuateError::new(format!("deploy model-replica net: {e}")))?;

    tracing::info!(
        %net_id, %slug, %datacenter_resource_id, target, generation,
        model_id = %policy.model_id,
        "deployed model-replica actuation net"
    );

    // Reap the prior generation's net — it's a completed one-shot at its terminal
    // marking, so this only frees engine registry state; the `service` job it
    // registered lives under the stable `slug` and was just updated in place by
    // the fresh net above (NOT torn down). Best-effort: a 404 (already hibernated)
    // is fine, and a reap failure must not fail an otherwise-good actuation.
    if let Some(prev) = prev_generation.filter(|p| *p != generation) {
        let prev_net = well_known::model_replica_net_id(replica_id, prev);
        if let Err(e) = petri.terminate_net(&prev_net).await {
            tracing::warn!(%prev_net, "failed to reap prior model-replica net (non-fatal): {e}");
        }
    }

    Ok(slug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy(zone: &str) -> ModelAutoscalePolicy {
        ModelAutoscalePolicy {
            model_id: "Qwen2.5-7B".to_string(),
            datacenter_resource_id: "dev-nomad".to_string(),
            residency_zone: zone.to_string(),
            min_replicas: 0,
            max_replicas: 4,
            mode: "manual".to_string(),
            desired_replicas: Some(1),
            scale_up_threshold: None,
            scale_down_threshold: None,
            cooldown_secs: None,
            replica_spec: json!({ "image": "vllm/vllm-openai:latest", "gpus": 1 }),
        }
    }

    #[test]
    fn replica_spec_sets_service_count_and_residency() {
        let spec = build_replica_spec(&policy("eu-west"), 2);
        assert_eq!(spec["job_type"], json!("service"));
        assert_eq!(spec["replicas"], json!(2));
        assert_eq!(spec["residency_zone"], json!("eu-west"));
        // Opaque replica_spec carried through.
        assert_eq!(spec["image"], json!("vllm/vllm-openai:latest"));
        assert_eq!(spec["gpus"], json!(1));
    }

    #[test]
    fn empty_residency_is_omitted_for_byte_stable_render() {
        let spec = build_replica_spec(&policy("   "), 1);
        assert!(spec.get("residency_zone").is_none());
        assert_eq!(spec["job_type"], json!("service"));
    }

    #[test]
    fn slug_is_sanitized_and_stable() {
        let id = Uuid::parse_str("aabbccdd-1111-2222-3333-444455556666").unwrap();
        let s = replica_slug("Qwen2.5-7B", id);
        assert_eq!(s, "model-qwen2-5-7b-aabbccdd");
        // Deterministic.
        assert_eq!(s, replica_slug("Qwen2.5-7B", id));
    }

    #[test]
    fn net_id_is_generation_discriminated_but_slug_is_stable() {
        // An actuation is an EVENT: each generation gets a FRESH net id (so the
        // engine re-seeds + re-fires `t_stage`), while the Nomad job slug is
        // STABLE across generations (so the service job is updated in place).
        let id = Uuid::parse_str("aabbccdd-1111-2222-3333-444455556666").unwrap();
        let g1 = crate::compiler::well_known::model_replica_net_id(id, 1717000000000);
        let g2 = crate::compiler::well_known::model_replica_net_id(id, 1717000015000);
        assert_ne!(g1, g2, "distinct generations must yield distinct net ids");
        assert!(g1.starts_with(&format!("model-replica-{id}-")));
        assert_eq!(
            replica_slug("Qwen2.5-7B", id),
            replica_slug("Qwen2.5-7B", id),
            "slug stays stable across generations",
        );
    }
}
