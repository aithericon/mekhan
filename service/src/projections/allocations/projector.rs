//! Pure fold from the engine event log into per-grant `AllocationRow`s.
//!
//! Used by both the consumer (online ingest) and tests (offline replay).
//! Mirrors `step_executions::projector` — pure `(events, net_id) → Vec<Row>`,
//! safe to re-fold the whole per-net buffer on every new event.
//!
//! ## What drives each row
//!
//! Rows are keyed by `grant_id` (the engine grant key `instance_id:node_id`,
//! which is also the accounting `signal_key`). For ONE pool-adapter net
//! `pool-<resource_id>` we fold:
//!
//! - **`datacenter_lease`** (PRIMARY):
//!   - `EffectCompleted { effect_handler_id: "resource_lease_acquire", … }`
//!     → open/refresh the row at `status = "held"`, capturing `alloc_id`,
//!     `node`, `executor_namespace`, `expiry`, `scheduler_flavor`,
//!     `acquired_at`, and `grant_id` from `effect_result.lease`.
//!   - `EffectCompleted { effect_handler_id: "resource_lease_release", … }`
//!     → `status = "released"`, `released_at`. `grant_id` is recovered from
//!     the release `effect_result` is minimal (`{alloc_id, released}`), so we
//!     match on `alloc_id` against the open held rows.
//!   - the terminal **accounting signal** the per-cluster watcher publishes
//!     (lands in the net event log as a `TokenCreated` whose `signal_key ==
//!     grant_id`, with the enriched `AllocationMetrics` flattened into the
//!     token color) → fill `exit_code`, `queue_wait_ms`, `elapsed_ms`,
//!     `cpu_seconds`, `gpu_seconds`, `peak_rss_bytes`, `allocated_tres`,
//!     `requested_tres`, back-fill `node`, and move `status` to
//!     `failed`/`expired`/`released` per `job_status`.
//!
//! - **`concurrency_limit_grant`** (BEST-EFFORT): `TransitionFired` on the pool net's
//!   `t_grant` (→ `held`) / `t_release` (→ `released`), reading the produced
//!   `Grant`/`Release` token's `grant_id`. This is intentionally minimal — the
//!   `datacenter_lease` path is the load-bearing one.
//!
//! ## Fields NOT in the event log
//!
//! `cluster_resource_id` is recovered from the net id (`pool-<resource_id>`),
//! NOT from `effect_result` (the effect_config — which carries `resource_id` /
//! `scheduler_flavor` — is not journaled into `EffectCompleted`). `node_id` and
//! the owning workflow net are recovered by splitting `grant_id` on the first
//! `:` (`<workflow_net_id>:<node_id>`); the consumer resolves the workflow net
//! to the `instance_id` UUID. `requested_tres` is author-provided and consumed
//! (not read-arc'd), so it is NOT recoverable from `consumed_tokens` (which
//! carries token IDs only) — it is taken best-effort from the accounting
//! signal's `requested_tres` field.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use petri_domain::{DomainEvent, PersistedEvent, TokenColor};

/// Allocation kind discriminant — the `allocations.kind` text column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AllocationKind {
    DatacenterLease,
    ConcurrencyLimitGrant,
}

impl AllocationKind {
    pub fn wire_str(self) -> &'static str {
        match self {
            AllocationKind::DatacenterLease => "datacenter_lease",
            AllocationKind::ConcurrencyLimitGrant => "concurrency_limit_grant",
        }
    }
}

/// Allocation lifecycle — the `allocations.status` text column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AllocationStatus {
    Pending,
    Held,
    Released,
    Failed,
    Expired,
}

impl AllocationStatus {
    pub fn wire_str(self) -> &'static str {
        match self {
            AllocationStatus::Pending => "pending",
            AllocationStatus::Held => "held",
            AllocationStatus::Released => "released",
            AllocationStatus::Failed => "failed",
            AllocationStatus::Expired => "expired",
        }
    }
}

/// One projected allocation. Keyed by `(net_id, grant_id, kind)`. The consumer
/// resolves `instance_id` (UUID) from the workflow net prefix of `grant_id` and
/// upserts into the `allocations` table.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AllocationRow {
    pub kind: AllocationKind,
    pub net_id: String,
    /// Workflow node / LeaseScope container id (the suffix of `grant_id`).
    pub node_id: Option<String>,
    /// Engine grant key `instance_id:node_id`; also the accounting signal_key.
    pub grant_id: String,
    /// Datacenter resource id, parsed from the `pool-<uuid>` net id.
    pub cluster_resource_id: Option<String>,
    pub scheduler_flavor: Option<String>,
    pub alloc_id: Option<String>,
    pub node: Option<String>,
    pub executor_namespace: Option<String>,
    pub status: AllocationStatus,
    pub requested_at: Option<DateTime<Utc>>,
    pub acquired_at: Option<DateTime<Utc>>,
    pub released_at: Option<DateTime<Utc>>,
    pub expiry: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub queue_wait_ms: Option<i64>,
    pub elapsed_ms: Option<i64>,
    /// Rounded whole seconds (payload float → round → i64).
    pub cpu_seconds: Option<i64>,
    pub gpu_seconds: Option<i64>,
    pub peak_rss_bytes: Option<i64>,
    pub requested_tres: Option<serde_json::Value>,
    pub allocated_tres: Option<serde_json::Value>,
    pub last_error: Option<String>,
    /// Engine event sequence of the last event folded into this row.
    pub last_sequence: u64,
}

impl AllocationRow {
    fn new(kind: AllocationKind, net_id: &str, grant_id: &str) -> Self {
        let node_id = grant_id.split_once(':').map(|(_, n)| n.to_string());
        let cluster_resource_id = net_id
            .strip_prefix("pool-")
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        Self {
            kind,
            net_id: net_id.to_string(),
            node_id,
            grant_id: grant_id.to_string(),
            cluster_resource_id,
            scheduler_flavor: None,
            alloc_id: None,
            node: None,
            executor_namespace: None,
            status: AllocationStatus::Pending,
            requested_at: None,
            acquired_at: None,
            released_at: None,
            expiry: None,
            exit_code: None,
            queue_wait_ms: None,
            elapsed_ms: None,
            cpu_seconds: None,
            gpu_seconds: None,
            peak_rss_bytes: None,
            requested_tres: None,
            allocated_tres: None,
            last_error: None,
            last_sequence: 0,
        }
    }
}

/// Project a net's event stream into per-grant allocation rows.
///
/// `net_id` is the net the events belong to (`pool-<resource_id>` for
/// datacenter leases / token pools). Pure: identical input → identical output.
pub fn project_allocations(events: &[PersistedEvent], net_id: &str) -> Vec<AllocationRow> {
    let mut state = State::default();
    for ev in events {
        state.absorb(ev, net_id);
    }
    state.into_rows()
}

#[derive(Default)]
struct State {
    /// `grant_id → row`. One row per grant on this net.
    rows: BTreeMap<String, AllocationRow>,
}

impl State {
    fn absorb(&mut self, persisted: &PersistedEvent, net_id: &str) {
        match &persisted.event {
            DomainEvent::EffectCompleted {
                effect_handler_id,
                effect_result,
                produced_tokens,
                ..
            } => match effect_handler_id.as_str() {
                "resource_lease_acquire" => {
                    self.fold_lease_acquire(
                        effect_result,
                        net_id,
                        persisted.sequence,
                        persisted.timestamp,
                    );
                }
                "resource_lease_release" => {
                    self.fold_lease_release(
                        effect_result,
                        produced_tokens,
                        persisted.sequence,
                        persisted.timestamp,
                    );
                }
                _ => {}
            },
            // Accounting signal: injected as a TokenCreated at a signal place
            // with `signal_key == grant_id`; the token color carries the
            // enriched `AllocationMetrics` payload (all fields OPTIONAL).
            DomainEvent::TokenCreated {
                token,
                signal_key: Some(signal_key),
                ..
            } => {
                self.fold_accounting_signal(
                    signal_key,
                    &token.color,
                    persisted.sequence,
                    persisted.timestamp,
                );
            }
            // BEST-EFFORT concurrency_limit_grant: the pool net's grant/release fires.
            DomainEvent::TransitionFired {
                transition_id,
                produced_tokens,
                ..
            } => {
                self.fold_pool_fire(
                    &transition_id.0,
                    produced_tokens,
                    net_id,
                    persisted.sequence,
                    persisted.timestamp,
                );
            }
            _ => {}
        }
    }

    fn fold_lease_acquire(
        &mut self,
        effect_result: &serde_json::Value,
        net_id: &str,
        sequence: u64,
        ts: DateTime<Utc>,
    ) {
        // Acquire result: { "alloc_id": str, "lease": { grant_id, alloc_id,
        //   node?, expiry?, executor_namespace?, scheduler: {…} } }.
        let lease = effect_result.get("lease").unwrap_or(effect_result);
        let Some(grant_id) = lease.get("grant_id").and_then(|v| v.as_str()) else {
            return;
        };
        let row = self
            .rows
            .entry(grant_id.to_string())
            .or_insert_with(|| AllocationRow::new(AllocationKind::DatacenterLease, net_id, grant_id));

        if let Some(alloc_id) = lease
            .get("alloc_id")
            .and_then(|v| v.as_str())
            .or_else(|| effect_result.get("alloc_id").and_then(|v| v.as_str()))
        {
            row.alloc_id = Some(alloc_id.to_string());
        }
        if let Some(node) = lease.get("node").and_then(|v| v.as_str()) {
            row.node = Some(node.to_string());
        }
        if let Some(ns) = lease.get("executor_namespace").and_then(|v| v.as_str()) {
            row.executor_namespace = Some(ns.to_string());
        }
        if let Some(exp) = lease.get("expiry").and_then(|v| v.as_str()) {
            row.expiry = parse_ts(exp);
        }
        if row.scheduler_flavor.is_none() {
            row.scheduler_flavor = scheduler_flavor_from_lease(lease);
        }
        // First acquire wins for acquired_at (replay re-emits the same fire).
        if row.acquired_at.is_none() {
            row.acquired_at = Some(ts);
        }
        // Only advance to held from a pre-held state; never regress a terminal.
        if matches!(row.status, AllocationStatus::Pending) {
            row.status = AllocationStatus::Held;
        }
        row.last_sequence = sequence;
    }

    fn fold_lease_release(
        &mut self,
        effect_result: &serde_json::Value,
        produced_tokens: &[(petri_domain::PlaceId, petri_domain::Token)],
        sequence: u64,
        ts: DateTime<Utc>,
    ) {
        // Release result is minimal: { "alloc_id": str, "released": true }.
        // grant_id is NOT in the result — recover it from the produced
        // "released" token ({ grant_id }), else correlate by alloc_id.
        let grant_id = produced_tokens
            .iter()
            .find_map(|(_, t)| match &t.color {
                TokenColor::Data(v) => v.get("grant_id").and_then(|g| g.as_str()).map(String::from),
                _ => None,
            })
            .or_else(|| {
                let alloc_id = effect_result.get("alloc_id").and_then(|v| v.as_str())?;
                self.rows
                    .values()
                    .find(|r| r.alloc_id.as_deref() == Some(alloc_id))
                    .map(|r| r.grant_id.clone())
            });
        let Some(grant_id) = grant_id else {
            return;
        };
        let row = self.rows.entry(grant_id.clone()).or_insert_with(|| {
            AllocationRow::new(AllocationKind::DatacenterLease, "", &grant_id)
        });
        row.released_at = Some(ts);
        // A clean release supersedes held; do not clobber a terminal failure.
        if matches!(row.status, AllocationStatus::Pending | AllocationStatus::Held) {
            row.status = AllocationStatus::Released;
        }
        row.last_sequence = sequence;
    }

    fn fold_accounting_signal(
        &mut self,
        signal_key: &str,
        color: &TokenColor,
        sequence: u64,
        ts: DateTime<Utc>,
    ) {
        let TokenColor::Data(payload) = color else {
            return;
        };
        // Only enrich rows we already know about (signal_key == grant_id). A
        // signal for an unknown key on this net is some other net's concern.
        let Some(row) = self.rows.get_mut(signal_key) else {
            return;
        };

        if let Some(v) = payload.get("exit_code").and_then(json_to_i64) {
            row.exit_code = Some(v as i32);
        }
        if let Some(v) = payload.get("queue_wait_ms").and_then(json_to_i64) {
            row.queue_wait_ms = Some(v);
        }
        if let Some(v) = payload.get("elapsed_ms").and_then(json_to_i64) {
            row.elapsed_ms = Some(v);
        }
        if let Some(v) = payload.get("cpu_seconds").and_then(json_to_f64) {
            row.cpu_seconds = Some(v.round() as i64);
        }
        if let Some(v) = payload.get("gpu_seconds").and_then(json_to_f64) {
            row.gpu_seconds = Some(v.round() as i64);
        }
        if let Some(v) = payload.get("peak_rss_bytes").and_then(json_to_i64) {
            row.peak_rss_bytes = Some(v);
        }
        if let Some(v) = payload.get("requested_tres") {
            if !v.is_null() {
                row.requested_tres = Some(v.clone());
            }
        }
        if let Some(v) = payload.get("allocated_tres") {
            if !v.is_null() {
                row.allocated_tres = Some(v.clone());
            }
        }
        if row.node.is_none() {
            if let Some(node) = payload.get("node").and_then(|v| v.as_str()) {
                row.node = Some(node.to_string());
            }
        }
        if row.scheduler_flavor.is_none() {
            if let Some(src) = payload.get("source").and_then(|v| v.as_str()) {
                row.scheduler_flavor = Some(src.to_string());
            }
        }

        // Terminal status mapping. Only a terminal job_status advances status;
        // intermediate signals (running/queued, if ever tapped) leave it Held.
        if let Some(job_status) = payload.get("job_status").and_then(|v| v.as_str()) {
            let next = match job_status {
                "completed" => Some(AllocationStatus::Released),
                "failed" | "lost" => Some(AllocationStatus::Failed),
                "cancelled" => Some(AllocationStatus::Released),
                "timed_out" => Some(AllocationStatus::Expired),
                _ => None,
            };
            if let Some(next) = next {
                // Don't regress an explicit release back to a non-terminal, but
                // do let a terminal accounting signal sharpen status.
                row.status = next;
                if row.released_at.is_none() {
                    row.released_at = Some(ts);
                }
                if matches!(job_status, "failed" | "lost") {
                    let msg = payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or(job_status);
                    row.last_error = Some(msg.to_string());
                }
            }
        }
        row.last_sequence = sequence;
    }

    /// BEST-EFFORT concurrency_limit_grant: the pool net's `t_grant` produces a
    /// `Grant { grant_id, … }`; `t_release` returns capacity. Project a minimal
    /// row off the produced token's `grant_id`.
    ///
    /// TODO(concurrency_limit): the grant/release transition ids are the well-known
    /// `t_grant` / `t_release` on the pool net (see `petri/pool_net.rs`), but a
    /// pool net that is ALSO a datacenter lease adapter carries both surfaces.
    /// We discriminate by handler (lease effects → `datacenter_lease`) above and
    /// only fall here for plain `concurrency_limit` nets. If that proves too coarse,
    /// inspect the net's resource KIND (requires a DB/registry lookup the pure
    /// projector intentionally avoids) and gate there instead.
    fn fold_pool_fire(
        &mut self,
        transition_id: &str,
        produced_tokens: &[(petri_domain::PlaceId, petri_domain::Token)],
        net_id: &str,
        sequence: u64,
        ts: DateTime<Utc>,
    ) {
        let (is_grant, is_release) = (transition_id == "t_grant", transition_id == "t_release");
        if !is_grant && !is_release {
            return;
        }
        let grant_id = produced_tokens.iter().find_map(|(_, t)| match &t.color {
            TokenColor::Data(v) => v.get("grant_id").and_then(|g| g.as_str()).map(String::from),
            _ => None,
        });
        let Some(grant_id) = grant_id else {
            return;
        };
        // Don't shadow a datacenter_lease row already projected for this grant.
        if let Some(existing) = self.rows.get(&grant_id) {
            if existing.kind == AllocationKind::DatacenterLease {
                return;
            }
        }
        let row = self
            .rows
            .entry(grant_id.clone())
            .or_insert_with(|| AllocationRow::new(AllocationKind::ConcurrencyLimitGrant, net_id, &grant_id));
        if is_grant {
            if row.acquired_at.is_none() {
                row.acquired_at = Some(ts);
            }
            if matches!(row.status, AllocationStatus::Pending) {
                row.status = AllocationStatus::Held;
            }
        } else {
            row.released_at = Some(ts);
            if matches!(row.status, AllocationStatus::Pending | AllocationStatus::Held) {
                row.status = AllocationStatus::Released;
            }
        }
        row.last_sequence = sequence;
    }

    fn into_rows(self) -> Vec<AllocationRow> {
        self.rows.into_values().collect()
    }
}

/// Extract the scheduler flavor from a lease's typed `scheduler` tagged-union
/// (the `Lease__scheduler` `oneOf` discriminator) — the single key of the
/// `scheduler` object, or an explicit `scheduler_flavor` if present. Best-effort.
fn scheduler_flavor_from_lease(lease: &serde_json::Value) -> Option<String> {
    if let Some(f) = lease.get("scheduler_flavor").and_then(|v| v.as_str()) {
        return Some(f.to_string());
    }
    match lease.get("scheduler") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Object(map)) => {
            // Internally-tagged: a `type`/`flavor` field, else the sole key.
            if let Some(t) = map
                .get("type")
                .or_else(|| map.get("flavor"))
                .and_then(|v| v.as_str())
            {
                return Some(t.to_string());
            }
            if map.len() == 1 {
                return map.keys().next().cloned();
            }
            None
        }
        _ => None,
    }
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn json_to_i64(v: &serde_json::Value) -> Option<i64> {
    if v.is_null() {
        return None;
    }
    v.as_i64()
        .or_else(|| v.as_u64().map(|u| u as i64))
        .or_else(|| v.as_f64().map(|f| f.round() as i64))
}

fn json_to_f64(v: &serde_json::Value) -> Option<f64> {
    if v.is_null() {
        return None;
    }
    v.as_f64()
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use petri_domain::{PlaceId, Token, TokenColor, TransitionId};

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).single().expect("valid ts")
    }

    fn effect_completed(
        seq: u64,
        ts_secs: i64,
        handler: &str,
        effect_result: serde_json::Value,
        produced: Vec<(PlaceId, Token)>,
    ) -> PersistedEvent {
        PersistedEvent {
            sequence: seq,
            timestamp: ts(ts_secs),
            event: DomainEvent::EffectCompleted {
                transition_id: TransitionId("t_x".into()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: produced,
                effect_handler_id: handler.to_string(),
                effect_result,
                read_tokens: vec![],
                process_step_started: None,
                process_step_completed: None,
            },
            hash: String::new(),
            previous_hash: None,
        }
    }

    fn token_created_signal(
        seq: u64,
        ts_secs: i64,
        signal_key: &str,
        payload: serde_json::Value,
    ) -> PersistedEvent {
        PersistedEvent {
            sequence: seq,
            timestamp: ts(ts_secs),
            event: DomainEvent::TokenCreated {
                token: Token::new(TokenColor::Data(payload)),
                place_id: PlaceId("p_sig".into()),
                place_name: None,
                workflow_id: None,
                signal_key: Some(signal_key.to_string()),
                dedup_id: None,
            },
            hash: String::new(),
            previous_hash: None,
        }
    }

    const NET: &str = "pool-11111111-1111-1111-1111-111111111111";
    const GRANT: &str = "mekhan-abc:lease1";

    #[test]
    fn acquire_opens_held_row_with_lease_fields() {
        let lease = serde_json::json!({
            "grant_id": GRANT,
            "alloc_id": "job-42",
            "node": "gpu-node-3",
            "executor_namespace": "lease-mekhan-abc_lease1",
            "expiry": "2026-06-02T12:00:00Z",
            "scheduler": { "slurm": { "partition": "gpu" } },
        });
        let result = serde_json::json!({ "alloc_id": "job-42", "lease": lease });
        let events = vec![effect_completed(1, 100, "resource_lease_acquire", result, vec![])];

        let rows = project_allocations(&events, NET);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.kind, AllocationKind::DatacenterLease);
        assert_eq!(r.grant_id, GRANT);
        assert_eq!(r.node_id.as_deref(), Some("lease1"));
        assert_eq!(
            r.cluster_resource_id.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(r.alloc_id.as_deref(), Some("job-42"));
        assert_eq!(r.node.as_deref(), Some("gpu-node-3"));
        assert_eq!(
            r.executor_namespace.as_deref(),
            Some("lease-mekhan-abc_lease1")
        );
        assert_eq!(r.scheduler_flavor.as_deref(), Some("slurm"));
        assert_eq!(r.status, AllocationStatus::Held);
        assert_eq!(r.acquired_at, Some(ts(100)));
        assert!(r.expiry.is_some());
        assert_eq!(r.last_sequence, 1);
    }

    #[test]
    fn release_moves_row_to_released_via_produced_grant_id() {
        let lease = serde_json::json!({ "grant_id": GRANT, "alloc_id": "job-42" });
        let acquire = effect_completed(
            1,
            100,
            "resource_lease_acquire",
            serde_json::json!({ "alloc_id": "job-42", "lease": lease }),
            vec![],
        );
        let release = effect_completed(
            2,
            200,
            "resource_lease_release",
            serde_json::json!({ "alloc_id": "job-42", "released": true }),
            vec![(
                PlaceId("p_released".into()),
                Token::new(TokenColor::Data(serde_json::json!({ "grant_id": GRANT }))),
            )],
        );
        let rows = project_allocations(&[acquire, release], NET);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, AllocationStatus::Released);
        assert_eq!(rows[0].released_at, Some(ts(200)));
    }

    #[test]
    fn release_correlates_by_alloc_id_when_no_grant_in_token() {
        let lease = serde_json::json!({ "grant_id": GRANT, "alloc_id": "job-42" });
        let acquire = effect_completed(
            1,
            100,
            "resource_lease_acquire",
            serde_json::json!({ "alloc_id": "job-42", "lease": lease }),
            vec![],
        );
        let release = effect_completed(
            2,
            200,
            "resource_lease_release",
            serde_json::json!({ "alloc_id": "job-42", "released": true }),
            vec![], // no produced token carrying grant_id
        );
        let rows = project_allocations(&[acquire, release], NET);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, AllocationStatus::Released);
    }

    #[test]
    fn accounting_signal_enriches_and_terminalizes() {
        let lease = serde_json::json!({ "grant_id": GRANT, "alloc_id": "job-42" });
        let acquire = effect_completed(
            1,
            100,
            "resource_lease_acquire",
            serde_json::json!({ "alloc_id": "job-42", "lease": lease }),
            vec![],
        );
        let signal = token_created_signal(
            2,
            300,
            GRANT,
            serde_json::json!({
                "source": "slurm",
                "scheduler_job_id": GRANT,
                "job_status": "completed",
                "exit_code": 0,
                "node": "gpu-node-3",
                "queue_wait_ms": 1500,
                "elapsed_ms": 60000,
                "cpu_seconds": 119.6,
                "gpu_seconds": 60.0,
                "peak_rss_bytes": 2048,
                "requested_tres": { "gpu_count": 1, "cpu_count": 4 },
                "allocated_tres": { "cpu_count": 4, "memory_gb": 16.0 },
            }),
        );
        let rows = project_allocations(&[acquire, signal], NET);
        let r = &rows[0];
        assert_eq!(r.status, AllocationStatus::Released);
        assert_eq!(r.exit_code, Some(0));
        assert_eq!(r.queue_wait_ms, Some(1500));
        assert_eq!(r.elapsed_ms, Some(60000));
        assert_eq!(r.cpu_seconds, Some(120)); // 119.6 rounded
        assert_eq!(r.gpu_seconds, Some(60));
        assert_eq!(r.peak_rss_bytes, Some(2048));
        assert!(r.requested_tres.is_some());
        assert!(r.allocated_tres.is_some());
        assert_eq!(r.scheduler_flavor.as_deref(), Some("slurm"));
    }

    #[test]
    fn failed_job_status_marks_failed_with_error() {
        let lease = serde_json::json!({ "grant_id": GRANT, "alloc_id": "job-42" });
        let acquire = effect_completed(
            1,
            100,
            "resource_lease_acquire",
            serde_json::json!({ "alloc_id": "job-42", "lease": lease }),
            vec![],
        );
        let signal = token_created_signal(
            2,
            300,
            GRANT,
            serde_json::json!({
                "source": "nomad",
                "job_status": "failed",
                "exit_code": 137,
                "error": "OOM killed",
            }),
        );
        let rows = project_allocations(&[acquire, signal], NET);
        assert_eq!(rows[0].status, AllocationStatus::Failed);
        assert_eq!(rows[0].exit_code, Some(137));
        assert_eq!(rows[0].last_error.as_deref(), Some("OOM killed"));
    }

    #[test]
    fn timed_out_maps_to_expired() {
        let lease = serde_json::json!({ "grant_id": GRANT, "alloc_id": "job-42" });
        let acquire = effect_completed(
            1,
            100,
            "resource_lease_acquire",
            serde_json::json!({ "alloc_id": "job-42", "lease": lease }),
            vec![],
        );
        let signal = token_created_signal(
            2,
            300,
            GRANT,
            serde_json::json!({ "source": "slurm", "job_status": "timed_out" }),
        );
        let rows = project_allocations(&[acquire, signal], NET);
        assert_eq!(rows[0].status, AllocationStatus::Expired);
    }

    #[test]
    fn unrelated_signal_key_does_not_open_a_row() {
        let signal = token_created_signal(
            1,
            100,
            "some-other-grant",
            serde_json::json!({ "job_status": "completed" }),
        );
        let rows = project_allocations(&[signal], NET);
        assert!(rows.is_empty());
    }

    #[test]
    fn concurrency_limit_grant_best_effort() {
        let net = "pool-22222222-2222-2222-2222-222222222222";
        let grant = "mekhan-xyz:pooled1";
        let fired = |seq, ts_secs, tid: &str| PersistedEvent {
            sequence: seq,
            timestamp: ts(ts_secs),
            event: DomainEvent::TransitionFired {
                transition_id: TransitionId(tid.to_string()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![(
                    PlaceId("p_grant".into()),
                    Token::new(TokenColor::Data(serde_json::json!({ "grant_id": grant }))),
                )],
                read_tokens: vec![],
                process_step_started: None,
                process_step_completed: None,
            },
            hash: String::new(),
            previous_hash: None,
        };
        let rows = project_allocations(&[fired(1, 100, "t_grant"), fired(2, 200, "t_release")], net);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, AllocationKind::ConcurrencyLimitGrant);
        assert_eq!(rows[0].status, AllocationStatus::Released);
        assert_eq!(rows[0].acquired_at, Some(ts(100)));
        assert_eq!(rows[0].released_at, Some(ts(200)));
    }

    #[test]
    fn replay_idempotent_acquire_keeps_first_acquired_at() {
        let lease = serde_json::json!({ "grant_id": GRANT, "alloc_id": "job-42" });
        let result = serde_json::json!({ "alloc_id": "job-42", "lease": lease });
        let a1 = effect_completed(1, 100, "resource_lease_acquire", result.clone(), vec![]);
        let a2 = effect_completed(1, 100, "resource_lease_acquire", result, vec![]);
        let rows = project_allocations(&[a1, a2], NET);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].acquired_at, Some(ts(100)));
    }
}
