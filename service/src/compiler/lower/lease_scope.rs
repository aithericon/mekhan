//! `WorkflowNodeData::LeaseScope` lowering. A container that HOLDS one unit of
//! capacity across its whole interior region — EITHER a `datacenter` allocation
//! OR a presence `capacity` runner — acquiring on enter and releasing on exit.
//! Any body step inside the scope enqueues onto the held unit's lease-scoped NATS
//! namespace (`lease-<grant>` for a datacenter's drain executor, `runner.<id>`
//! for a held lab runner) — implicit by containment, no per-step flag (see
//! `enclosing_leased_scope_slug` in `lower::automated_step`). The backend is
//! resolved from the `lease.pool` alias via the `LeaseHolder` role; a presence
//! lease additionally cap-matches the runner via the scope's `requirements`.
//!
//! Unlike a leased `Loop` (which adds a body cycle: `t_continue` + a guarded,
//! held-consuming exit), a LeaseScope is *straight-through*: the body runs once,
//! and a single unguarded `t_<id>_exit` releases the lease. Compose `Loop INSIDE
//! a LeaseScope` for warm iteration, or sequential steps for a warm pipeline.
//!
//! The claim/grant/register/release handshake + the parked lease envelope
//! (`p_<id>_data`, holding `{ lease: grant }`) + the held-alloc-death fail-fast
//! are owned by the shared `emit_lease_bridge` (also used by the leased Loop), so
//! the live lease e2e stay byte-identical when a leased Loop is re-expressed as
//! `LeaseScope { Loop { … } }`.

use super::*;

pub(crate) fn lower_lease_scope(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::LeaseScope {
        label,
        lease,
        requirements,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_lease_scope on non-LeaseScope node")
    };

    // Placement Requirements → Rhai literal, captured while we still hold the
    // node borrow. Only folded into the claim for a PRESENCE-backed lease (the
    // scope picks WHICH runner to hold via `satisfies(claim.requirements,
    // unit.caps)`); a datacenter lease ignores it (the `request` shapes the
    // alloc). `None` / empty ⇒ `#{ constraints: [] }` (matches any unit) — same
    // contract as a presence-pooled `AutomatedStep`.
    let requirements_rhai = match requirements {
        Some(req) if !req.constraints.is_empty() => {
            json_to_rhai_literal(&serde_json::to_value(req).unwrap_or_default())
        }
        _ => "#{ constraints: [] }".to_string(),
    };

    // A LeaseScope must contain at least one body node (`parent_id == id`).
    // An empty scope holds an allocation no step runs on — reject at publish so
    // the editor can ring the offending container.
    if cx.children.is_empty() {
        return Err(CompileError::LeaseScopeEmpty {
            node_id: id.clone(),
        });
    }

    // Resolve the lease binding BEFORE the `&mut *cx.ctx` reborrow (which blocks
    // `cx.fixups` / `cx.known_resources`). `lease` is REQUIRED here (non-Option)
    // — `validate_lease_scope` rejects an empty `pool` alias — so there is no
    // None arm. The `LeaseHolder` role accepts a `datacenter` (Scheduler) OR a
    // presence `capacity` (Presence): both park a typed lease whose
    // `executor_namespace` the body steps inherit by containment.
    let alias = lease.pool.trim();
    if alias.is_empty() {
        return Err(CompileError::Validation(format!(
            "lease scope '{}': `lease.pool` must name a capacity provider alias \
             (a datacenter or a presence runner pool)",
            id
        )));
    }
    let binding = super::automated_step::resolve_binding(
        id,
        alias,
        lease.request.as_ref(),
        super::automated_step::DeploymentRole::LeaseHolder,
        cx.known_resources,
        // A container spec keyed on the LeaseScope holder id is merged into the
        // lease claim `request` so a DATACENTER alloc's persistent drain executor
        // runs in the `.sif` (the body steps enqueue into that warm executor).
        // `resolve_binding` ignores the container for a presence lease (no `.sif`
        // — the runner is the host), so passing it is a no-op there.
        cx.container_specs.get(id),
    )?;
    // Record the typed-lease definition + the grant-inbox place to type while we
    // still hold `cx` (the `&mut *cx.ctx` reborrow below blocks `cx.fixups`).
    // `compile_to_air` drains these after `ctx.build()` — identical to the
    // per-step pooled path and the leased Loop.
    cx.fixups
        .lease_definitions
        .push((binding.lease_def_name.clone(), binding.lease_schema.clone()));
    cx.fixups.lease_inbox_schemas.push((
        format!("p_{id}_grant_inbox"),
        binding.lease_def_name.clone(),
    ));

    let scope_group = cx.fixups.scope_groups.get(id).cloned();
    let d_slug = format!("d_{}", id.replace('-', "_"));
    let ctx = &mut *cx.ctx;

    // Shared claim → acquire(ENTER) → register → park-held → fail-fast bridge.
    // `data_enter_extra = ""`: a LeaseScope parks ONLY `{ lease: grant }` (no
    // iteration counter — that's a Loop concern). `requirements_rhai` is folded
    // into the claim only when `binding.backend == Presence` (runner cap-match).
    let bridge =
        super::lease_bridge::emit_lease_bridge(ctx, id, label, &binding, "", &requirements_rhai);

    // t_{id}_exit — the LeaseScope's single terminal. Straight-through (NO
    // guard, NO continue): consume the body's final token + the held lease,
    // read-arc the parked envelope, forward the token, and arc to release_out.
    // The single `p_held` token is the structural release-exactly-once guarantee
    // (docs/14). A body failure propagates out the body's own error output and
    // is handled by the surrounding graph; the scope's own terminal is this exit.
    ctx.transition(format!("t_{id}_exit"), format!("{label} - Exit (release)"))
        .auto_input("input", &bridge.p_body_out)
        .read_input(d_slug.clone(), &bridge.p_data)
        .auto_input("held", &bridge.p_held)
        .auto_output("output", &bridge.p_output)
        .auto_output("release", &bridge.p_release_out)
        .logic_rhai("#{ output: input, release: #{ grant_id: held.grant_id } }")
        .done();

    cx.fixups
        .groups
        .push((format!("grp_{id}"), label.clone(), scope_group));

    let mut input_handles = HashMap::new();
    input_handles.insert("body_out".to_string(), bridge.p_body_out);

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: bridge.p_input,
            // Two source-handle outputs: default (None) is the scope's outer
            // `out` (post-exit); "body_in" is the inner handle that feeds body
            // children when they receive the acquired token from the scope.
            output_places: vec![
                (None, bridge.p_output),
                (Some("body_in".to_string()), bridge.p_body_in),
            ],
            input_places: HashMap::new(),
            input_handles,
        },
    );
    // LeaseScope is a parked producer: the held lease envelope is stored
    // write-once at `p_{id}_data` under a `lease` key, schemed by the foundation
    // pass and used by the read-arc synthesis to route `<scope>.lease.<field>`
    // references (body steps + downstream blocks).
    cx.publish_interface().data_port = Some(format!("p_{id}_data"));
    Ok(())
}

#[cfg(test)]
mod presence_lease_tests {
    //! A LeaseScope over a PRESENCE `capacity` (a single lab runner held across
    //! the whole interior) — the runner-based lease. Mirrors the datacenter lease
    //! path but resolves to the `Presence` backend, cap-matches the runner via the
    //! scope's `requirements`, and lets a plain body step inherit the held
    //! `runner.<id>` namespace by containment.
    use crate::compiler::named_global::globals_from_resources;
    use crate::compiler::resource_refs::{KnownResource, KnownResources};
    use crate::compiler::{compile_to_air_with_options, CompileOptions};
    use crate::models::template::WorkflowGraph;
    use serde_json::json;
    use std::collections::HashMap;
    use uuid::Uuid;

    fn logic_source(air: &serde_json::Value, transition_id: &str) -> String {
        let t = air
            .get("transitions")
            .and_then(|t| t.as_array())
            .expect("transitions array")
            .iter()
            .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(transition_id))
            .unwrap_or_else(|| panic!("transition {transition_id} not found"));
        let logic = t.get("logic").expect("transition has logic");
        logic
            .get("Rhai")
            .and_then(|l| l.get("source"))
            .and_then(|s| s.as_str())
            .or_else(|| logic.get("source").and_then(|s| s.as_str()))
            .unwrap_or_else(|| panic!("logic source not findable for {transition_id}"))
            .to_string()
    }

    /// A presence `capacity` resource whose axes resolve to the `Presence`
    /// backend (the `instrument` preset: presence · auto · presence_driven
    /// · predicate). `axes_for_resource("capacity", public)` deserializes the
    /// full axes out of `public_config`.
    fn presence_pool_known(alias: &str) -> KnownResources {
        let mut known = KnownResources::new();
        known.insert(
            alias.to_string(),
            KnownResource {
                id: Uuid::new_v4(),
                type_name: "capacity".to_string(),
                latest_version: 1,
                public_config: json!({
                    "liveness": "presence",
                    "acceptance": "auto",
                    "capacity_kind": "presence_driven",
                    "eligibility": "predicate",
                }),
            },
        );
        known
    }

    /// `start → lease[ step ] → end`: a LeaseScope over the presence pool with a
    /// `ros.robot_model == xarm6` placement requirement, holding one runner across
    /// a single plain-executor body step.
    fn presence_lease_graph() -> WorkflowGraph {
        serde_json::from_value(json!({
          "nodes": [
            {"id":"start","type":"start","slug":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"lease","type":"lease_scope","slug":"cell","position":{"x":120,"y":0},
             "data":{"type":"lease_scope","label":"Cell",
                "lease":{"pool":"xarm_fleet"},
                "requirements":{"constraints":[
                  {"capability":"ros","field":"robot_model","op":"eq","value":"xarm6"}]}}},
            {"id":"step","type":"automated_step","slug":"step","parentId":"lease","position":{"x":40,"y":60},
             "data":{"type":"automated_step","label":"Do",
                "executionSpec":{"backendType":"docker","config":{"image":"python:3.12-slim"}},
                "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                "deploymentModel":{"mode":"executor"}}},
            {"id":"end","type":"end","slug":"end","position":{"x":420,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e0","source":"start","target":"lease","targetHandle":"in","type":"sequence"},
            {"id":"e1","source":"lease","sourceHandle":"body_in","target":"step","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"step","target":"lease","targetHandle":"body_out","type":"sequence"},
            {"id":"e3","source":"lease","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }))
        .expect("presence-lease graph deser")
    }

    fn compile(graph: &WorkflowGraph) -> serde_json::Value {
        let known = presence_pool_known("xarm_fleet");
        let known_globals = globals_from_resources(&known);
        let crate::compiler::CompileArtifacts { air, .. } = compile_to_air_with_options(
            graph,
            "presence-lease",
            "test",
            &HashMap::new(),
            CompileOptions {
                known_globals: &known_globals,
                ..Default::default()
            },
        )
        .expect("a LeaseScope over a presence capacity must compile");
        air
    }

    /// The claim carries the scope's placement `requirements` so the presence
    /// pool's `satisfies(claim.requirements, unit.caps)`-guarded `t_grant` admits
    /// only a matching runner. (A datacenter lease's claim has NO `requirements`.)
    #[test]
    fn presence_lease_claim_carries_requirements() {
        let air = compile(&presence_lease_graph());
        let claim = logic_source(&air, "t_lease_claim");
        assert!(
            claim.contains("requirements:"),
            "presence-lease claim must carry `requirements`; got:\n{claim}"
        );
        assert!(
            claim.contains("robot_model") && claim.contains("xarm6"),
            "the cap-match constraint must be embedded in the claim; got:\n{claim}"
        );
    }

    /// The held-unit-death register is the PRESENCE shape (`unit_id: fail.unit_id`)
    /// — proof the bridge resolved to the presence backend, not the scheduler one
    /// (`grant_id: fail.grant_id`). The presence pool's `t_reap_held` routes a
    /// `{ runner_id, unit_id }` notice on the "fail" channel.
    #[test]
    fn presence_lease_uses_presence_fail_register() {
        let air = compile(&presence_lease_graph());
        let reg = logic_source(&air, "t_lease_lease_failed_register");
        assert!(
            reg.contains("unit_id: fail.unit_id"),
            "presence-lease death register must read the unit_id (presence shape); got:\n{reg}"
        );
    }

    /// A plain body step inherits the held runner's namespace by containment: its
    /// job stamps `executor_namespace` from a borrow into the scope's parked lease
    /// envelope (`<scope>.lease.executor_namespace`, post-build rewritten to a
    /// `d_<scope>` read-arc), so the job lands on the SAME held `runner.<id>`.
    #[test]
    fn presence_lease_body_inherits_held_namespace() {
        let air = compile(&presence_lease_graph());
        let prepare = logic_source(&air, "step/prepare");
        assert!(
            prepare.contains(".lease.executor_namespace"),
            "body step must borrow the scope's held executor_namespace; got:\n{prepare}"
        );
    }

    /// A LeaseScope over a `tokens` (concurrency-limit / seeded) capacity is
    /// rejected — a token has no held namespace for the body to inherit.
    #[test]
    fn lease_scope_over_tokens_is_rejected() {
        let mut known = KnownResources::new();
        known.insert(
            "limit".to_string(),
            KnownResource {
                id: Uuid::new_v4(),
                type_name: "capacity".to_string(),
                latest_version: 1,
                public_config: json!({
                    "liveness": "seeded",
                    "acceptance": "auto",
                    "capacity_kind": "fixed",
                    "capacity_amount": 4,
                    "eligibility": "partition",
                }),
            },
        );
        let known_globals = globals_from_resources(&known);
        let mut graph = presence_lease_graph();
        for n in &mut graph.nodes {
            if let crate::models::template::WorkflowNodeData::LeaseScope { lease, .. } =
                &mut n.data
            {
                lease.pool = "limit".to_string();
            }
        }
        let result = compile_to_air_with_options(
            &graph,
            "tokens-lease",
            "test",
            &HashMap::new(),
            CompileOptions {
                known_globals: &known_globals,
                ..Default::default()
            },
        );
        let msg = match result {
            Ok(_) => panic!("a LeaseScope over a tokens capacity must be rejected"),
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains("cannot back a held lease") || msg.contains("tokens"),
            "expected a LeaseScopeNotLeasable error, got: {msg}"
        );
    }

    /// The lease bridge emits a FAILURE-PATH release finalizer
    /// (`t_<id>_finally`): marked `finalizer: true` (so the engine fires it only
    /// during the post-failure drain, never normally), consuming the single
    /// held token and emitting the SAME `grant_id` release the success-path
    /// `t_<id>_exit` does. This is what stops a permanently-failed leased net
    /// from stranding its held runner/allocation.
    #[test]
    fn lease_scope_emits_failure_release_finalizer() {
        let air = compile(&presence_lease_graph());

        let t = air
            .get("transitions")
            .and_then(|t| t.as_array())
            .expect("transitions array")
            .iter()
            .find(|t| t.get("id").and_then(|v| v.as_str()) == Some("t_lease_finally"))
            .expect("lease bridge must emit a t_<id>_finally release finalizer");

        // It is a finalizer — never selected in normal evaluation.
        assert_eq!(
            t.get("finalizer").and_then(|v| v.as_bool()),
            Some(true),
            "t_lease_finally must carry finalizer: true; got:\n{t:#}"
        );

        // It consumes the single held token (release-exactly-once) — a
        // CONSUMING arc (not a read arc) from the held place.
        let held_arc = t
            .get("inputs")
            .and_then(|a| a.as_array())
            .expect("inputs array")
            .iter()
            .find(|a| {
                a.get("place").and_then(|v| v.as_str()) == Some("p_lease_held")
            })
            .expect("finalizer must consume p_lease_held");
        assert_ne!(
            held_arc.get("read").and_then(|v| v.as_bool()),
            Some(true),
            "the finalizer must CONSUME the held token, not read it"
        );

        // It emits the release on the release bridge with the same shape as exit.
        let finally_logic = logic_source(&air, "t_lease_finally");
        assert!(
            finally_logic.contains("grant_id: held.grant_id"),
            "finalizer release must carry the held grant_id; got:\n{finally_logic}"
        );
        let release_arc = t
            .get("outputs")
            .and_then(|a| a.as_array())
            .expect("outputs array")
            .iter()
            .find(|a| a.get("place").and_then(|v| v.as_str()) == Some("p_lease_release_out"));
        assert!(
            release_arc.is_some(),
            "finalizer must route its release to the pool release bridge"
        );

        // The normal exit is NOT a finalizer (it fires on body success).
        let exit = air
            .get("transitions")
            .and_then(|t| t.as_array())
            .unwrap()
            .iter()
            .find(|t| t.get("id").and_then(|v| v.as_str()) == Some("t_lease_exit"))
            .expect("t_lease_exit exists");
        assert_ne!(
            exit.get("finalizer").and_then(|v| v.as_bool()),
            Some(true),
            "the success-path exit must NOT be a finalizer"
        );
    }

    fn transition_ids(air: &serde_json::Value) -> Vec<String> {
        air.get("transitions")
            .and_then(|t| t.as_array())
            .expect("transitions array")
            .iter()
            .filter_map(|t| t.get("id").and_then(|v| v.as_str()).map(String::from))
            .collect()
    }

    fn guard_source(air: &serde_json::Value, transition_id: &str) -> String {
        let t = air
            .get("transitions")
            .and_then(|t| t.as_array())
            .expect("transitions array")
            .iter()
            .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(transition_id))
            .unwrap_or_else(|| panic!("transition {transition_id} not found"));
        let g = t.get("guard").unwrap_or(&serde_json::Value::Null);
        g.get("Rhai")
            .and_then(|x| x.get("source"))
            .and_then(|s| s.as_str())
            .or_else(|| g.get("source").and_then(|s| s.as_str()))
            .unwrap_or("")
            .to_string()
    }

    /// The pooled INHERIT-BYPASS: a presence-pooled step compiles with a
    /// `t_<id>_inherit` path guarded on an inherited `_executor_namespace`, while
    /// `t_<id>_claim` is guarded to the complementary case. This is what lets a
    /// pooled step (e.g. demo 40's pick/place primitives) run BOTH standalone
    /// (claim its own unit) AND under a held lease (inherit the held runner, no
    /// claim → no deadlock). The terminals split on the sentinel held grant_id.
    #[test]
    fn pooled_step_compiles_with_inherit_bypass() {
        let graph: WorkflowGraph = serde_json::from_value(json!({
          "nodes": [
            {"id":"start","type":"start","slug":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"step","type":"automated_step","slug":"step","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Run",
                "executionSpec":{"backendType":"docker","config":{"image":"python:3.12-slim"}},
                "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                "deploymentModel":{"mode":"executor","capacity":{"alias":"xarm_fleet"}}}},
            {"id":"end","type":"end","slug":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"step","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"step","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }))
        .expect("pooled-step graph deser");

        let known = presence_pool_known("xarm_fleet");
        let known_globals = globals_from_resources(&known);
        let crate::compiler::CompileArtifacts { air, .. } = compile_to_air_with_options(
            &graph,
            "pooled-inherit",
            "test",
            &HashMap::new(),
            CompileOptions {
                known_globals: &known_globals,
                ..Default::default()
            },
        )
        .expect("a pooled step must compile");

        let ids = transition_ids(&air);
        assert!(
            ids.iter().any(|i| i == "t_step_inherit"),
            "pooled step must emit the inherit-bypass transition, got: {ids:?}"
        );
        // t_claim only fires when NOTHING is inherited; t_inherit only when a
        // namespace IS inherited — mutually exclusive on the single input token.
        assert!(
            guard_source(&air, "t_step_claim").contains("_executor_namespace == ()"),
            "t_claim must be guarded to the no-inherit case"
        );
        assert!(
            guard_source(&air, "t_step_inherit").contains("_executor_namespace != ()"),
            "t_inherit must be guarded to the inherited case"
        );
        // The bypass dispatches on the inherited namespace, with no claim.
        assert!(
            logic_source(&air, "t_step_inherit")
                .contains("d.executor_namespace = input._executor_namespace"),
            "t_inherit must stamp the inherited namespace onto the job"
        );
        // Terminals split on the sentinel held grant_id so release-exactly-once
        // holds per path (claim path releases; inherit path does not).
        assert!(
            guard_source(&air, "t_step_to_output").contains("held.grant_id != ()"),
            "the claim-path success terminal must guard on a real held grant"
        );
        assert!(
            guard_source(&air, "t_step_inherit_to_output").contains("held.grant_id == ()"),
            "the inherit-path success terminal must guard on the sentinel held"
        );
    }
}
