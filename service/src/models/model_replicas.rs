//! Model-pool P4 (docs/29 §6') — the replica-autoscaler row + DTOs + the PURE
//! decision math.
//!
//! The autoscaler control loop (`crate::autoscaler`) reconciles ONE
//! [`ModelReplicaRow`] per `model_policy` resource: each tick it reads the policy
//! config, computes a desired replica COUNT ([`compute_target`]) — gated by a
//! durable cooldown ([`in_cooldown`]) anchored on `last_actuated_at` — observes
//! the live count from the FLEET ROSTER (live runners advertising the model_id,
//! NOT the staging effect result), actuates via a generated `model-replica-<id>`
//! one-shot net, and upserts the row. The row is ALSO the Control-Plane read
//! source (`GET /api/v1/models/replicas`).
//!
//! The decision functions here are pure + table-driven-testable: no DB, no clock
//! beyond the `now` passed in. The loop supplies `now = Utc::now()` and the
//! manual override (the row's `desired_count`, written by the scale endpoint).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use aithericon_resources::types::ModelAutoscalePolicy;

/// Terminal + transient states of a replica row's reconciliation. Stored as TEXT
/// (DB CHECK enforces the set — see `20240146000000_model_replicas.sql`).
pub mod status {
    pub const PROVISIONING: &str = "provisioning";
    pub const ACTIVE: &str = "active";
    pub const SCALING: &str = "scaling";
    pub const DRAINING: &str = "draining";
    pub const STOPPED: &str = "stopped";
    pub const FAILED: &str = "failed";
}

/// One `model_replicas` row — the durable reconciliation target + Control-Plane
/// read. `desired_count`/`observed_count` are stored `INT`; the loop works in
/// `u32` and converts at the edges.
#[derive(Clone, Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct ModelReplicaRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    /// The `model_policy` resource this row reconciles (UNIQUE — one row/policy).
    pub policy_resource_id: Uuid,
    pub model_id: String,
    /// Resolved `datacenter` resource UUID (the policy carries an alias; the loop
    /// resolves it before the upsert).
    pub datacenter_resource_id: Uuid,
    /// Native job NAME registered on the cluster (Nomad service-job id). `None`
    /// until first actuation.
    pub replica_slug: Option<String>,
    /// Last desired COUNT the loop drove (or the scale endpoint's manual override).
    pub desired_count: i32,
    /// Live count from the fleet roster (runners advertising `model_id`). NOT the
    /// staging effect result — that only proves "registered", not "serving".
    pub observed_count: i32,
    /// One of `status::*`.
    pub status: String,
    /// HARD residency zone recorded for the Control-Plane read + audit.
    pub residency_zone: Option<String>,
    pub last_error: Option<String>,
    /// Anchors the durable cooldown gate (survives a mekhan restart).
    pub last_actuated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `POST /api/v1/models/replicas/{policy_id}/scale` body — the L1 manual desired
/// override. Writes `desired_count` on the row; the loop picks it up next tick
/// (in `manual` mode the row's `desired_count` is the live control, seeded from
/// the policy's `desired_replicas`).
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct ModelReplicaScaleRequest {
    pub desired_replicas: u32,
}

/// Whether the loop is inside the cooldown window and must NOT actuate this tick.
/// `(last_actuated_at, cooldown_secs)` both present + `cooldown > 0` ⇒ gated until
/// `last_actuated_at + cooldown_secs`. Anything else ⇒ never gated (first
/// actuation, or no cooldown configured). Durable: `now` and `last_actuated_at`
/// come from the row, so a restart doesn't reset the window.
pub fn in_cooldown(
    last_actuated_at: Option<DateTime<Utc>>,
    cooldown_secs: Option<u64>,
    now: DateTime<Utc>,
) -> bool {
    match (last_actuated_at, cooldown_secs) {
        (Some(t), Some(c)) if c > 0 => now < t + chrono::Duration::seconds(c as i64),
        _ => false,
    }
}

/// The PURE desired-COUNT decision, clamped to `[min_replicas, max_replicas]`.
///
/// - `manual` ⇒ the `manual_override` (the row's `desired_count`) if present,
///   else the policy's `desired_replicas`. `None` only when neither is set (no
///   decision yet — the loop no-ops).
/// - `scale_to_zero` ⇒ needs `demand`: `Some` demand `> 0` scales to ≥1 (clamped),
///   `== 0` scales to 0. `demand == None` (L1 — router not wired) ⇒ `None` (no
///   decision; this mode is HARD-BLOCKED on the router `/metrics`).
/// - `keep_warm` ⇒ floors at `min_replicas`; with `demand` it lifts toward
///   `ceil(demand)`. `demand == None` ⇒ `Some(min_replicas)` (keep the floor warm
///   even without a demand signal).
/// - unknown mode ⇒ `None` (no decision; the loop logs + skips).
///
/// `demand` is `None` for all of L1. The thresholds on the policy are read only
/// by the L2 reactive path (see [`crate::autoscaler::demand`]).
pub fn compute_target(
    policy: &ModelAutoscalePolicy,
    demand: Option<f64>,
    manual_override: Option<u32>,
) -> Option<u32> {
    let raw = match policy.mode.as_str() {
        "manual" => manual_override.or(policy.desired_replicas)?,
        "scale_to_zero" => match demand {
            Some(d) if d > 0.0 => 1,
            Some(_) => 0,
            None => return None,
        },
        "keep_warm" => match demand {
            Some(d) => d.ceil().max(0.0) as u32,
            None => policy.min_replicas,
        },
        _ => return None,
    };
    Some(raw.clamp(policy.min_replicas, policy.max_replicas))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).single().expect("valid ts")
    }

    fn policy(mode: &str, min: u32, max: u32, desired: Option<u32>) -> ModelAutoscalePolicy {
        ModelAutoscalePolicy {
            model_id: "qwen2.5-7b".to_string(),
            datacenter_resource_id: "dev-nomad".to_string(),
            residency_zone: "eu-west".to_string(),
            min_replicas: min,
            max_replicas: max,
            mode: mode.to_string(),
            desired_replicas: desired,
            scale_up_threshold: None,
            scale_down_threshold: None,
            cooldown_secs: None,
            replica_spec: json!({ "image": "vllm/vllm-openai:latest", "gpus": 1 }),
        }
    }

    #[test]
    fn manual_uses_override_then_policy_then_none() {
        let p = policy("manual", 0, 4, Some(2));
        // Row override wins over the policy seed.
        assert_eq!(compute_target(&p, None, Some(3)), Some(3));
        // No override → policy desired.
        assert_eq!(compute_target(&p, None, None), Some(2));
        // Neither set → no decision.
        let p0 = policy("manual", 0, 4, None);
        assert_eq!(compute_target(&p0, None, None), None);
    }

    #[test]
    fn manual_clamps_to_min_max() {
        let p = policy("manual", 1, 2, None);
        assert_eq!(compute_target(&p, None, Some(5)), Some(2)); // clamp high
        assert_eq!(compute_target(&p, None, Some(0)), Some(1)); // clamp low
    }

    #[test]
    fn scale_to_zero_needs_demand() {
        let p = policy("scale_to_zero", 0, 3, None);
        assert_eq!(compute_target(&p, Some(5.0), None), Some(1)); // demand>0 → up (clamped ≥? min=0 → 1)
        assert_eq!(compute_target(&p, Some(0.0), None), Some(0)); // demand==0 → 0
        assert_eq!(compute_target(&p, None, None), None); // L1: no demand → no decision
    }

    #[test]
    fn keep_warm_floors_at_min() {
        let p = policy("keep_warm", 2, 8, None);
        assert_eq!(compute_target(&p, None, None), Some(2)); // no demand → floor
        assert_eq!(compute_target(&p, Some(0.0), None), Some(2)); // demand 0 → still floor
        assert_eq!(compute_target(&p, Some(4.2), None), Some(5)); // ceil(4.2)=5
        assert_eq!(compute_target(&p, Some(100.0), None), Some(8)); // clamp high
    }

    #[test]
    fn unknown_mode_is_no_decision() {
        let p = policy("bananas", 0, 4, Some(1));
        assert_eq!(compute_target(&p, None, Some(2)), None);
    }

    #[test]
    fn cooldown_gates_within_window_only() {
        let now = ts(1_000);
        // No prior actuation → never gated.
        assert!(!in_cooldown(None, Some(60), now));
        // No cooldown configured → never gated.
        assert!(!in_cooldown(Some(ts(990)), None, now));
        assert!(!in_cooldown(Some(ts(990)), Some(0), now));
        // Within the window → gated.
        assert!(in_cooldown(Some(ts(990)), Some(60), now)); // 990+60=1050 > 1000
                                                            // Past the window → free.
        assert!(!in_cooldown(Some(ts(900)), Some(60), now)); // 900+60=960 < 1000
    }
}
