//! Nomad allocation/task state → `JobStatus` translation.

use petri_domain::JobStatus;

use crate::models::{Allocation, Job, TaskEvent};

/// Map a Nomad task event to a `JobStatus`.
///
/// Returns `None` for informational events that don't represent a meaningful
/// status change worth signaling (e.g., "Received", "Driver").
pub fn map_task_event(event: &TaskEvent) -> Option<JobStatus> {
    match event.type_field.as_str() {
        "Started" => Some(JobStatus::Running),
        "Terminated" => {
            if event.exit_code == 0 {
                Some(JobStatus::Completed)
            } else {
                Some(JobStatus::Failed)
            }
        }
        "Killed" => Some(JobStatus::Failed),
        "Driver Failure" => Some(JobStatus::Failed),
        // Informational events — not signaled
        "Received" | "Driver" | "Task Setup" | "Building Task Directory" => None,
        _ => None,
    }
}

/// Map a Nomad job query response to a `JobStatus`.
///
/// Used by `NomadClient::status()` to translate `GET /v1/job/{id}` responses.
pub fn map_job_status(job: &Job, _task_name: &str) -> JobStatus {
    match job.status.as_str() {
        "pending" => JobStatus::Queued,
        "running" => JobStatus::Running,
        "dead" => {
            if job.stop {
                return JobStatus::Cancelled;
            }
            // Nomad's job query response does not include TaskStates (those
            // live on allocations). Without exit code info we conservatively
            // report Failed; the watcher path uses task events for finer
            // granularity (Completed vs Failed).
            JobStatus::Failed
        }
        _ => JobStatus::Lost,
    }
}

/// Map an allocation's client_status to a `JobStatus`.
///
/// Used by the watcher as a fallback when no task events are available.
pub fn map_alloc_client_status(alloc: &Allocation) -> Option<JobStatus> {
    match alloc.client_status.as_str() {
        "pending" => Some(JobStatus::Queued),
        "running" => Some(JobStatus::Running),
        "complete" => Some(JobStatus::Completed),
        "failed" => Some(JobStatus::Failed),
        "lost" => Some(JobStatus::Lost),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Allocation, Job, TaskEvent};
    use std::collections::HashMap;

    fn make_task_event(type_field: &str, exit_code: i32) -> TaskEvent {
        TaskEvent {
            type_field: type_field.to_string(),
            exit_code,
            display_message: String::new(),
            time: 0,
        }
    }

    #[test]
    fn test_map_task_event_started() {
        let event = make_task_event("Started", 0);
        assert_eq!(map_task_event(&event), Some(JobStatus::Running));
    }

    #[test]
    fn test_map_task_event_terminated_success() {
        let event = make_task_event("Terminated", 0);
        assert_eq!(map_task_event(&event), Some(JobStatus::Completed));
    }

    #[test]
    fn test_map_task_event_terminated_failure() {
        let event = make_task_event("Terminated", 1);
        assert_eq!(map_task_event(&event), Some(JobStatus::Failed));
    }

    #[test]
    fn test_map_task_event_killed() {
        let event = make_task_event("Killed", 0);
        assert_eq!(map_task_event(&event), Some(JobStatus::Failed));
    }

    #[test]
    fn test_map_task_event_driver_failure() {
        let event = make_task_event("Driver Failure", 0);
        assert_eq!(map_task_event(&event), Some(JobStatus::Failed));
    }

    #[test]
    fn test_map_task_event_informational() {
        assert_eq!(map_task_event(&make_task_event("Received", 0)), None);
        assert_eq!(map_task_event(&make_task_event("Driver", 0)), None);
        assert_eq!(map_task_event(&make_task_event("Task Setup", 0)), None);
    }

    fn make_job(status: &str, stop: bool) -> Job {
        Job {
            id: "test-job".to_string(),
            name: "test-job".to_string(),
            status: status.to_string(),
            stop,
            meta: HashMap::new(),
            task_groups: Vec::new(),
        }
    }

    #[test]
    fn test_map_job_status_pending() {
        let job = make_job("pending", false);
        assert_eq!(map_job_status(&job, "petri-worker"), JobStatus::Queued);
    }

    #[test]
    fn test_map_job_status_running() {
        let job = make_job("running", false);
        assert_eq!(map_job_status(&job, "petri-worker"), JobStatus::Running);
    }

    #[test]
    fn test_map_job_status_dead_stopped() {
        let job = make_job("dead", true);
        assert_eq!(map_job_status(&job, "petri-worker"), JobStatus::Cancelled);
    }

    #[test]
    fn test_map_job_status_dead_not_stopped() {
        let job = make_job("dead", false);
        // Without task state info, defaults to Failed
        assert_eq!(map_job_status(&job, "petri-worker"), JobStatus::Failed);
    }

    #[test]
    fn test_map_job_status_unknown() {
        let job = make_job("garbage", false);
        assert_eq!(map_job_status(&job, "petri-worker"), JobStatus::Lost);
    }

    #[test]
    fn test_map_alloc_client_status() {
        let make_alloc = |status: &str| Allocation {
            id: "a".into(),
            job_id: "j".into(),
            client_status: status.into(),
            desired_status: "run".into(),
            ..Default::default()
        };

        assert_eq!(
            map_alloc_client_status(&make_alloc("pending")),
            Some(JobStatus::Queued)
        );
        assert_eq!(
            map_alloc_client_status(&make_alloc("running")),
            Some(JobStatus::Running)
        );
        assert_eq!(
            map_alloc_client_status(&make_alloc("complete")),
            Some(JobStatus::Completed)
        );
        assert_eq!(
            map_alloc_client_status(&make_alloc("failed")),
            Some(JobStatus::Failed)
        );
        assert_eq!(
            map_alloc_client_status(&make_alloc("lost")),
            Some(JobStatus::Lost)
        );
        assert_eq!(map_alloc_client_status(&make_alloc("unknown")), None);
    }
}
