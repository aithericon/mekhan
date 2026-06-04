//! Result schema (v1) and summary statistics.
//!
//! [`Stats::from_millis`] computes nearest-rank percentiles over a sample of
//! per-iteration wall-clock millisecond timings. The remaining structs mirror
//! the JSON RESULTS SCHEMA v1 one-record-per-point shape emitted by
//! [`crate::report::emit`].

use serde::Serialize;

/// Summary statistics over a sample of timings (milliseconds).
#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub mean: f64,
    pub n: usize,
}

impl Stats {
    /// Compute percentile/mean stats over `samples` (in milliseconds).
    ///
    /// Uses the nearest-rank method on a sorted copy. An empty sample yields
    /// all-zero stats with `n == 0`.
    pub fn from_millis(samples: &[f64]) -> Stats {
        if samples.is_empty() {
            return Stats {
                p50: 0.0,
                p95: 0.0,
                p99: 0.0,
                mean: 0.0,
                n: 0,
            };
        }

        let mut sorted = samples.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let n = sorted.len();
        let mean = sorted.iter().sum::<f64>() / n as f64;

        Stats {
            p50: nearest_rank(&sorted, 50.0),
            p95: nearest_rank(&sorted, 95.0),
            p99: nearest_rank(&sorted, 99.0),
            mean,
            n,
        }
    }
}

/// Nearest-rank percentile on an already-sorted, non-empty slice.
///
/// rank = ceil(p/100 * n), clamped to [1, n], then 1-indexed into the slice.
fn nearest_rank(sorted: &[f64], percentile: f64) -> f64 {
    let n = sorted.len();
    debug_assert!(n > 0, "nearest_rank requires a non-empty slice");
    let rank = ((percentile / 100.0) * n as f64).ceil() as usize;
    let idx = rank.clamp(1, n) - 1;
    sorted[idx]
}

/// The `metrics` block of a result record.
#[derive(Debug, Clone, Serialize)]
pub struct Metrics {
    pub wall_ms: Stats,
    pub events_per_sec: Option<f64>,
    pub rss_mb: Option<f64>,
}

/// The `run` block — environment provenance shared across a sweep.
#[derive(Debug, Clone, Serialize)]
pub struct RunMeta {
    pub git_sha: String,
    pub timestamp_ms: u64,
    pub host: String,
    pub profile: String,
}

/// One measured benchmark point, serialized as a single JSON artifact.
#[derive(Debug, Clone, Serialize)]
pub struct ResultRecord {
    pub schema_version: u32,
    pub run: RunMeta,
    pub layer: String,
    pub axis: String,
    pub scenario: String,
    pub params: serde_json::Value,
    pub metrics: Metrics,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_millis_empty_is_zeros() {
        let s = Stats::from_millis(&[]);
        assert_eq!(s.n, 0);
        assert_eq!(s.p50, 0.0);
        assert_eq!(s.p95, 0.0);
        assert_eq!(s.p99, 0.0);
        assert_eq!(s.mean, 0.0);
    }

    #[test]
    fn from_millis_percentiles_nearest_rank() {
        // 1..=100; nearest-rank: p50 -> rank 50 -> value 50, p95 -> 95, p99 -> 99.
        let samples: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        let s = Stats::from_millis(&samples);
        assert_eq!(s.n, 100);
        assert_eq!(s.p50, 50.0);
        assert_eq!(s.p95, 95.0);
        assert_eq!(s.p99, 99.0);
        assert_eq!(s.mean, 50.5);
    }

    #[test]
    fn from_millis_single_sample() {
        let s = Stats::from_millis(&[7.0]);
        assert_eq!(s.n, 1);
        assert_eq!(s.p50, 7.0);
        assert_eq!(s.p95, 7.0);
        assert_eq!(s.p99, 7.0);
        assert_eq!(s.mean, 7.0);
    }

    #[test]
    fn from_millis_unsorted_input() {
        let s = Stats::from_millis(&[30.0, 10.0, 20.0]);
        // sorted: [10,20,30]; p50 -> rank ceil(1.5)=2 -> 20.
        assert_eq!(s.p50, 20.0);
        assert_eq!(s.p99, 30.0);
    }
}
