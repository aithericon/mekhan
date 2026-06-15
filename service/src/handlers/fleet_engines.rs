//! Per-node engine-inventory read (docs/31 Phase 0, OQ-2).
//!
//! `GET /api/v1/fleet/engines` is the operator-visibility + placement-debugging
//! surface over the single authoritative engine-inventory read model
//! ([`crate::handlers::model_pool::serving_runner_inventory`]). It answers the
//! question the placement loop asks every tick — "which base is live on which
//! node, with how many free slots, serving which LoRA adapters" — WITHOUT a new
//! store: it is `runner_interfaces.catalog ∩ presence` already on hand, grouped
//! by base per node, with per-engine headroom layered on from the router
//! in-flight gauge.
//!
//! Shape: `node → [engine{ base, max_num_seqs (=C), loaded_adapters, headroom }]`.
//! Headroom per base engine = `max_num_seqs − Σ(base + its adapters in-flight)`,
//! the in-flight read from the router `/metrics`
//! `inference_router_model_inflight` gauge (the SAME signal `demand.rs` scrapes,
//! via the raw [`DemandSource::inflight_for`] accessor — NOT the starved delta).
//!
//! **Fail-soft, like the rest of the model-pool reads.** When the router poll is
//! unconfigured (`AUTOSCALER_DEMAND_URL` unset) or unreachable, headroom degrades
//! to the FULL budget (`= C`) rather than failing the read — the same posture as
//! `serving_runner_counts` returning an empty map on a catalog-scan error. The
//! `max_num_seqs`/headroom is BASE-only and SHARED across that base's LoRAs (C is
//! per-engine, not per-adapter); a LoRA reads its budget via its `base`
//! back-pointer, never carrying its own `max_num_seqs`.
//!
//! Workspace-scoped (caller-implicit), session/human authed — the same boundary
//! as the other `models`-tagged reads.

use std::collections::BTreeMap;

use axum::{extract::State, Json};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::autoscaler::demand::{DemandSource, PrometheusDemandSource};
use crate::handlers::model_pool::serving_runner_inventory;
use crate::models::error::ApiError;
use crate::models::runner::ModelInterfaceKind;
use crate::AppState;

/// One LoRA adapter loaded on a base engine (the adapter half of the base↔LoRA
/// graph, attached via each adapter's `base` back-pointer).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LoadedAdapter {
    /// The adapter's model id (the router routes on this).
    pub model_id: String,
    /// The adapter-weights URI the load command supplied (e.g. `hf://...`), when
    /// the runner reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_uri: Option<String>,
}

/// One base engine live on a node — a base model + its per-engine concurrency
/// budget (`C`), the LoRA adapters it currently serves, and its computed
/// headroom.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NodeEngine {
    /// The base model id this engine serves.
    pub base: String,
    /// Per-engine concurrency `C` (vLLM `--max-num-seqs`), SHARED across this
    /// base's LoRA adapters. `None` when the runner advertised the base without a
    /// configured budget (older agents / partial catalogs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_num_seqs: Option<u32>,
    /// LoRA adapters loaded on this base engine.
    pub loaded_adapters: Vec<LoadedAdapter>,
    /// Free slots = `max_num_seqs − Σ(base + adapters in-flight)`, floored at 0.
    /// `None` when `max_num_seqs` is unknown (no budget to subtract against).
    /// Fail-soft: when the router in-flight poll is unconfigured/unreachable,
    /// this is the FULL budget (`= max_num_seqs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headroom: Option<u32>,
}

/// One live node (runner) and the base engines it is serving.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NodeInventory {
    /// The runner (node) id.
    pub runner_id: Uuid,
    /// The base engines live on this node.
    pub engines: Vec<NodeEngine>,
    /// Models **provisioned to disk** on this node but NOT resident — loadable
    /// without a re-download (the `pulled` superset minus the resident base
    /// engines above). The runner-local "ready to load" browser; empty for a vLLM
    /// node (its base is fixed at launch, so provisioned == resident).
    #[serde(default)]
    pub pulled: Vec<String>,
}

/// `GET /api/v1/fleet/engines` response — the per-node engine inventory.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FleetEnginesResponse {
    /// Whether per-engine headroom was computed against the router in-flight
    /// gauge (`true`) or degraded to the full budget because the router poll is
    /// unconfigured/unreachable (`false`). Operator hint, not a hard error.
    pub headroom_from_router: bool,
    /// Live nodes with their base engines.
    pub nodes: Vec<NodeInventory>,
}

/// `GET /api/v1/fleet/engines` — per-node Base/adapter inventory + per-engine
/// `max_num_seqs` + headroom.
///
/// Reads [`serving_runner_inventory`] (the `presence ∩ catalog` join, retaining
/// the runner→entries mapping), groups each node's entries by base, reads the
/// `max_num_seqs` off Base entries, attaches LoRAs via the `base` back-pointer,
/// and layers headroom from the router in-flight gauge (fail-soft to full
/// budget). Workspace-scoped, session/human authed.
#[utoipa::path(
    get,
    path = "/api/v1/fleet/engines",
    responses(
        (status = 200, description = "Per-node engine inventory: base engines, per-engine C, loaded LoRA adapters, headroom", body = FleetEnginesResponse),
    ),
    tag = "models",
)]
pub async fn list_fleet_engines(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<FleetEnginesResponse>, ApiError> {
    let workspace_id = user.workspace_id.unwrap_or_else(Uuid::nil);

    let inventory = serving_runner_inventory(&state.db, &state.runner_presence, workspace_id).await;
    // Provisioned-to-disk superset per node (the "ready to load" set). Resident
    // bases are subtracted below so a node lists only what it could load without
    // a re-download.
    let mut pulled_by_node = crate::handlers::model_pool::serving_runner_pulled(
        &state.db,
        &state.runner_presence,
        workspace_id,
    )
    .await;

    // Router in-flight poll for headroom. Constructed per-request from the same
    // env knob the autoscaler uses (`AUTOSCALER_DEMAND_URL`); unset/empty ⇒ no
    // source ⇒ headroom degrades to the full budget (fail-soft). This is a
    // debug/visibility read, not a hot path, so a per-request source is fine.
    let demand: Option<PrometheusDemandSource> = std::env::var("AUTOSCALER_DEMAND_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .map(PrometheusDemandSource::new);

    let mut headroom_from_router = false;
    let mut nodes: Vec<NodeInventory> = Vec::with_capacity(inventory.len());

    for (runner_id, entries) in inventory {
        // Group this node's entries into base engines + their adapters. Keyed by
        // base model id; ordered (BTreeMap) so the read is deterministic.
        let mut engines: BTreeMap<String, NodeEngine> = BTreeMap::new();

        // First pass: register every Base entry so its `max_num_seqs` is on hand.
        for entry in &entries {
            if entry.kind == ModelInterfaceKind::Base {
                engines
                    .entry(entry.model_id.clone())
                    .or_insert_with(|| NodeEngine {
                        base: entry.model_id.clone(),
                        max_num_seqs: None,
                        loaded_adapters: Vec::new(),
                        headroom: None,
                    })
                    .max_num_seqs = entry.max_num_seqs;
            }
        }

        // Second pass: attach LoRAs via the `base` back-pointer. A LoRA whose base
        // is not (yet) advertised as a Base entry on this node still gets its own
        // engine slot keyed on the back-pointer so the adapter is not dropped.
        for entry in &entries {
            if entry.kind == ModelInterfaceKind::Lora {
                let Some(base) = entry.base.clone() else {
                    // A LoRA missing its base back-pointer is a hard invariant
                    // violation; skip it rather than panic on this read.
                    continue;
                };
                engines
                    .entry(base.clone())
                    .or_insert_with(|| NodeEngine {
                        base: base.clone(),
                        max_num_seqs: None,
                        loaded_adapters: Vec::new(),
                        headroom: None,
                    })
                    .loaded_adapters
                    .push(LoadedAdapter {
                        model_id: entry.model_id.clone(),
                        source_uri: entry.source_uri.clone(),
                    });
            }
        }

        // Headroom = C − Σ(base + adapters in-flight), per base engine.
        for engine in engines.values_mut() {
            let Some(c) = engine.max_num_seqs else {
                continue; // no budget → no headroom number
            };

            // Sum in-flight across the base AND every loaded adapter (they share
            // the one engine's slots). Fail-soft: an unconfigured/unreachable
            // router leaves `headroom = C` (full budget).
            let mut in_flight: f64 = 0.0;
            let mut polled_any = false;
            if let Some(src) = demand.as_ref() {
                if let Some(v) = src.inflight_for(&engine.base).await {
                    in_flight += v;
                    polled_any = true;
                }
                for adapter in &engine.loaded_adapters {
                    if let Some(v) = src.inflight_for(&adapter.model_id).await {
                        in_flight += v;
                        polled_any = true;
                    }
                }
            }
            if polled_any {
                headroom_from_router = true;
            }

            let used = in_flight.max(0.0).round() as u32;
            engine.headroom = Some(c.saturating_sub(used));
        }

        let engines: Vec<NodeEngine> = engines.into_values().collect();

        // "Ready to load" = provisioned-to-disk minus the resident bases already
        // shown as engines. Deduped + ordered for a stable read.
        let resident: std::collections::HashSet<&str> =
            engines.iter().map(|e| e.base.as_str()).collect();
        let mut pulled: Vec<String> = pulled_by_node
            .remove(&runner_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|m| !resident.contains(m.as_str()))
            .collect();
        pulled.sort();
        pulled.dedup();

        nodes.push(NodeInventory {
            runner_id,
            engines,
            pulled,
        });
    }

    // Deterministic node ordering for a stable operator read.
    nodes.sort_by_key(|n| n.runner_id);

    Ok(Json(FleetEnginesResponse {
        headroom_from_router,
        nodes,
    }))
}
