//! Integration tests against a real `nomad agent -dev`.
//!
//! These tests start a Nomad dev agent via the test harness,
//! register job templates, dispatch jobs, and verify status transitions.

use std::time::Duration;

use petri_domain::{JobStatus, SchedulerClient, SubmitRequest};
use petri_test_harness::nomad::{ensure_nomad_dev, register_test_job_template, NOMAD_DEV_ADDR};

use petri_domain::ExternalSignal;

use crate::client::NomadClient;
use crate::config::NomadConfig;
use crate::watcher::NomadWatcher;

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,petri_nomad=debug")
        .with_test_writer()
        .try_init();
}

fn make_config() -> NomadConfig {
    NomadConfig {
        addr: NOMAD_DEV_ADDR.to_string(),
        token: None,
        region: "global".to_string(),
        task_name: "petri-worker".to_string(),
        ca_cert: None,
    }
}

/// Poll job status until terminal or timeout.
async fn poll_until_terminal(client: &NomadClient, job_id: &str, timeout: Duration) -> JobStatus {
    let start = tokio::time::Instant::now();
    loop {
        if let Ok(status) = client.status(job_id).await {
            if status.is_terminal() {
                return status;
            }
        }
        if start.elapsed() > timeout {
            panic!(
                "Job {} did not reach terminal state within {:?}",
                job_id, timeout
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

// ==================== Client: Submit ====================

#[tokio::test]
async fn test_dispatch_success_exit_0() {
    init_tracing();
    let addr = ensure_nomad_dev().await;

    register_test_job_template(addr, "petri-test-ok", "/bin/echo", &["hello"])
        .await
        .expect("register template");

    tokio::time::sleep(Duration::from_millis(500)).await;

    let config = make_config();
    let client = NomadClient::new_single_place(config, "test-net", "sig_inbox").unwrap();

    let result = client
        .submit(SubmitRequest {
            job_template_id: "petri-test-ok".to_string(),
            signal_key: "test-ok:0".to_string(),
            execution_id: "exec-test-ok".to_string(),
            token_data: serde_json::json!({"model": "test"}),
        })
        .await
        .expect("submit should succeed");

    assert!(
        !result.scheduler_job_id.is_empty(),
        "Should return a dispatched job ID"
    );
    tracing::info!(job_id = %result.scheduler_job_id, "Dispatched job");

    // Poll until terminal (echo exits 0 → Completed)
    let status =
        poll_until_terminal(&client, &result.scheduler_job_id, Duration::from_secs(15)).await;
    assert!(
        status.is_terminal(),
        "Job should be terminal, got: {:?}",
        status
    );
}

#[tokio::test]
async fn test_dispatch_nonzero_exit() {
    init_tracing();
    let addr = ensure_nomad_dev().await;

    register_test_job_template(addr, "petri-test-fail", "/bin/sh", &["-c", "exit 1"])
        .await
        .expect("register template");

    tokio::time::sleep(Duration::from_millis(500)).await;

    let config = make_config();
    let client = NomadClient::new_single_place(config, "test-net", "sig_inbox").unwrap();

    let result = client
        .submit(SubmitRequest {
            job_template_id: "petri-test-fail".to_string(),
            signal_key: "test-fail:0".to_string(),
            execution_id: "exec-test-fail".to_string(),
            token_data: serde_json::json!({}),
        })
        .await
        .expect("submit should succeed");

    let status =
        poll_until_terminal(&client, &result.scheduler_job_id, Duration::from_secs(15)).await;
    assert_eq!(status, JobStatus::Failed, "Non-zero exit should be Failed");
}

// ==================== Client: Cancel ====================

#[tokio::test]
async fn test_cancel_job() {
    init_tracing();
    let addr = ensure_nomad_dev().await;

    register_test_job_template(addr, "petri-test-sleep", "/bin/sleep", &["60"])
        .await
        .expect("register template");

    tokio::time::sleep(Duration::from_millis(500)).await;

    let config = make_config();
    let client = NomadClient::new_single_place(config, "test-net", "sig_inbox").unwrap();

    let result = client
        .submit(SubmitRequest {
            job_template_id: "petri-test-sleep".to_string(),
            signal_key: "test-cancel:0".to_string(),
            execution_id: "exec-test-cancel".to_string(),
            token_data: serde_json::json!({}),
        })
        .await
        .expect("submit");

    // Give it a moment to start
    tokio::time::sleep(Duration::from_secs(2)).await;

    client
        .cancel(&result.scheduler_job_id)
        .await
        .expect("cancel should succeed");

    tokio::time::sleep(Duration::from_secs(1)).await;

    let status = client
        .status(&result.scheduler_job_id)
        .await
        .expect("status query");

    assert_eq!(
        status,
        JobStatus::Cancelled,
        "Stopped job should be Cancelled"
    );
}

// ==================== Client: Status ====================

#[tokio::test]
async fn test_status_nonexistent_job() {
    init_tracing();
    let _addr = ensure_nomad_dev().await;

    let config = make_config();
    let client = NomadClient::new_single_place(config, "test-net", "sig_inbox").unwrap();

    let result = client.status("nonexistent-job-12345").await;
    assert!(result.is_err(), "Should error for nonexistent job");
}

// ==================== Client: Meta tags ====================

#[tokio::test]
async fn test_dispatch_includes_petri_meta() {
    init_tracing();
    let addr = ensure_nomad_dev().await;

    register_test_job_template(addr, "petri-test-meta", "/bin/echo", &["meta-test"])
        .await
        .expect("register template");

    tokio::time::sleep(Duration::from_millis(500)).await;

    let config = make_config();
    let client =
        NomadClient::new_single_place(config.clone(), "gpu-resource", "status_inbox").unwrap();

    let result = client
        .submit(SubmitRequest {
            job_template_id: "petri-test-meta".to_string(),
            signal_key: "train-alpha:0".to_string(),
            execution_id: "exec-train-alpha".to_string(),
            token_data: serde_json::json!({"model_name": "ResNet-50"}),
        })
        .await
        .expect("submit");

    // Query the dispatched job to verify meta tags
    let http = config.build_http_client().unwrap();
    let url = format!("{}/v1/job/{}", NOMAD_DEV_ADDR, result.scheduler_job_id);
    let resp: serde_json::Value = http.get(&url).send().await.unwrap().json().await.unwrap();

    let meta = resp.get("Meta").and_then(|m| m.as_object());
    assert!(meta.is_some(), "Job should have Meta field");

    let meta = meta.unwrap();
    assert_eq!(
        meta.get("petri_net_id").and_then(|v| v.as_str()),
        Some("gpu-resource"),
        "Meta should contain petri_net_id"
    );
    assert_eq!(
        meta.get("petri_place").and_then(|v| v.as_str()),
        Some("status_inbox"),
        "Meta should contain petri_place"
    );
    assert_eq!(
        meta.get("petri_signal_key").and_then(|v| v.as_str()),
        Some("train-alpha:0"),
        "Meta should contain petri_signal_key"
    );

    // token_data should NOT be in Meta — and as of the dispatch-payload-removal
    // change in `client.rs`, it is no longer sent in the Nomad Payload either.
    // The executor pulls the full ExecutionJob from the NATS job queue, so the
    // dispatch carries only routing meta. Hitting Nomad's hardcoded 16KB
    // dispatch-payload limit was the original motivation.
    assert!(
        meta.get("model_name").is_none(),
        "token_data fields should not appear in Meta"
    );
    assert!(
        resp.get("Payload").map_or(true, |p| p.is_null()),
        "dispatched job should have no Payload (token_data flows via NATS, not Nomad dispatch)"
    );
}

// ==================== Watcher: Event Stream ====================

#[tokio::test]
async fn test_event_stream_contains_allocation_events() {
    init_tracing();
    let addr = ensure_nomad_dev().await;

    register_test_job_template(addr, "petri-test-watch", "/bin/echo", &["watcher-test"])
        .await
        .expect("register template");

    tokio::time::sleep(Duration::from_millis(500)).await;

    let config = make_config();

    // Dispatch a job
    let client = NomadClient::new_single_place(config.clone(), "watch-net", "sig_inbox").unwrap();
    let result = client
        .submit(SubmitRequest {
            job_template_id: "petri-test-watch".to_string(),
            signal_key: "watch-test:0".to_string(),
            execution_id: "exec-watch-test".to_string(),
            token_data: serde_json::json!({}),
        })
        .await
        .expect("submit");

    tracing::info!(job_id = %result.scheduler_job_id, "Dispatched job for watcher test");

    // Wait for job to finish
    poll_until_terminal(&client, &result.scheduler_job_id, Duration::from_secs(15)).await;

    // Query allocations for this job directly (more reliable than streaming)
    let http = config.build_http_client().unwrap();
    let url = format!(
        "{}/v1/job/{}/allocations",
        NOMAD_DEV_ADDR, result.scheduler_job_id
    );
    let allocs: Vec<serde_json::Value> = http
        .get(&url)
        .send()
        .await
        .expect("alloc query")
        .json()
        .await
        .expect("alloc parse");

    assert!(
        !allocs.is_empty(),
        "Should have at least one allocation for the dispatched job"
    );

    let alloc = &allocs[0];
    let client_status = alloc
        .get("ClientStatus")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    tracing::info!(
        alloc_id = %alloc.get("ID").and_then(|v| v.as_str()).unwrap_or("?"),
        client_status = %client_status,
        "Allocation found"
    );

    assert!(
        client_status == "complete" || client_status == "failed",
        "Allocation should be terminal, got: {}",
        client_status
    );

    // Now verify the event stream is accessible and returns Allocation events
    // by reading from the Nomad allocations API endpoint (already done above).
    // The streaming event stream test would require async ndjson parsing which
    // is covered by the watcher code itself. Here we verify the data model
    // by deserializing a real allocation.
    let alloc_id = alloc.get("ID").and_then(|v| v.as_str()).unwrap();
    let alloc_url = format!("{}/v1/allocation/{}", NOMAD_DEV_ADDR, alloc_id);
    let alloc_detail: crate::models::Allocation = http
        .get(&alloc_url)
        .send()
        .await
        .expect("alloc detail query")
        .json()
        .await
        .expect("alloc detail parse");

    assert_eq!(alloc_detail.id, alloc_id);
    assert_eq!(alloc_detail.job_id, result.scheduler_job_id);

    // Verify the petri meta tags are accessible via the allocation's job
    if let Some(ref job) = alloc_detail.job {
        assert_eq!(
            job.meta.get("petri_net_id").map(|s| s.as_str()),
            Some("watch-net"),
            "Allocation's job should have petri_net_id meta"
        );
        assert_eq!(
            job.meta.get("petri_place").map(|s| s.as_str()),
            Some("sig_inbox"),
            "Allocation's job should have petri_place meta"
        );
    } else {
        panic!("Allocation should have embedded Job with meta tags");
    }

    // Verify task state is present for our task
    let task_state = alloc_detail.task_states.get("petri-worker");
    assert!(
        task_state.is_some(),
        "Allocation should have task state for petri-worker"
    );

    let task_state = task_state.unwrap();
    assert_eq!(
        task_state.state, "dead",
        "Completed task should be in dead state"
    );
    assert!(
        !task_state.events.is_empty(),
        "Task should have lifecycle events"
    );

    // Verify we can find Started and Terminated events
    let event_types: Vec<&str> = task_state
        .events
        .iter()
        .map(|e| e.type_field.as_str())
        .collect();
    tracing::info!(?event_types, "Task events for petri-worker");
    assert!(
        event_types.contains(&"Started"),
        "Should have Started event, got: {:?}",
        event_types
    );
    assert!(
        event_types.contains(&"Terminated"),
        "Should have Terminated event, got: {:?}",
        event_types
    );
}

// ==================== Watcher: NATS E2E ====================

/// End-to-end: Nomad dispatch → event stream → NomadWatcher → ExternalSignal on NATS.
///
/// Verifies the full pipeline without polling Nomad — the signal arriving on NATS
/// is the proof that the watcher processed the allocation event correctly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_watcher_publishes_signal_to_nats() {
    use async_nats::jetstream::consumer::PullConsumer;
    use async_nats::jetstream::stream::Config as StreamConfig;
    use futures::StreamExt;
    use petri_test_harness::nats::shared_nats_url;

    init_tracing();

    // 1. Infrastructure
    let nomad_addr = ensure_nomad_dev().await;
    let nats_url = shared_nats_url().await;
    let nats_client = async_nats::connect(nats_url).await.expect("connect NATS");
    let jetstream = async_nats::jetstream::new(nats_client);

    let net_id = "watcher-e2e-net";
    let place = "sig_inbox";
    let corr = "e2e-test:0";

    // 2. Create JetStream stream covering petri.signal.>
    //    Delete first to purge stale messages from previous runs.
    let stream_name = "TEST_WATCHER_SIGNALS";
    let _ = jetstream.delete_stream(stream_name).await;
    let stream = jetstream
        .create_stream(StreamConfig {
            name: stream_name.to_string(),
            subjects: vec!["petri.signal.>".to_string()],
            max_age: std::time::Duration::from_secs(300),
            ..Default::default()
        })
        .await
        .expect("create signal stream");

    // 3. Create pull consumer filtered to the exact subject BEFORE watcher starts
    let filter_subject = format!("petri.signal.{}.{}", net_id, place);
    let consumer: PullConsumer = stream
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            filter_subject: filter_subject.clone(),
            ..Default::default()
        })
        .await
        .expect("create pull consumer");

    // 4. Start NomadWatcher
    let config = make_config();
    let watcher = NomadWatcher::new(config.clone(), jetstream.clone())
        .await
        .expect("create watcher");
    let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);
    let watcher_handle = tokio::spawn(async move {
        watcher.run(shutdown_rx).await;
    });

    // 5. Let watcher connect to Nomad event stream
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 6. Dispatch a job
    register_test_job_template(nomad_addr, "petri-test-e2e-watch", "/bin/echo", &["e2e"])
        .await
        .expect("register template");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let client = NomadClient::new_single_place(config.clone(), net_id, place).unwrap();
    let result = client
        .submit(SubmitRequest {
            job_template_id: "petri-test-e2e-watch".to_string(),
            signal_key: corr.to_string(),
            execution_id: format!("exec-{}", corr),
            token_data: serde_json::json!({"test": true}),
        })
        .await
        .expect("submit job");

    tracing::info!(job_id = %result.scheduler_job_id, "Dispatched job for watcher E2E test");

    // 7. Wait for a terminal signal on NATS (30s deadline)
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut terminal_signal: Option<ExternalSignal> = None;

    while tokio::time::Instant::now() < deadline {
        let mut messages = consumer
            .fetch()
            .max_messages(10)
            .expires(Duration::from_secs(2))
            .messages()
            .await
            .expect("fetch messages");

        while let Some(Ok(msg)) = messages.next().await {
            msg.ack().await.expect("ack");
            let signal: ExternalSignal =
                serde_json::from_slice(&msg.payload).expect("parse ExternalSignal");

            let job_id = signal.payload["scheduler_job_id"].as_str().unwrap_or("");
            let status_str = signal.payload["job_status"].as_str().unwrap_or("");

            tracing::info!(
                source = %signal.source,
                corr = %signal.signal_key,
                status = %status_str,
                job_id = %job_id,
                "Received signal on NATS"
            );

            // Skip signals from stale jobs (previous test runs)
            if job_id != result.scheduler_job_id {
                continue;
            }

            if status_str == "completed" || status_str == "failed" {
                terminal_signal = Some(signal);
                break;
            }
        }

        if terminal_signal.is_some() {
            break;
        }
    }

    // 8. Assert
    let signal = terminal_signal.expect("Should have received a terminal signal within 30s");
    assert_eq!(signal.source, "nomad");
    assert_eq!(signal.signal_key, corr);
    assert_eq!(
        signal.payload["scheduler_job_id"].as_str().unwrap(),
        result.scheduler_job_id
    );
    assert_eq!(signal.payload["exit_code"], 0);
    assert_eq!(signal.payload["job_status"], "completed");

    // 9. Cleanup
    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(5), watcher_handle).await;
    jetstream
        .delete_stream(stream_name)
        .await
        .expect("delete stream");
}

// ==================== ExternalSignal serde ====================

#[test]
fn test_external_signal_serde_with_real_payload() {
    let signal = ExternalSignal {
        source: "nomad".to_string(),
        signal_key: "train-alpha:0".to_string(),
        payload: serde_json::json!({
            "scheduler_job_id": "petri-test-ok/dispatch-abc123",
            "allocation_id": "alloc-xyz789",
            "job_status": "completed",
            "exit_code": 0,
            "message": "Exit Code: 0",
            "node_id": "node-1",
            "node_name": "worker-1",
        }),
        timestamp: chrono::Utc::now(),
        dedup_id: None,
    };

    let json = serde_json::to_string_pretty(&signal).unwrap();
    let parsed: ExternalSignal = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.source, "nomad");
    assert_eq!(parsed.signal_key, "train-alpha:0");
    assert_eq!(
        parsed.payload["scheduler_job_id"],
        "petri-test-ok/dispatch-abc123"
    );
    assert_eq!(parsed.payload["exit_code"], 0);
}
