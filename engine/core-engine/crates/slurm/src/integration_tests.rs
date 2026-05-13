//! Integration tests for petri-slurm against a live Slurm cluster via SSH.
//!
//! These tests require:
//! - A running Slurm Docker container (`just slurm-up`)
//! - SSH key at `infra/slurm/ssh/slurm_test` (committed to repo)
//! - A job template at `/opt/petri/templates/default.sh` on the remote (baked into image)
//!
//! Run with: `cargo test -p petri-slurm -- --ignored --test-threads=1`

#[cfg(test)]
mod tests {
    use crate::config::SlurmConfig;
    use crate::models::SqueueEntry;
    use crate::ssh::SshSession;
    use crate::status_mapping;
    use petri_domain::{JobStatus, SchedulerClient, SubmitRequest};

    /// Build a config pointing at the local Docker Slurm sandbox.
    fn sandbox_config() -> SlurmConfig {
        SlurmConfig {
            ssh_host: "localhost".to_string(),
            ssh_port: 2222,
            ssh_user: "testuser".to_string(),
            ssh_key: "infra/slurm/ssh/slurm_test".to_string(),
            ssh_known_hosts: "accept".to_string(),
            poll_interval_secs: 2,
            template_dir: "/opt/petri/templates".to_string(),
            lookback_window_secs: 3600,
            command_timeout_secs: 60,
        }
    }

    // ── SSH Layer ──────────────────────────────────────────────────────

    #[tokio::test]
    #[ignore] // requires live Slurm container
    async fn test_ssh_connect_and_exec() {
        let config = sandbox_config();
        let ssh = SshSession::connect(&config).await.expect("SSH connect");

        let output = ssh.exec("hostname").await.expect("hostname");
        assert!(
            !output.trim().is_empty(),
            "hostname should return something"
        );

        let output = ssh.exec("sinfo -h -o '%P %a'").await.expect("sinfo");
        assert!(
            output.contains("debug"),
            "should see the debug partition: {}",
            output
        );
    }

    #[tokio::test]
    #[ignore]
    async fn test_ssh_command_failure_returns_error() {
        let config = sandbox_config();
        let ssh = SshSession::connect(&config).await.expect("SSH connect");

        let result = ssh.exec("false").await;
        assert!(result.is_err(), "exit 1 should be an error");
    }

    // ── SlurmClient: submit / status / cancel ──────────────────────────

    #[tokio::test]
    #[ignore]
    async fn test_submit_and_observe_job() {
        let config = sandbox_config();
        let client =
            crate::client::SlurmClient::new_single_place(config.clone(), "test-net", "inbox");

        // Submit a job
        let request = SubmitRequest {
            job_template_id: "default".to_string(),
            signal_key: "integ-test:0".to_string(),
            execution_id: "exec-integ-test".to_string(),
            token_data: serde_json::json!({"run_id": "test-123"}),
        };

        let result = client.submit(request).await.expect("sbatch should succeed");
        let job_id = &result.scheduler_job_id;
        assert!(
            !job_id.is_empty(),
            "should get a numeric job ID back from sbatch"
        );
        println!("Submitted job: {}", job_id);

        // Verify the job appears in squeue
        let ssh = SshSession::connect(&config).await.expect("SSH connect");
        let squeue_out = ssh.exec("squeue -o '%i|%k|%T' -h").await.expect("squeue");
        println!("squeue output:\n{}", squeue_out);
        let entries = SqueueEntry::parse_all(&squeue_out);
        let our_job = entries.iter().find(|e| e.job_id == *job_id);
        // Job may have already completed by now for short jobs, so check both
        if let Some(entry) = our_job {
            println!(
                "Found job {} in squeue: state={}, comment={}",
                job_id, entry.state, entry.comment
            );
            // Verify the comment contains our routing metadata
            assert!(
                entry.comment.contains("petri_net_id"),
                "comment should contain routing metadata: {}",
                entry.comment
            );
            let status = status_mapping::map_slurm_state(&entry.state);
            assert!(
                status.is_some(),
                "state should be mappable: {}",
                entry.state
            );
        } else {
            println!("Job {} already left squeue (fast completion)", job_id);
        }

        // Wait for the job to finish (it sleeps 2s)
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        // Verify squeue no longer shows the job
        let squeue_out = ssh
            .exec("squeue -o '%i|%k|%T' -h")
            .await
            .expect("squeue after wait");
        let entries = SqueueEntry::parse_all(&squeue_out);
        let still_active = entries.iter().any(|e| e.job_id == *job_id);
        assert!(
            !still_active,
            "job should have completed and left squeue by now"
        );

        // Cancel test: submit another job, then cancel it
        let request2 = SubmitRequest {
            job_template_id: "default".to_string(),
            signal_key: "integ-cancel:0".to_string(),
            execution_id: "exec-integ-cancel".to_string(),
            token_data: serde_json::json!({}),
        };
        let result2 = client
            .submit(request2)
            .await
            .expect("second sbatch should succeed");
        println!(
            "Submitted job for cancel test: {}",
            result2.scheduler_job_id
        );

        client
            .cancel(&result2.scheduler_job_id)
            .await
            .expect("scancel should succeed");
        println!("Cancelled job: {}", result2.scheduler_job_id);
    }

    // ── Parsers against real output ────────────────────────────────────

    #[tokio::test]
    #[ignore]
    async fn test_squeue_parser_against_real_output() {
        let config = sandbox_config();
        let ssh = SshSession::connect(&config).await.expect("SSH connect");

        // Submit a job so there's something to parse
        let submit_out = ssh
            .exec("sbatch --parsable --comment='{\"petri_net_id\":\"parse-test\",\"petri_place\":\"inbox\",\"petri_signal_key\":\"test:0\"}' --job-name=petri-parsetest /opt/petri/templates/default.sh")
            .await
            .expect("sbatch");
        let job_id = submit_out.trim().split(';').next().unwrap().trim();
        println!("Submitted parse-test job: {}", job_id);

        // Get squeue output and parse it
        let squeue_out = ssh.exec("squeue -o '%i|%k|%T' -h").await.expect("squeue");
        println!("Raw squeue output:\n{}", squeue_out);

        let entries = SqueueEntry::parse_all(&squeue_out);
        println!("Parsed {} squeue entries", entries.len());
        for e in &entries {
            println!(
                "  job_id={} state={} comment={}",
                e.job_id, e.state, e.comment
            );
        }

        // Should have at least our job (unless it's already done)
        // Just verify parsing didn't crash and produced valid entries
        for entry in &entries {
            assert!(!entry.job_id.is_empty());
            assert!(!entry.state.is_empty());
        }

        // Clean up
        let _ = ssh.exec(&format!("scancel {}", job_id)).await;
    }

    // ── Status mapping smoke test with real states ─────────────────────

    #[tokio::test]
    #[ignore]
    async fn test_job_lifecycle_states() {
        let config = sandbox_config();
        let ssh = SshSession::connect(&config).await.expect("SSH connect");

        // Submit a fast job
        let submit_out = ssh
            .exec("sbatch --parsable --wrap='echo done' --job-name=petri-lifecycle")
            .await
            .expect("sbatch");
        let job_id = submit_out.trim().split(';').next().unwrap().trim();
        println!("Submitted lifecycle test job: {}", job_id);

        // Immediately check — may be PENDING or RUNNING
        let squeue_out = ssh.exec("squeue -o '%i|%k|%T' -h").await.expect("squeue");
        let entries = SqueueEntry::parse_all(&squeue_out);
        if let Some(entry) = entries.iter().find(|e| e.job_id == job_id) {
            let mapped = status_mapping::map_slurm_state(&entry.state);
            println!("Job {} state={} mapped={:?}", job_id, entry.state, mapped);
            assert!(mapped.is_some());
            let status = mapped.unwrap();
            assert!(
                status == JobStatus::Queued || status == JobStatus::Running,
                "initial state should be Queued or Running, got {:?}",
                status
            );
        }

        // Wait for completion
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        // Job should be gone from squeue
        let squeue_out = ssh.exec("squeue -o '%i|%k|%T' -h").await.expect("squeue");
        let entries = SqueueEntry::parse_all(&squeue_out);
        assert!(
            !entries.iter().any(|e| e.job_id == job_id),
            "job should have left squeue after completion"
        );
    }
}
