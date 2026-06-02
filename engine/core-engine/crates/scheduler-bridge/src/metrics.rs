//! Allocation accounting telemetry flattened into terminal scheduler signals.
//!
//! [`AllocationMetrics`] is the shared, payload-only enrichment both the Slurm
//! and Nomad watchers serialize into the [`ExternalSignal`](petri_domain::ExternalSignal)
//! `payload` JSON they already publish on a terminal job status. It lives here
//! (in `petri-scheduler-bridge`) because that is the one engine crate both
//! `petri-slurm` and `petri-nomad` already depend on.
//!
//! ## Contract
//!
//! - **Payload-only.** `ExternalSignal` itself is unchanged — `payload` is opaque
//!   JSON. No new `DomainEvent` variant. `dedup_id` semantics (`<source>:<job>:<status>`)
//!   are untouched.
//! - **Everything optional.** Every field is `Option` and skipped when `None`
//!   (`skip_serializing_if`), so an unavailable datum simply omits its key and
//!   the payload stays small (well under NATS limits). The struct flattens into
//!   the signal payload via `#[serde(flatten)]` at the publish site, so its keys
//!   sit alongside the existing `source`/`scheduler_job_id`/`job_status` fields.
//! - **Source-symmetric.** Slurm fills most fields from the `sacct` row already
//!   fetched; Nomad fills `node`/`exit_code` at the post-dispatch poll and
//!   timing/usage from the terminal allocation/task state.

use serde::{Deserialize, Serialize};

/// Allocation accounting metrics carried in a terminal scheduler signal payload.
///
/// All fields are optional — a watcher omits anything the scheduler did not
/// report. Flattened into the `ExternalSignal.payload` JSON object at publish.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AllocationMetrics {
    /// Numeric process exit code (Slurm `ExitCode` `exit:signal` → `exit`;
    /// Nomad task `ExitCode`). `None` when unknown / still running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,

    /// Placement host (Slurm `NodeList` from scontrol/sacct; Nomad alloc
    /// `NodeName`). `None` while unplaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,

    /// Queue wait in milliseconds (`start_time - submit_time`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_wait_ms: Option<i64>,

    /// Wall-clock run time in milliseconds (`end_time - start_time`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<i64>,

    /// CPU-seconds consumed (Slurm `TotalCPU`; Nomad cpu task-seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_seconds: Option<f64>,

    /// GPU-seconds consumed (Slurm gres/gpu count × elapsed; Nomad device
    /// seconds). May be `None` when the scheduler does not report GPU usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_seconds: Option<f64>,

    /// Peak resident set size in bytes (Slurm `MaxRSS`; Nomad `memory_max`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_rss_bytes: Option<i64>,

    /// Requested trackable resources (from the job's `ReqTRES` / job spec).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_tres: Option<RequestedTres>,

    /// Allocated trackable resources (from the job's `AllocTRES` / alloc spec).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocated_tres: Option<AllocatedTres>,
}

impl AllocationMetrics {
    /// Whether every field is unset — i.e. nothing to add to the payload.
    pub fn is_empty(&self) -> bool {
        *self == AllocationMetrics::default()
    }
}

/// Requested trackable resources subset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestedTres {
    /// Requested CPU count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_count: Option<i64>,
    /// Requested GPU count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_count: Option<i64>,
    /// Requested GPU type/model (e.g. `a100`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_type: Option<String>,
    /// Requested memory in gibibytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_gb: Option<f64>,
}

impl RequestedTres {
    /// Whether every field is unset.
    pub fn is_empty(&self) -> bool {
        *self == RequestedTres::default()
    }

    /// `Some(self)` when at least one field is set, else `None` — for collapsing
    /// an all-empty TRES block to an omitted payload key.
    pub fn non_empty(self) -> Option<Self> {
        if self.is_empty() {
            None
        } else {
            Some(self)
        }
    }
}

/// Allocated trackable resources subset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AllocatedTres {
    /// Allocated CPU count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_count: Option<i64>,
    /// Allocated GPU count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_count: Option<i64>,
    /// Allocated memory in gibibytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_gb: Option<f64>,
}

impl AllocatedTres {
    /// Whether every field is unset.
    pub fn is_empty(&self) -> bool {
        *self == AllocatedTres::default()
    }

    /// `Some(self)` when at least one field is set, else `None`.
    pub fn non_empty(self) -> Option<Self> {
        if self.is_empty() {
            None
        } else {
            Some(self)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_metrics_serializes_to_empty_object() {
        let m = AllocationMetrics::default();
        assert!(m.is_empty());
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v, serde_json::json!({}));
    }

    #[test]
    fn populated_metrics_omit_none_fields() {
        let m = AllocationMetrics {
            exit_code: Some(0),
            node: Some("node01".into()),
            elapsed_ms: Some(1500),
            cpu_seconds: Some(12.5),
            requested_tres: RequestedTres {
                cpu_count: Some(4),
                gpu_count: Some(1),
                gpu_type: Some("a100".into()),
                memory_gb: Some(16.0),
            }
            .non_empty(),
            ..Default::default()
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["exit_code"], 0);
        assert_eq!(v["node"], "node01");
        assert_eq!(v["elapsed_ms"], 1500);
        assert_eq!(v["cpu_seconds"], 12.5);
        assert_eq!(v["requested_tres"]["gpu_type"], "a100");
        // None fields are absent.
        assert!(v.get("gpu_seconds").is_none());
        assert!(v.get("peak_rss_bytes").is_none());
        assert!(v.get("queue_wait_ms").is_none());
        assert!(v.get("allocated_tres").is_none());
    }

    #[test]
    fn flattens_alongside_existing_payload_keys() {
        // Mirrors the publish site: base signal keys + flattened metrics.
        let m = AllocationMetrics {
            exit_code: Some(1),
            node: Some("n2".into()),
            ..Default::default()
        };
        #[derive(Serialize)]
        struct Payload<'a> {
            source: &'a str,
            scheduler_job_id: &'a str,
            #[serde(flatten)]
            metrics: AllocationMetrics,
        }
        let v = serde_json::to_value(Payload {
            source: "slurm",
            scheduler_job_id: "12345",
            metrics: m,
        })
        .unwrap();
        assert_eq!(v["source"], "slurm");
        assert_eq!(v["scheduler_job_id"], "12345");
        assert_eq!(v["exit_code"], 1);
        assert_eq!(v["node"], "n2");
    }

    #[test]
    fn tres_non_empty_collapses_default() {
        assert!(RequestedTres::default().non_empty().is_none());
        assert!(AllocatedTres::default().non_empty().is_none());
        assert!(RequestedTres {
            cpu_count: Some(1),
            ..Default::default()
        }
        .non_empty()
        .is_some());
    }
}
