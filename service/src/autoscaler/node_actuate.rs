//! Provisioning seam for the node-fleet scaler (docs/31 Phase 2, Loop 1).
//!
//! A near-verbatim clone of [`crate::autoscaler::actuate`] re-targeted from a
//! per-MODEL replica onto a generic vLLM-engine NODE pool. Where `actuate.rs`
//! provisions a single-model service job (with `model_id` baked into the spec),
//! this provisions a MODEL-AGNOSTIC engine fleet: the spec carries the pool's
//! opaque `engine_spec` (vLLM image / `--enable-lora` / `--enable-sleep-mode` /
//! gpus) plus `job_type=service` + `replicas=<node Count>` + `residency_zone`, and
//! carries **NO `model_id`** — models are loaded/unloaded onto the running nodes by
//! the placement controller (Loop 2, Phase 3).
//!
//! Provision / scale / teardown are ONE path: deploy a net carrying `replicas =
//! target`. `target == 0` registers the service at Count 0 (a drain). The engine
//! renderer keeps the byte-stable batch render when these fields are absent (P3b
//! regression guard), so this never perturbs the lease-executor path.
//!
//! ## GDPR fail-closed (doc 28 §11, doc 31 OQ-4 / DERIVED-A)
//!
//! `NodePoolPolicy.residency_zone` is the SINGLE residency-zone source — a
//! non-empty zone is a HARD placement constraint. The engine fails closed at
//! placement (an unsatisfiable zone leaves the allocation pending, never
//! outside-zone). We ADD the same mekhan-side fail-closed guard as `actuate.rs`:
//! the Slurm leg of `render_parameterized_job` does not honor residency, so we
//! REFUSE to provision a non-empty zone onto a non-Nomad datacenter rather than
//! silently placing it unconstrained.
//!
//! The `e16db353` generation-keyed pattern is lifted as-is: a fresh
//! `node-pool-<id>-<gen>` net per actuation re-registers the stable Nomad slug in
//! place; the prior generation is reaped.

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use aithericon_resources::types::NodePoolPolicy;
use aithericon_sdk::scenario::ScenarioDefinition;
use aithericon_sdk::{effects, Context, DynamicToken};

use crate::compiler::well_known;
use crate::petri::client::PetriClient;
use crate::petri::pool_net::DatacenterConnection;
use crate::petri::staging_net::resolve_datacenter_connection;

/// A node-pool actuation failure. Distinct from a *cluster* failure (which the
/// engine records as `status:"failed"` DATA on the net's success port) — this is a
/// mekhan-side refusal (config / fail-closed) or a deploy failure.
#[derive(Debug)]
pub struct NodeActuateError {
    pub message: String,
}

impl NodeActuateError {
    fn new(m: impl Into<String>) -> Self {
        Self { message: m.into() }
    }
}

impl std::fmt::Display for NodeActuateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Build the AIR for a one-shot node-pool actuation run. Mirrors
/// [`crate::autoscaler::actuate::build_model_replica_net`] but for the generic
/// engine fleet: the echoed `staging_id` (= the `node_replicas` row id) correlates
/// the effect result back to the row.
///
/// `spec` is the StageSpec-shaped JSON the engine deserializes off the request
/// token — built by [`build_engine_spec`] (the pool's `engine_spec` merged with
/// `residency_zone` / `replicas` / `job_type=service`, **no `model_id`**).
pub fn build_node_pool_net(
    pool_id: Uuid,
    generation: i64,
    slug: &str,
    conn: &DatacenterConnection,
    spec: Value,
) -> ScenarioDefinition {
    let net_id = well_known::node_pool_net_id(pool_id, generation);
    let flavor = conn.scheduler_flavor.as_str();
    let mut ctx = Context::new(net_id).description(format!(
        "Node-pool actuation {pool_id}: register generic engine service job '{slug}' on \
         datacenter resource {} (flavor {flavor}) via the stage_template engine effect.",
        conn.resource_id
    ));

    let effect_config = conn.effect_config();

    let start: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.state("start", "Node-Pool Request");
    let staged: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.terminal("staged", "Actuated");
    let failed: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.terminal("failed", "Actuation Failed (fatal)");

    ctx.transition("t_stage", "Register Engine Fleet Job")
        .auto_input("request", &start)
        .auto_output("staged", &staged)
        .auto_output("_error", &failed)
        .effect_with_config(effects::STAGE_TEMPLATE.handler_id, effect_config);

    ctx.seed_one(
        &start,
        DynamicToken(json!({
            "staging_id": pool_id.to_string(),
            "slug": slug,
            "spec": spec,
            "escape_hatch": {},
            "package_ref": Value::Null,
        })),
    );

    ctx.build()
}

/// Merge the pool's opaque, MODEL-AGNOSTIC `engine_spec` with the autoscaler-driven
/// service fields into the StageSpec JSON the engine reads. `residency_zone` is
/// omitted when empty (so the engine keeps the byte-stable no-residency render).
///
/// Critically carries **NO `model_id`**: the engine boots a generic vLLM node;
/// models are placed onto it by the placement controller (Loop 2).
pub fn build_engine_spec(pool: &NodePoolPolicy, target: u32) -> Value {
    // A non-object engine_spec is tolerated as "no extra resources".
    let mut spec = if pool.engine_spec.is_object() {
        pool.engine_spec.clone()
    } else {
        json!({})
    };
    let obj = spec.as_object_mut().expect("object by construction");
    obj.insert("job_type".to_string(), json!("service"));
    obj.insert("replicas".to_string(), json!(target as i64));
    if !pool.residency_zone.trim().is_empty() {
        obj.insert("residency_zone".to_string(), json!(pool.residency_zone));
    }
    spec
}

/// A stable, Nomad-safe job slug for a node-pool row. `node-pool-<8>` (the engine
/// fleet has no model_id to sanitize into the name — the pool id is its identity).
pub fn node_pool_slug(pool_id: Uuid) -> String {
    let short = &pool_id.to_string()[..8];
    format!("node-pool-{short}")
}

/// Actuate a node-pool row to `target` node Count (provision / scale / teardown —
/// one path). Resolves the datacenter, applies the GDPR fail-closed residency
/// guard, builds + deploys a FRESH `node-pool-<row>-<generation>` net, then reaps
/// the prior generation's (now terminal) net. Returns the registered slug.
///
/// Forks [`crate::autoscaler::actuate::actuate_replica`] exactly — same gen-keyed
/// net id + stable slug + prior-gen reap + the line-187 residency guard — but drops
/// `model_id` from the engine spec entirely.
///
/// `generation` is this actuation's monotonic stamp (the loop passes
/// `last_actuated_at.timestamp_millis()`); `prev_generation` is the stamp of the
/// net last deployed for this row (`None` on first actuation). A fresh net id per
/// actuation is what makes scale/teardown actually fire — a stable-per-row id let
/// the one-shot net reach its terminal marking, after which re-POSTing the same id
/// never re-seeds `t_stage` (the `e16db353` bug). The Nomad job slug stays stable
/// across generations, so the fresh net re-registers the same service job in place
/// at the new Count.
#[allow(clippy::too_many_arguments)]
pub async fn actuate_node_pool(
    db: &PgPool,
    petri: &PetriClient,
    workspace_id: Uuid,
    pool_id: Uuid,
    generation: i64,
    prev_generation: Option<i64>,
    pool: &NodePoolPolicy,
    datacenter_resource_id: Uuid,
    target: u32,
) -> Result<String, NodeActuateError> {
    // Resolve the target cluster connection.
    let conn = resolve_datacenter_connection(db, workspace_id, datacenter_resource_id)
        .await
        .map_err(|e| NodeActuateError::new(format!("resolve datacenter connection: {e}")))?
        .ok_or_else(|| {
            NodeActuateError::new(format!(
                "datacenter resource {datacenter_resource_id} not found in workspace, or missing \
                 its flavor's required connection field"
            ))
        })?;

    // GDPR fail-closed: residency is only honored on the Nomad renderer. Refuse
    // rather than place unconstrained on a Slurm (or other) datacenter. Verbatim
    // from `actuate.rs:187` (zone from `NodePoolPolicy.residency_zone`).
    if !pool.residency_zone.trim().is_empty() && conn.scheduler_flavor != "nomad" {
        return Err(NodeActuateError::new(format!(
            "GDPR fail-closed: residency_zone '{}' requires a Nomad datacenter, but resource \
             {datacenter_resource_id} is flavor '{}' (which does not honor placement constraints) \
             — refusing to provision unconstrained",
            pool.residency_zone, conn.scheduler_flavor
        )));
    }

    let slug = node_pool_slug(pool_id);
    let spec = build_engine_spec(pool, target);
    let air = serde_json::to_value(build_node_pool_net(
        pool_id, generation, &slug, &conn, spec,
    ))
    .map_err(|e| NodeActuateError::new(format!("serialize node-pool net AIR: {e}")))?;
    let net_id = well_known::node_pool_net_id(pool_id, generation);

    crate::petri::instance::deploy_instance(
        petri,
        &net_id,
        &air,
        petri_api_types::DispatchOptions::default(),
        None,
    )
    .await
    .map_err(|e| NodeActuateError::new(format!("deploy node-pool net: {e}")))?;

    tracing::info!(
        %net_id, %slug, %datacenter_resource_id, target, generation,
        "deployed node-pool actuation net"
    );

    // Reap the prior generation's net — it's a completed one-shot at its terminal
    // marking, so this only frees engine registry state; the `service` job it
    // registered lives under the stable `slug` and was just updated in place by the
    // fresh net above (NOT torn down). Best-effort: a 404 (already hibernated) is
    // fine, and a reap failure must not fail an otherwise-good actuation.
    if let Some(prev) = prev_generation.filter(|p| *p != generation) {
        let prev_net = well_known::node_pool_net_id(pool_id, prev);
        if let Err(e) = petri.terminate_net(&prev_net).await {
            tracing::warn!(%prev_net, "failed to reap prior node-pool net (non-fatal): {e}");
        }
    }

    Ok(slug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pool(zone: &str) -> NodePoolPolicy {
        NodePoolPolicy {
            datacenter_resource_id: "dev-nomad".to_string(),
            residency_zone: zone.to_string(),
            gpu_class: "a100-80gb".to_string(),
            max_num_seqs: 8,
            engine_spec: json!({ "image": "vllm/vllm-openai:latest", "gpus": 1 }),
            min_nodes: 0,
            max_nodes: 4,
            cooldown_secs: None,
        }
    }

    #[test]
    fn engine_spec_sets_service_count_and_residency_and_no_model_id() {
        let spec = build_engine_spec(&pool("eu-west"), 2);
        assert_eq!(spec["job_type"], json!("service"));
        assert_eq!(spec["replicas"], json!(2));
        assert_eq!(spec["residency_zone"], json!("eu-west"));
        // Opaque engine_spec carried through.
        assert_eq!(spec["image"], json!("vllm/vllm-openai:latest"));
        assert_eq!(spec["gpus"], json!(1));
        // The fleet is model-agnostic — NEVER a model_id.
        assert!(spec.get("model_id").is_none());
    }

    #[test]
    fn empty_residency_is_omitted_for_byte_stable_render() {
        let spec = build_engine_spec(&pool("   "), 1);
        assert!(spec.get("residency_zone").is_none());
        assert_eq!(spec["job_type"], json!("service"));
    }

    #[test]
    fn slug_is_stable_per_pool() {
        let id = Uuid::parse_str("aabbccdd-1111-2222-3333-444455556666").unwrap();
        let s = node_pool_slug(id);
        assert_eq!(s, "node-pool-aabbccdd");
        // Deterministic.
        assert_eq!(s, node_pool_slug(id));
    }

    #[test]
    fn net_id_is_generation_discriminated_but_slug_is_stable() {
        // An actuation is an EVENT: each generation gets a FRESH net id (so the
        // engine re-seeds + re-fires `t_stage`), while the Nomad job slug is STABLE
        // across generations (so the service job is updated in place).
        let id = Uuid::parse_str("aabbccdd-1111-2222-3333-444455556666").unwrap();
        let g1 = well_known::node_pool_net_id(id, 1717000000000);
        let g2 = well_known::node_pool_net_id(id, 1717000015000);
        assert_ne!(g1, g2, "distinct generations must yield distinct net ids");
        assert!(g1.starts_with(&format!("node-pool-{id}-")));
        assert_eq!(
            node_pool_slug(id),
            node_pool_slug(id),
            "slug stays stable across generations",
        );
    }
}
