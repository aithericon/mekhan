//! Requirements manifest — the auto-derived set of resource/pool *slots* a
//! compiled template needs bound at run time.
//!
//! Today every node binds a resource/pool by a workspace-relative ALIAS
//! (`AutomatedStep.capacity.alias`, `LeaseScope.lease.pool`, an Agent's LLM
//! `model.resource_alias`, a `Scheduled` step's `scheduler` datacenter, and the
//! `<path>.<field>` DataResource refs the resource scanners discover). That
//! alias is resolved at PUBLISH time and the concrete `resource_id` is baked
//! into the AIR — both as inlined `__resources` config/secret splices (keyed by
//! the alias/resource name) and as `pool-{resource_id}` backing-net references.
//!
//! This module makes those bindings first-class RUN-TIME parameters. At compile
//! we derive ONE [`RequirementSlot`] per DISTINCT resource/pool reference
//! (deduped by its alias key), typed by the resolved `resource_type`, tagged
//! with the [`SlotRole`] of the binding surface that introduced it. The slot
//! ALSO records the precise AIR addresses the concrete baseline baked for it
//! (see [`SlotAirAddresses`]) so a binding-aware launcher (Phase C) can
//! substitute a different effective resource without recompiling: rewrite the
//! `pool-{old_id}` net ids and re-splice the `__resources` entries under the
//! recorded keys.
//!
//! The manifest is computed alongside `interfaces`/`node_configs` in
//! [`crate::compiler::compile::CompileArtifacts`] and persisted in a new
//! `workflow_templates.requirements_json` column (Phase C). A template with NO
//! derived slots is byte-for-byte today's behaviour.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::compiler::lower::automated_step::{resolve_binding, DeploymentRole};
use crate::compiler::resource_refs::KnownResources;
use crate::compiler::CompileError;
use crate::models::template::{DeploymentModel, WorkflowGraph, WorkflowNodeData};

/// What kind of binding surface introduced a requirement slot. Mirrors the
/// compiler's [`crate::compiler::lower::automated_step::DeploymentRole`] plus a
/// fourth `DataResource` variant for the `<path>.<field>` envelope/credential
/// refs (which carry no [`DeploymentRole`] — they're plain resource reads, not
/// pool bindings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SlotRole {
    /// `Executor { capacity: { alias } }` / `HumanTask.capacity` — an in-net
    /// admission pool (Tokens or Presence). Bakes a `pool-{resource_id}` net id.
    ExecutorCapacity,
    /// `LeaseScope { lease.pool }` — a held lease (datacenter alloc or a single
    /// presence runner). Bakes a `pool-{resource_id}` net id.
    LeaseHolder,
    /// `Scheduled { scheduler }` (a standalone cluster lease) — a datacenter
    /// lease. Bakes a `pool-{resource_id}` net id.
    SchedulerLease,
    /// A `<path>.<field>` resource reference (secret/connection envelope or
    /// control-flow constant): an Agent's LLM `model.resource_alias`, a Python /
    /// LLM / Kreuzberg `<resource>.<field>` body ref, a `resource_alias` declared
    /// on a backend, etc. Bakes a `__resources["<alias>"]` splice entry (no pool
    /// net).
    DataResource,
}

impl SlotRole {
    /// Derive the slot role from a resolved [`crate::compiler::lower::automated_step::DeploymentRole`].
    /// The 1:1 mapping keeps the manifest's role typing in lockstep with the
    /// lowering's binding gate so they can't drift.
    pub(crate) fn from_deployment_role(
        role: crate::compiler::lower::automated_step::DeploymentRole,
    ) -> Self {
        use crate::compiler::lower::automated_step::DeploymentRole;
        match role {
            DeploymentRole::ExecutorCapacity => SlotRole::ExecutorCapacity,
            DeploymentRole::LeaseHolder => SlotRole::LeaseHolder,
            DeploymentRole::SchedulerLease => SlotRole::SchedulerLease,
        }
    }
}

/// One auto-derived requirement slot: a distinct resource/pool reference the
/// template needs bound at instance-creation time.
///
/// `key` is the binding ALIAS (the workspace-relative resource name) — the same
/// string the author typed and the same key the `__resources` splice is indexed
/// by. It dedupes references: two surfaces naming the same alias collapse to one
/// slot whose [`used_by`](Self::used_by) lists every referencing node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RequirementSlot {
    /// Stable slot key = the binding alias (workspace resource name). Unique
    /// within a manifest; the per-instance/per-workspace binding maps key →
    /// `resource_id` against it.
    pub key: String,
    /// Resolved resource type name (`postgres`, `openai`, a `capacity` /
    /// `datacenter` kind, …) of the resource the home-workspace baseline bound.
    /// The effective-binding resolution (Phase C) only accepts a substitute of a
    /// matching type.
    pub resource_type: String,
    /// Which binding surface introduced this slot. When the same alias is
    /// referenced through multiple roles (rare — e.g. a pool also read as a
    /// `<path>.<field>` constant), the FIRST-derived role wins and a pool role
    /// always beats `DataResource` (see [`derive_requirements`]).
    pub role: SlotRole,
    /// Whether the launch run-gate must reject if this slot is unbound. Pool
    /// bindings (`ExecutorCapacity` / `LeaseHolder` / `SchedulerLease`) are
    /// always required — an unbound pool has no backing net to dispatch against.
    /// `DataResource` slots are required too (a missing secret envelope would
    /// crash the step at run time), so today every derived slot is required;
    /// the field exists so a future "optional resource" surface can opt out.
    pub required: bool,
    /// The validated `request`/claim params (pool bindings) or `None`
    /// (`DataResource`). Surfaced so the binding UI (Phase E) can show what the
    /// slot expects, and so a substitute pool can be validated against the same
    /// claim shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_shape: Option<serde_json::Value>,
    /// Node ids that reference this slot. Merged across every surface naming the
    /// same alias. Sorted + deduped for stable serialization.
    pub used_by: Vec<String>,
}

/// The AIR addresses the concrete home-workspace baseline baked for ONE slot.
/// This is the substitution map a binding-aware launcher (Phase C) consumes:
/// for any slot whose effective resource differs from the baked baseline, it
/// rewrites every `pool-{baked_resource_id}` net-id occurrence to
/// `pool-{effective_resource_id}` and re-splices the `__resources` entries under
/// the recorded [`resource_keys`](Self::resource_keys).
///
/// Precision matters: a binding that bakes NO AIR address (a pure
/// name-resolution / control-flow-constant ref that never reaches a pool net or
/// a secret splice) records EMPTY vectors — the launcher then has nothing to
/// substitute for it and treats it as a no-op rewrite.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SlotAirAddresses {
    /// The `pool-{resource_id}` backing-net id(s) this slot baked. Non-empty for
    /// pool roles (`ExecutorCapacity` / `LeaseHolder` / `SchedulerLease`); empty
    /// for `DataResource`. The launcher rewrites every occurrence
    /// `pool-{old} -> pool-{new}` across the AIR (bridge subjects, spawn refs,
    /// backing-net references).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub net_ids: Vec<String>,
    /// The `__resources["<key>"]` splice key(s) this slot baked — the alias name
    /// the resolver's secret/connection envelope is indexed under. Non-empty for
    /// any slot whose resource rides a `__resources` splice (`DataResource`, and
    /// pool resources whose connection config the body also reads); empty
    /// otherwise. The launcher re-resolves + re-splices these keys for the
    /// effective resource.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resource_keys: Vec<String>,
}

/// The full requirements artifact threaded through
/// [`crate::compiler::compile::CompileArtifacts`]. Pairs the ordered slot list
/// with the per-slot baked-AIR-address map (keyed by [`RequirementSlot::key`]).
///
/// Kept as a SEPARATE artifact from `interfaces` (per the phase contract — the
/// interface registry must not be overloaded). Serialized whole into
/// `workflow_templates.requirements_json` (Phase C).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RequirementsManifest {
    /// Auto-derived slots, sorted by `key` for stable serialization.
    pub slots: Vec<RequirementSlot>,
    /// Per-slot baked AIR addresses, keyed by [`RequirementSlot::key`]. Every
    /// slot in [`slots`](Self::slots) has an entry (possibly with empty vectors
    /// when the binding baked no AIR address).
    #[serde(default)]
    pub air_addresses: BTreeMap<String, SlotAirAddresses>,
}

impl RequirementsManifest {
    /// True when no slots were derived — the template launches byte-for-byte as
    /// today (no binding-aware substitution, the launcher fast-paths it).
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// One pool-binding surface discovered while walking the graph: the node that
/// declares it, its alias, the validated `request`, and the [`DeploymentRole`]
/// gate. Collected first so [`derive_requirements`] resolves each through the
/// SAME [`resolve_binding`] path the lowering uses (so the slot's type + baked
/// net id match what publish actually bakes).
struct PoolSurface<'a> {
    node_id: &'a str,
    alias: String,
    request: Option<&'a serde_json::Value>,
    role: DeploymentRole,
}

/// Extract the pool-binding surface(s) a single node declares. Mirrors the
/// node-data arms `discover_resource_globals` walks (the alias-discovery seam)
/// so the requirements manifest covers exactly the surfaces that bake a
/// `pool-{id}` net id:
/// - `AutomatedStep`/`Agent` `Executor { capacity }` → [`DeploymentRole::ExecutorCapacity`]
/// - `AutomatedStep`/`Agent` `Scheduled { scheduler }` → [`DeploymentRole::SchedulerLease`]
/// - `HumanTask { capacity }` → [`DeploymentRole::ExecutorCapacity`]
/// - `LeaseScope { lease.pool }` → [`DeploymentRole::LeaseHolder`]
fn pool_surface_for_node(node: &crate::models::template::WorkflowNode) -> Option<PoolSurface<'_>> {
    // `Executor.capacity` / `Scheduled.scheduler` share the same shape across
    // AutomatedStep and Agent (both carry a `deployment_model`).
    let deployment_surface = |dm: &DeploymentModel| -> Option<(String, DeploymentRole)> {
        match dm {
            DeploymentModel::Executor {
                capacity: Some(binding),
                ..
            } if !binding.alias.is_empty() => {
                Some((binding.alias.clone(), DeploymentRole::ExecutorCapacity))
            }
            DeploymentModel::Scheduled {
                scheduler: Some(alias),
                ..
            } if !alias.trim().is_empty() => {
                Some((alias.trim().to_string(), DeploymentRole::SchedulerLease))
            }
            _ => None,
        }
    };

    match &node.data {
        WorkflowNodeData::AutomatedStep {
            deployment_model, ..
        } => {
            let (alias, role) = deployment_surface(deployment_model)?;
            // The `request` only exists on the Executor.capacity binding; a
            // Scheduled.scheduler lease carries none (mirrors `resolve_binding`'s
            // call site, which passes `None` for the scheduler path).
            let request = match (deployment_model, role) {
                (
                    DeploymentModel::Executor {
                        capacity: Some(b), ..
                    },
                    DeploymentRole::ExecutorCapacity,
                ) => b.request.as_ref(),
                _ => None,
            };
            Some(PoolSurface {
                node_id: &node.id,
                alias,
                request,
                role,
            })
        }
        WorkflowNodeData::Agent {
            deployment_model, ..
        } => {
            let (alias, role) = deployment_surface(deployment_model)?;
            let request = match (deployment_model, role) {
                (
                    DeploymentModel::Executor {
                        capacity: Some(b), ..
                    },
                    DeploymentRole::ExecutorCapacity,
                ) => b.request.as_ref(),
                _ => None,
            };
            Some(PoolSurface {
                node_id: &node.id,
                alias,
                request,
                role,
            })
        }
        WorkflowNodeData::HumanTask {
            capacity: Some(binding),
            ..
        } if !binding.alias.is_empty() => Some(PoolSurface {
            node_id: &node.id,
            alias: binding.alias.clone(),
            request: binding.request.as_ref(),
            role: DeploymentRole::ExecutorCapacity,
        }),
        WorkflowNodeData::LeaseScope { lease, .. } if !lease.pool.trim().is_empty() => {
            Some(PoolSurface {
                node_id: &node.id,
                alias: lease.pool.trim().to_string(),
                request: lease.request.as_ref(),
                role: DeploymentRole::LeaseHolder,
            })
        }
        _ => None,
    }
}

/// The worker-GROUP routing partitions an executor-dispatched graph references —
/// the implicit `default` group (and any explicit `group` on a fungible
/// `Executor { capacity: None }` step). Mirrors the head-injection in
/// [`crate::process::discover`] (Pass 1: "Unified worker dispatch"): every
/// `AutomatedStep`/`Agent` routes through a worker group, so a step's group (or
/// `default`) lands in `envelope_heads` and the group's `capacity` resource gets
/// `envelope_used = true`.
///
/// These are NOT user-bindable run-time requirements: the fungible worker target
/// holds no reservation ("Runs on any worker serving this step's backend") and
/// the group is resolved + hard-failed at LOWERING against the registry, never
/// bound per-instance. Their `capacity` resource still rides the `__resources`
/// splice (the UUID is the routing partition), but [`derive_requirements`]
/// excludes a routing-ONLY head from becoming a [`SlotRole::DataResource`] slot —
/// otherwise it surfaces as a phantom "required, used-by-0-nodes" binding in the
/// run sheet. A genuine `Executor { capacity }` / lease / scheduler binding to
/// the same alias is a pool slot (derived in pass 1) and is kept.
fn worker_group_routing_heads(graph: &WorkflowGraph) -> BTreeSet<String> {
    let mut heads = BTreeSet::new();
    for node in &graph.nodes {
        let dm = match &node.data {
            WorkflowNodeData::AutomatedStep {
                deployment_model, ..
            }
            | WorkflowNodeData::Agent {
                deployment_model, ..
            } => deployment_model,
            _ => continue,
        };
        match dm {
            // Fungible worker pool: the step's explicit `group`, else `default`.
            DeploymentModel::Executor {
                capacity: None,
                group,
            } => {
                let alias = group
                    .as_deref()
                    .filter(|g| !g.is_empty())
                    .unwrap_or(crate::worker_groups::DEFAULT_WORKER_GROUP_PATH);
                heads.insert(alias.to_string());
            }
            // Pooled / Scheduled steps default-route their non-lease grant to the
            // workspace `default` group (same as discover.rs).
            _ => {
                heads.insert(crate::worker_groups::DEFAULT_WORKER_GROUP_PATH.to_string());
            }
        }
    }
    heads
}

/// Internal mutable slot accumulator — collapses surfaces by alias key.
struct SlotBuilder {
    resource_type: String,
    role: SlotRole,
    required: bool,
    request_shape: Option<serde_json::Value>,
    used_by: BTreeSet<String>,
    net_ids: BTreeSet<String>,
    resource_keys: BTreeSet<String>,
}

/// Derive the [`RequirementsManifest`] for a compiled graph.
///
/// ONE slot per DISTINCT resource/pool reference, deduped by alias `key`. The
/// derivation walks two seams and reuses (never duplicates) the existing
/// resolution logic:
///
/// 1. **Pool bindings** ([`pool_surface_for_node`]) — each is resolved through
///    [`resolve_binding`] (the same path the lowering uses), yielding the slot's
///    role + the baked `pool-{resource_id}` net id ([`SlotAirAddresses::net_ids`]).
///    The `resource_type` comes from the resolved [`KnownResources`] entry so it
///    matches what publish bakes. A resolution error here is propagated — it's
///    the same error the lowering would raise, surfaced once at compile.
///
/// 2. **DataResource refs** — every remaining [`KnownResources`] entry (the
///    `<path>.<field>` resource refs the scanners discovered, an Agent's LLM
///    `model.resource_alias`, declared backend `resource_alias`es, …) that is
///    NOT already a pool slot becomes a [`SlotRole::DataResource`] slot. Its
///    baked AIR address is the `__resources["<alias>"]` splice key
///    ([`SlotAirAddresses::resource_keys`]) — no pool net. (A resource reached
///    ONLY as a control-flow constant still appears in `KnownResources`; it
///    bakes no secret splice, so its `resource_keys` is left empty — the
///    launcher treats it as a no-op rewrite, matching the precision contract.)
///
/// When the SAME alias is both a pool binding AND a `KnownResources` entry whose
/// connection config a body reads, the pool slot wins the `role` (derived first)
/// and ADDITIONALLY records the `__resources` key (the body still reads it), so
/// the launcher substitutes both the net id and the secret splice.
///
/// `known` is empty on the analyze/preview/test compile paths (no DB) — then no
/// resource resolves, [`pool_surface_for_node`] surfaces still resolve via
/// `resolve_binding` (which hard-fails on an unknown alias exactly as today), so
/// those paths derive no DataResource slots and either error on an unresolved
/// pool alias or (the common case: no pool bindings) produce an empty manifest.
///
/// `envelope_used` is the subset of `known` that publish ACTUALLY splices a
/// `__resources` envelope for (the
/// [`crate::compiler::named_global::splice_resources_from_globals`] set —
/// resources reached through their secret/connection envelope, NOT those used
/// only as control-flow constants). A resource is only a late-bindable
/// `DataResource` slot — and only records `resource_keys` — when it is in
/// `envelope_used`: a control-flow-constant-only ref bakes NO `__resources`
/// splice, so there is nothing for the launcher to substitute and recording a
/// slot for it would make [`crate::petri::binding::baseline_satisfies`] wrongly
/// report it satisfied-by-baseline. Such a ref produces no slot and launches via
/// its baked constants exactly as today.
pub(crate) fn derive_requirements(
    graph: &WorkflowGraph,
    known: &KnownResources,
    envelope_used: &KnownResources,
) -> Result<RequirementsManifest, CompileError> {
    let mut builders: BTreeMap<String, SlotBuilder> = BTreeMap::new();

    // 1. Pool bindings — resolve through `resolve_binding` for role + net id.
    for node in &graph.nodes {
        let Some(surface) = pool_surface_for_node(node) else {
            continue;
        };
        let binding = resolve_binding(
            surface.node_id,
            &surface.alias,
            surface.request,
            surface.role,
            known,
            // The container spec only shapes the lease `request_rhai` (a `.sif`
            // wrap), never the resolved type or baked net id we read here — pass
            // `None` so derivation is independent of per-node container specs.
            None,
        )?;
        // `resource_type` from the resolved registry entry (matches publish's
        // baked type). `known` MAY be empty (analyze) — then `resolve_binding`
        // already hard-failed above on the unknown alias, so a present binding
        // implies a present entry; fall back to the empty string defensively.
        let resource_type = known
            .get(&surface.alias)
            .map(|r| r.type_name.clone())
            .unwrap_or_default();
        let role = SlotRole::from_deployment_role(surface.role);
        let entry = builders
            .entry(surface.alias.clone())
            .or_insert_with(|| SlotBuilder {
                resource_type: resource_type.clone(),
                role,
                // Pool bindings are always required.
                required: true,
                request_shape: surface.request.cloned(),
                used_by: BTreeSet::new(),
                net_ids: BTreeSet::new(),
                resource_keys: BTreeSet::new(),
            });
        entry.used_by.insert(surface.node_id.to_string());
        entry.net_ids.insert(binding.backing_net_id.clone());
        // Only record the `__resources["<alias>"]` splice key for a pool slot
        // whose body ALSO reads its config — i.e. publish actually splices an
        // envelope for it (`envelope_used`). A pool used purely for admission
        // (no body ref to its connection config) bakes no `__resources` entry,
        // so there is nothing to re-splice at launch.
        if envelope_used.contains_key(&surface.alias) {
            entry.resource_keys.insert(surface.alias.clone());
        }
        // First non-empty request_shape wins (surfaces naming the same alias
        // should carry the same request; keep the first seen for stability).
        if entry.request_shape.is_none() {
            entry.request_shape = surface.request.cloned();
        }
    }

    // 2. DataResource refs — every ENVELOPE-USED `KnownResources` entry not
    //    already a pool slot. Publish splices a `__resources["<alias>"]`
    //    envelope for exactly this set; a resource reached ONLY as a
    //    control-flow constant is NOT in `envelope_used`, bakes no splice, and
    //    so derives NO slot (it launches via its baked constants as today).
    //
    //    EXCLUDE worker-group routing partitions (the implicit `default` group
    //    and explicit fungible-step `group`s): they ride the `__resources`
    //    splice for their UUID but are resolved at lowering, not bound per-run —
    //    surfacing one as a slot is a phantom "required, used-by-0-nodes"
    //    binding. A real pool binding to the same alias already built a pass-1
    //    slot (`builders.contains_key`) and is preserved.
    let group_routing_heads = worker_group_routing_heads(graph);
    for (alias, info) in envelope_used {
        if group_routing_heads.contains(alias) && !builders.contains_key(alias) {
            continue;
        }
        let entry = builders.entry(alias.clone()).or_insert_with(|| SlotBuilder {
            resource_type: info.type_name.clone(),
            role: SlotRole::DataResource,
            required: true,
            request_shape: None,
            used_by: BTreeSet::new(),
            net_ids: BTreeSet::new(),
            resource_keys: BTreeSet::new(),
        });
        // The envelope-used resource rides the `__resources["<alias>"]` splice
        // (its secret/connection envelope) — record the key for both a pure
        // DataResource slot AND a pool slot whose body also reads its config.
        entry.resource_keys.insert(alias.clone());
    }

    // Materialize the ordered slots + the parallel address map.
    let mut slots = Vec::with_capacity(builders.len());
    let mut air_addresses: BTreeMap<String, SlotAirAddresses> = BTreeMap::new();
    for (key, b) in builders {
        slots.push(RequirementSlot {
            key: key.clone(),
            resource_type: b.resource_type,
            role: b.role,
            required: b.required,
            request_shape: b.request_shape,
            used_by: b.used_by.into_iter().collect(),
        });
        air_addresses.insert(
            key,
            SlotAirAddresses {
                net_ids: b.net_ids.into_iter().collect(),
                resource_keys: b.resource_keys.into_iter().collect(),
            },
        );
    }

    Ok(RequirementsManifest {
        slots,
        air_addresses,
    })
}

#[cfg(test)]
mod tests {
    //! Pure-compiler tests for requirements-manifest derivation. No DB, no live
    //! stack — `derive_requirements` walks a hand-built [`WorkflowGraph`] +
    //! [`KnownResources`] and reuses the same `resolve_binding` path the lowering
    //! uses. These run offline under `cargo test -p mekhan-service`.

    use super::*;
    use crate::compiler::resource_refs::{KnownResource, KnownResources};
    use crate::compiler::well_known::pool_net_id;
    use crate::models::template::{
        CapacityBinding, DeploymentModel, ExecutionBackendType, ExecutionSpecConfig, LeaseBinding,
        Port, Position, WorkflowGraph, WorkflowNode, WorkflowNodeData,
    };
    use uuid::Uuid;

    fn pos() -> Position {
        Position { x: 0.0, y: 0.0 }
    }

    /// An AutomatedStep node carrying a specific deployment model (the capacity /
    /// scheduled pool binding under test).
    fn auto(id: &str, dm: DeploymentModel) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "automated_step".to_string(),
            slug: None,
            position: pos(),
            data: WorkflowNodeData::AutomatedStep {
                label: "Run".to_string(),
                description: None,
                execution_spec: ExecutionSpecConfig {
                    backend_type: ExecutionBackendType::Docker,
                    entrypoint: None,
                    config: serde_json::json!({ "image": "alpine:latest" }),
                },
                input: Port::empty_input(),
                output: crate::models::template::default_output_port(
                    ExecutionBackendType::Docker,
                ),
                retry_policy: Default::default(),
                deployment_model: dm,
                channels: Vec::new(),
                requirements: None,
                asset_bindings: Vec::new(),
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    /// A LeaseScope node binding `pool` (the body is irrelevant to derivation —
    /// `derive_requirements` only reads the node's `lease.pool` surface).
    fn lease_scope(id: &str, pool: &str) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "lease_scope".to_string(),
            slug: None,
            position: pos(),
            data: WorkflowNodeData::LeaseScope {
                label: "Hold".to_string(),
                description: None,
                lease: LeaseBinding {
                    pool: pool.to_string(),
                    request: None,
                },
                requirements: None,
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    fn graph(nodes: Vec<WorkflowNode>) -> WorkflowGraph {
        WorkflowGraph {
            nodes,
            edges: Vec::new(),
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(),
            default_scheduler: None,
        }
    }

    /// A `capacity` resource whose public_config resolves to the Tokens backend
    /// (the `ExecutorCapacity` role accepts it).
    fn capacity_resource(id: Uuid) -> KnownResource {
        KnownResource {
            id,
            type_name: "capacity".to_string(),
            latest_version: 1,
            public_config: serde_json::json!({
                "liveness": "seeded",
                "acceptance": "auto",
                "capacity_kind": "fixed",
                "capacity_amount": 4,
                "eligibility": "partition",
            }),
        }
    }

    /// A `datacenter` resource — resolves to the Scheduler backend (accepted by
    /// `SchedulerLease` and `LeaseHolder`).
    fn datacenter_resource(id: Uuid) -> KnownResource {
        KnownResource {
            id,
            type_name: "datacenter".to_string(),
            latest_version: 1,
            public_config: serde_json::json!({
                "scheduler_flavor": "nomad",
                "nomad_addr": "http://nomad.test:4646",
            }),
        }
    }

    fn slot<'a>(m: &'a RequirementsManifest, key: &str) -> &'a RequirementSlot {
        m.slots
            .iter()
            .find(|s| s.key == key)
            .unwrap_or_else(|| panic!("no slot for key '{key}' in {:?}", m.slots))
    }

    #[test]
    fn empty_graph_derives_empty_manifest() {
        let m = derive_requirements(&graph(vec![]), &KnownResources::new(), &KnownResources::new())
            .expect("empty graph derives");
        assert!(m.is_empty());
        assert!(m.slots.is_empty());
        assert!(m.air_addresses.is_empty());
    }

    #[test]
    fn capacity_binding_derives_executor_capacity_slot_with_pool_net_address() {
        let cap_id = Uuid::new_v4();
        let mut known = KnownResources::new();
        known.insert("prod_gpu".to_string(), capacity_resource(cap_id));

        let g = graph(vec![auto(
            "step1",
            DeploymentModel::Executor {
                capacity: Some(CapacityBinding {
                    alias: "prod_gpu".to_string(),
                    request: None,
                }),
                group: None,
            },
        )]);

        let m = derive_requirements(&g, &known, &known).expect("derive");
        assert_eq!(m.slots.len(), 1, "one slot per distinct binding");
        let s = slot(&m, "prod_gpu");
        assert_eq!(s.resource_type, "capacity");
        assert_eq!(s.role, SlotRole::ExecutorCapacity);
        assert!(s.required, "pool bindings are always required");
        assert_eq!(s.used_by, vec!["step1".to_string()]);

        // The baked AIR address is the pool-{resource_id} net id.
        let addr = m.air_addresses.get("prod_gpu").expect("address");
        assert_eq!(addr.net_ids, vec![pool_net_id(cap_id)]);
        // The capacity resource also rides the __resources splice keyed by alias.
        assert_eq!(addr.resource_keys, vec!["prod_gpu".to_string()]);
    }

    #[test]
    fn scheduled_binding_derives_scheduler_lease_slot() {
        let dc_id = Uuid::new_v4();
        let mut known = KnownResources::new();
        known.insert("prod_dc".to_string(), datacenter_resource(dc_id));

        let g = graph(vec![auto(
            "sched",
            DeploymentModel::Scheduled {
                scheduler: Some("prod_dc".to_string()),
                job_template: "petri-worker".to_string(),
                job_template_ref: None,
                resources: None,
            },
        )]);

        let m = derive_requirements(&g, &known, &known).expect("derive");
        assert_eq!(m.slots.len(), 1);
        let s = slot(&m, "prod_dc");
        assert_eq!(s.resource_type, "datacenter");
        assert_eq!(s.role, SlotRole::SchedulerLease);
        assert_eq!(
            m.air_addresses.get("prod_dc").unwrap().net_ids,
            vec![pool_net_id(dc_id)]
        );
    }

    #[test]
    fn lease_scope_derives_lease_holder_slot() {
        let dc_id = Uuid::new_v4();
        let mut known = KnownResources::new();
        known.insert("cluster".to_string(), datacenter_resource(dc_id));

        let g = graph(vec![lease_scope("scope1", "cluster")]);
        let m = derive_requirements(&g, &known, &known).expect("derive");
        let s = slot(&m, "cluster");
        assert_eq!(s.role, SlotRole::LeaseHolder);
        assert_eq!(s.resource_type, "datacenter");
    }

    #[test]
    fn data_resource_ref_derives_data_resource_slot_no_pool_net() {
        // A `<path>.<field>` resource ref (e.g. a Python step reading a postgres
        // connection) shows up in KnownResources but is no pool binding — it
        // becomes a DataResource slot whose ONLY baked address is the
        // __resources splice key (no pool net).
        let pg_id = Uuid::new_v4();
        let mut known = KnownResources::new();
        known.insert(
            "main_db".to_string(),
            KnownResource {
                id: pg_id,
                type_name: "postgres".to_string(),
                latest_version: 2,
                public_config: serde_json::Value::Null,
            },
        );

        let m = derive_requirements(&graph(vec![]), &known, &known).expect("derive");
        assert_eq!(m.slots.len(), 1);
        let s = slot(&m, "main_db");
        assert_eq!(s.role, SlotRole::DataResource);
        assert_eq!(s.resource_type, "postgres");
        assert!(s.required);

        let addr = m.air_addresses.get("main_db").unwrap();
        assert!(addr.net_ids.is_empty(), "DataResource bakes no pool net");
        assert_eq!(addr.resource_keys, vec!["main_db".to_string()]);
    }

    #[test]
    fn control_flow_constant_only_resource_derives_no_slot() {
        // A resource reached ONLY as a control-flow constant (e.g. `demo_pg.port`
        // inside a guard) appears in `known` but NOT in the envelope-used set
        // publish splices — it bakes no `__resources` envelope. It MUST derive no
        // slot and record no resource_keys, else `baseline_satisfies` would
        // wrongly gate the launch on a binding the AIR never carries. It launches
        // via its baked constants exactly as today.
        let pg_id = Uuid::new_v4();
        let mut known = KnownResources::new();
        known.insert(
            "demo_pg".to_string(),
            KnownResource {
                id: pg_id,
                type_name: "postgres".to_string(),
                latest_version: 1,
                public_config: serde_json::json!({ "port": 5432 }),
            },
        );
        // Empty envelope_used: publish splices nothing for this resource.
        let envelope_used = KnownResources::new();

        let m = derive_requirements(&graph(vec![]), &known, &envelope_used).expect("derive");
        assert!(
            m.is_empty(),
            "control-flow-constant-only resource derives no slot: {:?}",
            m.slots
        );
        assert!(m.air_addresses.is_empty());
    }

    #[test]
    fn pool_alias_not_envelope_used_records_no_resource_keys() {
        // A pool whose body never reads its connection config is admission-only:
        // publish bakes its `pool-{id}` net id but NO `__resources` splice. The
        // slot still exists (the pool net is late-bindable) but records EMPTY
        // resource_keys — the launcher rewrites the net id only, never re-splices
        // a non-existent envelope.
        let cap_id = Uuid::new_v4();
        let mut known = KnownResources::new();
        known.insert("prod_gpu".to_string(), capacity_resource(cap_id));

        let g = graph(vec![auto(
            "step1",
            DeploymentModel::Executor {
                capacity: Some(CapacityBinding {
                    alias: "prod_gpu".to_string(),
                    request: None,
                }),
                group: None,
            },
        )]);
        // The pool is NOT envelope-used (no body ref to its config).
        let envelope_used = KnownResources::new();

        let m = derive_requirements(&g, &known, &envelope_used).expect("derive");
        assert_eq!(m.slots.len(), 1, "the pool slot still exists");
        let s = slot(&m, "prod_gpu");
        assert_eq!(s.role, SlotRole::ExecutorCapacity);
        let addr = m.air_addresses.get("prod_gpu").unwrap();
        assert_eq!(addr.net_ids, vec![pool_net_id(cap_id)], "pool net baked");
        assert!(
            addr.resource_keys.is_empty(),
            "admission-only pool records no __resources key: {:?}",
            addr.resource_keys
        );
    }

    #[test]
    fn worker_group_routing_head_derives_no_slot() {
        // A fungible `Executor { capacity: None }` step routes through the
        // implicit `default` worker group → discover.rs marks the `default`
        // capacity `envelope_used`. It must NOT become a bindable requirement
        // slot (routing infra resolved at lowering, not a per-run binding) — the
        // phantom "required, used-by-0-nodes" slot the run sheet showed.
        let grp_id = Uuid::new_v4();
        let mut envelope_used = KnownResources::new();
        envelope_used.insert("default".to_string(), capacity_resource(grp_id));

        let g = graph(vec![auto(
            "step1",
            DeploymentModel::Executor {
                capacity: None,
                group: None,
            },
        )]);

        let m = derive_requirements(&g, &envelope_used, &envelope_used).expect("derive");
        assert!(
            m.is_empty(),
            "worker-group routing head derives no slot: {:?}",
            m.slots
        );
    }

    #[test]
    fn pooled_step_default_route_does_not_leak_default_group_slot() {
        // A real `Executor { capacity }` pool binding ALSO default-routes its
        // non-lease grant to the `default` group (discover.rs), so `default`
        // shows up in `envelope_used` alongside the real pool. Only the pool slot
        // (`prod_gpu`) should derive — the `default` routing head is excluded.
        let cap_id = Uuid::new_v4();
        let grp_id = Uuid::new_v4();
        let mut known = KnownResources::new();
        known.insert("prod_gpu".to_string(), capacity_resource(cap_id));
        known.insert("default".to_string(), capacity_resource(grp_id));

        let g = graph(vec![auto(
            "step1",
            DeploymentModel::Executor {
                capacity: Some(CapacityBinding {
                    alias: "prod_gpu".to_string(),
                    request: None,
                }),
                group: None,
            },
        )]);

        let m = derive_requirements(&g, &known, &known).expect("derive");
        assert_eq!(m.slots.len(), 1, "only the real pool slot: {:?}", m.slots);
        assert_eq!(slot(&m, "prod_gpu").role, SlotRole::ExecutorCapacity);
        assert!(
            m.slots.iter().all(|s| s.key != "default"),
            "the `default` worker-group routing head is excluded"
        );
    }

    #[test]
    fn mixed_graph_derives_one_slot_per_distinct_reference_typed_and_roled() {
        // capacity + datacenter(scheduled) + lease + a plain data-resource ref —
        // four DISTINCT references → four slots, each typed + roled correctly.
        let cap_id = Uuid::new_v4();
        let dc_id = Uuid::new_v4();
        let lease_id = Uuid::new_v4();
        let pg_id = Uuid::new_v4();

        let mut known = KnownResources::new();
        known.insert("prod_gpu".to_string(), capacity_resource(cap_id));
        known.insert("prod_dc".to_string(), datacenter_resource(dc_id));
        known.insert("cluster".to_string(), datacenter_resource(lease_id));
        known.insert(
            "main_db".to_string(),
            KnownResource {
                id: pg_id,
                type_name: "postgres".to_string(),
                latest_version: 1,
                public_config: serde_json::Value::Null,
            },
        );

        let g = graph(vec![
            auto(
                "cap_step",
                DeploymentModel::Executor {
                    capacity: Some(CapacityBinding {
                        alias: "prod_gpu".to_string(),
                        request: None,
                    }),
                    group: None,
                },
            ),
            auto(
                "sched_step",
                DeploymentModel::Scheduled {
                    scheduler: Some("prod_dc".to_string()),
                    job_template: "petri-worker".to_string(),
                    job_template_ref: None,
                    resources: None,
                },
            ),
            lease_scope("scope1", "cluster"),
        ]);

        let m = derive_requirements(&g, &known, &known).expect("derive");
        assert_eq!(m.slots.len(), 4, "four distinct references → four slots");

        assert_eq!(slot(&m, "prod_gpu").role, SlotRole::ExecutorCapacity);
        assert_eq!(slot(&m, "prod_dc").role, SlotRole::SchedulerLease);
        assert_eq!(slot(&m, "cluster").role, SlotRole::LeaseHolder);
        assert_eq!(slot(&m, "main_db").role, SlotRole::DataResource);

        // Slots sorted by key for stable serialization.
        let keys: Vec<&str> = m.slots.iter().map(|s| s.key.as_str()).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "slots are key-sorted");
    }

    #[test]
    fn same_alias_two_nodes_dedupes_to_one_slot_merging_used_by() {
        // Two AutomatedSteps both bind `prod_gpu` → ONE slot whose `used_by`
        // lists both node ids (sorted + deduped).
        let cap_id = Uuid::new_v4();
        let mut known = KnownResources::new();
        known.insert("prod_gpu".to_string(), capacity_resource(cap_id));

        let mk = |id: &str| {
            auto(
                id,
                DeploymentModel::Executor {
                    capacity: Some(CapacityBinding {
                        alias: "prod_gpu".to_string(),
                        request: None,
                    }),
                    group: None,
                },
            )
        };
        let g = graph(vec![mk("step_b"), mk("step_a")]);

        let m = derive_requirements(&g, &known, &known).expect("derive");
        assert_eq!(m.slots.len(), 1, "same alias collapses to one slot");
        let s = slot(&m, "prod_gpu");
        assert_eq!(
            s.used_by,
            vec!["step_a".to_string(), "step_b".to_string()],
            "used_by merges + sorts both nodes"
        );
    }

    #[test]
    fn pool_alias_also_read_as_data_keeps_pool_role_and_records_both_addresses() {
        // When the SAME alias is a pool binding AND a KnownResources entry, the
        // pool role wins (derived first) but the __resources splice key is ALSO
        // recorded so the launcher substitutes both the net id and the secret.
        let cap_id = Uuid::new_v4();
        let mut known = KnownResources::new();
        known.insert("prod_gpu".to_string(), capacity_resource(cap_id));

        let g = graph(vec![auto(
            "step1",
            DeploymentModel::Executor {
                capacity: Some(CapacityBinding {
                    alias: "prod_gpu".to_string(),
                    request: None,
                }),
                group: None,
            },
        )]);

        let m = derive_requirements(&g, &known, &known).expect("derive");
        let s = slot(&m, "prod_gpu");
        assert_eq!(s.role, SlotRole::ExecutorCapacity, "pool role wins");
        let addr = m.air_addresses.get("prod_gpu").unwrap();
        assert_eq!(addr.net_ids, vec![pool_net_id(cap_id)]);
        assert_eq!(addr.resource_keys, vec!["prod_gpu".to_string()]);
    }
}
