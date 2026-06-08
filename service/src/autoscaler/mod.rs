//! Model-pool autoscaler — the placement control loop.
//!
//! A mekhan background loop (cloning the presence-sweep spawn shape,
//! `runners_presence::start_presence_sweep`) that, each tick, decides WHICH
//! models are loaded and HOW they are spread across the ALREADY-REGISTERED LLM
//! runners. It does NOT provision compute: there is no Nomad node-pool scaler and
//! no per-model Nomad job — autoscaling is pure placement onto enrolled runners
//! via NATS load/unload ([`crate::runner_commands`]).
//!
//! Per tick the [`placement`] pass walks, for every model with an autoscale
//! policy folded onto its `model_states` row, a cheapest-first cascade against the
//! live engine inventory ([`crate::handlers::model_pool::serving_runner_inventory`]):
//! adapter-load → wake → (idle-evict on zero demand) → terminal "no eligible
//! runner". Residency is sourced from what each runner advertises in its interface
//! catalog (`RunnerInterfaceCatalog.residency_zone`) and enforced fail-closed.
//!
//! **L1 (manual)** constructs the loop with `demand = None`: only `manual`-mode
//! policies place by their desired count; `scale_to_zero`/`keep_warm` are
//! HARD-BLOCKED on the Router `/metrics` (L2 — see [`demand`]). Fail-soft
//! per-policy: one bad policy never kills the loop. Inference NEVER touches the
//! engine net or the presence net.

pub mod demand;
pub mod placement;

use std::time::Duration;

use sqlx::PgPool;

use crate::nats::MekhanNats;
use crate::runners_presence::RunnerPresence;

use self::demand::DemandSource;

/// Reconcile cadence. Short enough that a manual scale POST takes effect quickly;
/// long enough that a stuck read doesn't hammer the control plane.
const RECONCILE_INTERVAL_SECS: u64 = 15;

/// Spawn the autoscaler control loop. Called from `main.rs` after the presence +
/// worker-liveness controllers. `demand` is `None` for L1 (manual mode only);
/// L2 passes a [`DemandSource`] scraping the Router `/metrics`.
pub fn spawn_autoscaler(
    db: PgPool,
    nats: MekhanNats,
    runner_presence: RunnerPresence,
    demand: Option<std::sync::Arc<dyn DemandSource>>,
) {
    tokio::spawn(run_autoscaler(db, nats, runner_presence, demand));
}

async fn run_autoscaler(
    db: PgPool,
    nats: MekhanNats,
    runner_presence: RunnerPresence,
    demand: Option<std::sync::Arc<dyn DemandSource>>,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(RECONCILE_INTERVAL_SECS));
    tracing::info!(
        interval_secs = RECONCILE_INTERVAL_SECS,
        mode = if demand.is_some() {
            "reactive(L2)"
        } else {
            "manual(L1)"
        },
        "model autoscaler started"
    );

    loop {
        tick.tick().await;
        if let Err(e) = reconcile_once(&db, &nats, &runner_presence, demand.as_deref()).await {
            tracing::warn!("autoscaler reconcile tick failed: {e}");
        }
    }
}

/// One reconcile pass: the model placement controller walks the cheapest-first
/// cascade per `model_states` policy with demand. A per-policy failure fails-soft
/// (recorded on the row + carry on); only a setup `sqlx::Error` kills the tick.
async fn reconcile_once(
    db: &PgPool,
    nats: &MekhanNats,
    runner_presence: &RunnerPresence,
    demand: Option<&dyn DemandSource>,
) -> Result<(), sqlx::Error> {
    placement::reconcile_placement(db, nats, runner_presence, demand).await
}

/// A `model_states` row's folded-in autoscale-policy columns. The autoscale policy
/// stopped being a resource; it lives on the model SET. This is the per-row
/// projection the placement pass reads, converted to the in-memory
/// [`ModelAutoscalePolicy`](aithericon_resources::types::ModelAutoscalePolicy) DTO
/// via [`ModelStatePolicyRow::into_policy`]. The `WHERE autoscale_mode IS NOT NULL`
/// load filter guarantees `mode` is `Some`.
#[derive(sqlx::FromRow)]
pub(crate) struct ModelStatePolicyRow {
    pub model_id: String,
    pub base: Option<String>,
    pub autoscale_mode: Option<String>,
    pub desired_replicas: Option<i32>,
    pub scale_up_threshold: Option<f64>,
    pub scale_down_threshold: Option<f64>,
    pub cooldown_secs: Option<i64>,
    pub residency_zone: Option<String>,
    pub idle_evict: bool,
}

impl ModelStatePolicyRow {
    /// Build the in-memory [`ModelAutoscalePolicy`](aithericon_resources::types::ModelAutoscalePolicy)
    /// DTO from the row. `mode` is guaranteed `Some` by the load filter; defensively
    /// default it rather than panic.
    pub(crate) fn into_policy(self) -> aithericon_resources::types::ModelAutoscalePolicy {
        aithericon_resources::types::ModelAutoscalePolicy {
            model_id: self.model_id,
            residency_zone: self.residency_zone.unwrap_or_default(),
            mode: self.autoscale_mode.unwrap_or_default(),
            desired_replicas: self.desired_replicas.map(|v| v as u32),
            scale_up_threshold: self.scale_up_threshold,
            scale_down_threshold: self.scale_down_threshold,
            cooldown_secs: self.cooldown_secs.map(|v| v as u64),
            base: self.base,
            idle_evict: Some(self.idle_evict),
        }
    }
}
