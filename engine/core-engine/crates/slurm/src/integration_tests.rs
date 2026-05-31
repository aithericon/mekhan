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

    /// Absolute path to the committed sandbox SSH key.
    ///
    /// `cargo test` runs with CWD = the package manifest dir
    /// (`core-engine/crates/slurm`), not the engine workspace root, so a bare
    /// relative `infra/slurm/ssh/slurm_test` does not resolve. Anchor on
    /// `CARGO_MANIFEST_DIR` and walk up to the engine root (`../../..`) where
    /// `infra/` actually lives.
    fn sandbox_ssh_key() -> String {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../infra/slurm/ssh/slurm_test")
            .to_string_lossy()
            .into_owned()
    }

    /// Build a config pointing at the local Docker Slurm sandbox.
    fn sandbox_config() -> SlurmConfig {
        SlurmConfig {
            ssh_host: "localhost".to_string(),
            ssh_port: 2222,
            ssh_user: "testuser".to_string(),
            ssh_key: sandbox_ssh_key(),
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

    // ── Allocation lifecycle (salloc → scontrol → srun → scancel) ──────

    #[tokio::test]
    #[ignore] // requires live Slurm container
    async fn slurm_alloc_lifecycle() {
        use crate::alloc;

        let config = sandbox_config();
        let ssh = SshSession::connect(&config).await.expect("SSH connect");

        let grant_id = "lease-lifecycle";
        let request = serde_json::json!({});

        // salloc: hold an allocation without running anything.
        let alloc_id = alloc::salloc_no_shell(&ssh, grant_id, &request)
            .await
            .expect("salloc should grant an allocation");
        assert!(!alloc_id.is_empty(), "salloc should return a job id");
        println!("Held allocation: {}", alloc_id);

        // scontrol: the allocation should resolve to a node (CPU-only sandbox).
        // Allow a brief moment for the node to be assigned.
        let mut allocation = alloc::scontrol_node(&ssh, &alloc_id)
            .await
            .expect("scontrol should find the allocation");
        for _ in 0..10 {
            if allocation.node.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            allocation = alloc::scontrol_node(&ssh, &alloc_id)
                .await
                .expect("scontrol should find the allocation");
        }
        let node = allocation
            .node
            .clone()
            .expect("allocation should land on a node");
        assert!(!node.is_empty(), "NodeList should be non-empty");
        println!("Allocation {} on node {}", alloc_id, node);

        // srun: run a command ON the held allocation.
        let output = alloc::srun_into_alloc(&ssh, &alloc_id, "echo lease-ok")
            .await
            .expect("srun into the held allocation should succeed");
        assert!(
            output.contains("lease-ok"),
            "srun output should contain 'lease-ok': {}",
            output
        );

        // scancel: release the allocation.
        alloc::scancel(&ssh, &alloc_id)
            .await
            .expect("scancel should succeed");
        println!("Cancelled allocation: {}", alloc_id);

        // The job should leave squeue once cancelled.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let squeue_out = ssh
            .exec("squeue -o '%i|%k|%T' -h")
            .await
            .expect("squeue after scancel");
        let entries = SqueueEntry::parse_all(&squeue_out);
        assert!(
            !entries.iter().any(|e| e.job_id == alloc_id),
            "cancelled allocation {} should have left squeue: {}",
            alloc_id,
            squeue_out
        );
    }

    // ── L2: submit a body ONTO a held allocation (srun --jobid) ────────

    /// Hold an allocation, then dispatch a body onto it via the `submit` path
    /// with `spec.alloc_id` set (the L2 wire). Asserts the body ran ON the held
    /// allocation (`srun --jobid`, NOT a new `sbatch` job) and the exit code
    /// propagated, then releases the allocation.
    ///
    /// The sandbox has Slurm accounting disabled (`sacct` errors), so the proof
    /// that the body attached to the held allocation rather than queuing a new
    /// job is two-fold: (a) the worker template's stdout — captured straight
    /// off the synchronous `srun` step — shows our `PETRI_TOKEN_DATA` flowing
    /// through plus the template's completion marker, and (b) `squeue` reveals
    /// NO job id beyond the held allocation (a fresh sbatch would have minted a
    /// new one).
    #[tokio::test]
    #[ignore] // requires live Slurm container
    async fn slurm_submit_into_held_alloc() {
        use crate::alloc;

        let config = sandbox_config();
        let ssh = SshSession::connect(&config).await.expect("SSH connect");

        // 1. Hold an allocation (no body running yet).
        let grant_id = "lease-l2-body";
        let alloc_id = alloc::salloc_no_shell(&ssh, grant_id, &serde_json::json!({}))
            .await
            .expect("salloc should grant an allocation");
        assert!(!alloc_id.is_empty(), "salloc should return a job id");
        println!("Held allocation: {}", alloc_id);

        // Wait for the node to be assigned (CPU-only sandbox grants fast).
        let mut allocation = alloc::scontrol_node(&ssh, &alloc_id)
            .await
            .expect("scontrol should find the allocation");
        for _ in 0..10 {
            if allocation.node.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            allocation = alloc::scontrol_node(&ssh, &alloc_id)
                .await
                .expect("scontrol");
        }
        assert!(
            allocation.node.is_some(),
            "allocation should land on a node before dispatch"
        );

        // Snapshot the live job ids before dispatch: just the held allocation.
        let pre = ssh
            .exec("squeue -o '%i' -h")
            .await
            .expect("squeue snapshot");
        let pre_ids: std::collections::HashSet<String> = pre
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        println!("Pre-dispatch job ids: {:?}", pre_ids);

        // 2. Dispatch a body ONTO the held allocation. The L2 wire: alloc_id
        //    rides token_data["spec"]["alloc_id"], so submit() branches to
        //    submit_into_alloc → srun --jobid=<alloc> <template>.
        let client =
            crate::client::SlurmClient::new_single_place(config.clone(), "test-net", "inbox");
        let request = SubmitRequest {
            job_template_id: "default".to_string(),
            signal_key: "integ-l2-body:0".to_string(),
            execution_id: "exec-integ-l2-body".to_string(),
            token_data: serde_json::json!({
                "run_id": "l2-body-123",
                "spec": { "alloc_id": alloc_id }
            }),
        };

        let result = client
            .submit(request)
            .await
            .expect("srun into held alloc should succeed");
        // submit_into_alloc correlates the result on the held alloc id — NOT a
        // freshly minted sbatch id.
        assert_eq!(
            result.scheduler_job_id, alloc_id,
            "leased-body result should correlate on the held alloc id, not a new sbatch id"
        );
        println!("Body dispatched onto allocation {} via submit()", alloc_id);

        // 3a. Behavioral proof the body ran ON the held allocation: re-run the
        //     exact command shape submit_into_alloc emits and capture the
        //     synchronous srun stdout. `default.sh` echoes its PETRI_TOKEN_DATA
        //     and a completion marker — seeing both proves the template executed
        //     under the held alloc with the wired env, and a clean exit (no
        //     CommandFailed) proves the exit code propagated.
        let comment = "{}";
        let token_data_json =
            serde_json::to_string(&serde_json::json!({"run_id": "l2-body-123"})).unwrap();
        let srun_cmd = alloc::srun_into_alloc_template_command(
            &alloc_id,
            comment,
            "integ-l2-body:0",
            &token_data_json,
            "exec-integ-l2-body",
            &format!("{}/default.sh", config.template_dir),
        );
        let srun_out = ssh
            .exec(&srun_cmd)
            .await
            .expect("srun template into held alloc should exit 0");
        println!("srun stdout:\n{}", srun_out);
        assert!(
            srun_out.contains("l2-body-123"),
            "body stdout should echo the dispatched PETRI_TOKEN_DATA: {}",
            srun_out
        );
        assert!(
            srun_out.contains("Job complete"),
            "body should run to completion on the held alloc: {}",
            srun_out
        );

        // 3b. No NEW job id was minted — a fresh sbatch would have appeared in
        //     squeue. Only the held allocation should still be present.
        let post = ssh
            .exec("squeue -o '%i' -h")
            .await
            .expect("squeue after dispatch");
        let new_ids: Vec<String> = post
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && *l != alloc_id && !pre_ids.contains(l))
            .collect();
        assert!(
            new_ids.is_empty(),
            "srun must NOT mint a new batch job; unexpected new job ids {:?} (squeue:\n{})",
            new_ids,
            post
        );

        // 4. Release the allocation (single-node cluster — never leak it).
        alloc::scancel(&ssh, &alloc_id)
            .await
            .expect("scancel should release the allocation");
        println!("Released allocation: {}", alloc_id);
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
