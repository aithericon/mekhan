//! Slurm job state → `JobStatus` translation.
//!
//! Maps Slurm state strings (from `squeue` and `sacct`) to the
//! unified `JobStatus` enum used by the engine.

use petri_domain::JobStatus;

/// Map a Slurm state string to a `JobStatus`.
///
/// Slurm states come from `squeue -o %T` (active jobs) or `sacct -o State`
/// (completed jobs). Some states include a reason suffix after `+` (e.g.,
/// `CANCELLED+` or `CANCELLED by 1000`) — we strip everything after the
/// first non-alpha character.
///
/// ## Mapping
///
/// | Slurm State      | JobStatus  |
/// |------------------|------------|
/// | PENDING          | Queued     |
/// | CONFIGURING      | Queued     |
/// | RUNNING          | Running    |
/// | COMPLETING       | Running    |
/// | COMPLETED        | Completed  |
/// | FAILED           | Failed     |
/// | OUT_OF_MEMORY    | Failed     |
/// | TIMEOUT          | TimedOut   |
/// | DEADLINE         | TimedOut   |
/// | CANCELLED[+]     | Cancelled  |
/// | REVOKED          | Cancelled  |
/// | NODE_FAIL        | Lost       |
/// | BOOT_FAIL        | Lost       |
/// | PREEMPTED        | Queued     |
/// | SUSPENDED        | Queued     |
/// | REQUEUED         | Queued     |
pub fn map_slurm_state(raw_state: &str) -> Option<JobStatus> {
    // Normalize: uppercase, strip any suffix after space or '+'
    let state = raw_state.trim().to_uppercase();
    let base = state
        .split(|c: char| !c.is_ascii_alphabetic() && c != '_')
        .next()
        .unwrap_or(&state);

    match base {
        // Waiting states → Queued
        "PENDING" | "CONFIGURING" | "PREEMPTED" | "SUSPENDED" | "REQUEUED" => {
            Some(JobStatus::Queued)
        }

        // Active states → Running
        "RUNNING" | "COMPLETING" => Some(JobStatus::Running),

        // Success → Completed
        "COMPLETED" => Some(JobStatus::Completed),

        // Failure states → Failed
        "FAILED" | "OUT_OF_MEMORY" => Some(JobStatus::Failed),

        // Timeout states → TimedOut
        "TIMEOUT" | "DEADLINE" => Some(JobStatus::TimedOut),

        // Cancellation → Cancelled
        "CANCELLED" | "REVOKED" => Some(JobStatus::Cancelled),

        // Infrastructure failure → Lost
        "NODE_FAIL" | "BOOT_FAIL" => Some(JobStatus::Lost),

        _ => {
            tracing::warn!(raw_state = %raw_state, "Unknown Slurm state");
            None
        }
    }
}

/// Parse an exit code string from sacct output.
///
/// sacct formats exit codes as `exit_code:signal` (e.g., `0:0`, `1:0`, `0:9`).
/// Returns the exit code portion.
pub fn parse_exit_code(raw: &str) -> i32 {
    raw.split(':')
        .next()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_queued_states() {
        assert_eq!(map_slurm_state("PENDING"), Some(JobStatus::Queued));
        assert_eq!(map_slurm_state("CONFIGURING"), Some(JobStatus::Queued));
        assert_eq!(map_slurm_state("PREEMPTED"), Some(JobStatus::Queued));
        assert_eq!(map_slurm_state("SUSPENDED"), Some(JobStatus::Queued));
        assert_eq!(map_slurm_state("REQUEUED"), Some(JobStatus::Queued));
    }

    #[test]
    fn test_map_running_states() {
        assert_eq!(map_slurm_state("RUNNING"), Some(JobStatus::Running));
        assert_eq!(map_slurm_state("COMPLETING"), Some(JobStatus::Running));
    }

    #[test]
    fn test_map_completed() {
        assert_eq!(map_slurm_state("COMPLETED"), Some(JobStatus::Completed));
    }

    #[test]
    fn test_map_failed_states() {
        assert_eq!(map_slurm_state("FAILED"), Some(JobStatus::Failed));
        assert_eq!(map_slurm_state("OUT_OF_MEMORY"), Some(JobStatus::Failed));
    }

    #[test]
    fn test_map_timeout_states() {
        assert_eq!(map_slurm_state("TIMEOUT"), Some(JobStatus::TimedOut));
        assert_eq!(map_slurm_state("DEADLINE"), Some(JobStatus::TimedOut));
    }

    #[test]
    fn test_map_cancelled_states() {
        assert_eq!(map_slurm_state("CANCELLED"), Some(JobStatus::Cancelled));
        assert_eq!(map_slurm_state("CANCELLED+"), Some(JobStatus::Cancelled));
        assert_eq!(
            map_slurm_state("CANCELLED by 1000"),
            Some(JobStatus::Cancelled)
        );
        assert_eq!(map_slurm_state("REVOKED"), Some(JobStatus::Cancelled));
    }

    #[test]
    fn test_map_lost_states() {
        assert_eq!(map_slurm_state("NODE_FAIL"), Some(JobStatus::Lost));
        assert_eq!(map_slurm_state("BOOT_FAIL"), Some(JobStatus::Lost));
    }

    #[test]
    fn test_map_unknown_state() {
        assert_eq!(map_slurm_state("GARBAGE"), None);
    }

    #[test]
    fn test_map_case_insensitive() {
        assert_eq!(map_slurm_state("pending"), Some(JobStatus::Queued));
        assert_eq!(map_slurm_state("Running"), Some(JobStatus::Running));
    }

    #[test]
    fn test_map_whitespace() {
        assert_eq!(map_slurm_state("  RUNNING  "), Some(JobStatus::Running));
    }

    #[test]
    fn test_parse_exit_code() {
        assert_eq!(parse_exit_code("0:0"), 0);
        assert_eq!(parse_exit_code("1:0"), 1);
        assert_eq!(parse_exit_code("0:9"), 0);
        assert_eq!(parse_exit_code("137:0"), 137);
        assert_eq!(parse_exit_code("garbage"), -1);
    }
}
