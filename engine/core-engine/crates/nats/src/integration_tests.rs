// =============================================================================
// Clockmaster Integration Tests
// =============================================================================

use futures::StreamExt;
use petri_test_harness::nats::{shared_nats_url, NatsTestContext};
use std::time::Duration;

#[tokio::test]
async fn test_clockmaster_schedule_and_fire() {
    use crate::clockmaster::{Clockmaster, NatsTimerClient};
    use petri_domain::timer::{TimerClient, TimerScheduleRequest};
    use petri_domain::ExternalSignal;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url)
        .await
        .expect("Failed to create context");

    // Create a unique bucket name for this test
    let bucket_name = format!("TIMERS_{}", ctx.prefix.replace("_", "").to_uppercase());

    // Create the KV bucket
    let kv_config = async_nats::jetstream::kv::Config {
        bucket: bucket_name.clone(),
        history: 1,
        ..Default::default()
    };
    ctx.jetstream
        .create_key_value(kv_config)
        .await
        .expect("create bucket");

    // Initialize TimerClient and Clockmaster
    let timer_client = NatsTimerClient::with_bucket(&ctx.jetstream, &bucket_name)
        .await
        .expect("create client");

    // Multi-tenancy (phase 2): Clockmaster fires timer signals under the
    // timer's workspace on the namespaced subject `petri.{ws}.{net}.signal.{place}`,
    // captured by the global `PETRI_GLOBAL` stream (no longer the old
    // `tns.{prefix}.signals.>` test stream). Use a unique per-test workspace so
    // the filter subject can't collide with other tests sharing the NATS server.
    let workspace_id = format!("cmws-{}", ctx.prefix);
    let net_id = "test-net";
    let place_id = "test-place";
    let signal_subject = crate::Subjects::signal_transfer(&workspace_id, net_id, place_id);

    // Clockmaster's 3rd arg is now the workspace_id (was a signal prefix).
    let clockmaster =
        Clockmaster::with_options(ctx.jetstream.clone(), &bucket_name, &workspace_id)
            .await
            .expect("create clockmaster");

    // Consume the fired signal from PETRI_GLOBAL, filtered to this net's
    // workspace-scoped signal subject. Create the consumer FIRST (before the
    // signal is published) so it sees the timer fire.
    let global = petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure PETRI_GLOBAL");
    let consumer = global
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name: Some(format!("cm_test_{}", ctx.prefix)),
            filter_subject: signal_subject.clone(),
            ..Default::default()
        })
        .await
        .expect("consumer");

    let mut messages = consumer.messages().await.expect("messages");

    // Schedule a timer BEFORE clockmaster starts (so hydration picks it up).
    // The timer carries the same workspace so it fires under `petri.{ws}…`.
    let correlation_id = uuid::Uuid::new_v4();

    timer_client
        .schedule(TimerScheduleRequest {
            net_id: net_id.to_string(),
            place_id: place_id.to_string(),
            correlation_id,
            delay_ms: 100, // Short delay
            payload: serde_json::json!({"foo": "bar"}),
            workspace_id: workspace_id.clone(),
        })
        .await
        .expect("schedule");

    // Run Clockmaster in background AFTER timer is in KV
    let cm_handle = tokio::spawn(async move {
        clockmaster.run().await.unwrap();
    });

    // Wait for signal
    let msg = tokio::time::timeout(Duration::from_secs(5), messages.next())
        .await
        .expect("timeout")
        .expect("no message")
        .expect("error");

    let signal: ExternalSignal = serde_json::from_slice(&msg.payload).expect("parse");

    assert_eq!(signal.source, "clockmaster");
    assert_eq!(signal.signal_key, correlation_id.to_string());
    assert_eq!(signal.payload["foo"], "bar");

    // Verify metadata exists
    assert!(signal.payload.get("drift_ms").is_some());
    assert!(signal.payload.get("scheduled_at").is_some());
    assert!(signal.payload.get("triggered_at").is_some());

    cm_handle.abort();
    ctx.cleanup().await.ok();
}

// =============================================================================
// ActivityTracker Integration Tests
// =============================================================================

/// Helper: create a unique KV bucket for activity tracking per test.
async fn create_activity_kv(
    jetstream: &async_nats::jetstream::Context,
    prefix: &str,
) -> async_nats::jetstream::kv::Store {
    let bucket_name = format!("ACTIVITY_{}", prefix.replace('_', "").to_uppercase());
    let kv_config = async_nats::jetstream::kv::Config {
        bucket: bucket_name.clone(),
        history: 1,
        ..Default::default()
    };
    jetstream
        .create_key_value(kv_config)
        .await
        .expect("create activity KV bucket")
}

#[tokio::test]
async fn test_activity_touch_creates_entry() {
    use crate::hibernation::{ActivityState, ActivityTracker};

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    let kv = create_activity_kv(&ctx.jetstream, &ctx.prefix).await;
    let tracker = ActivityTracker::new(
        kv,
        Duration::from_secs(300),
        crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
    );

    tracker.touch("net-1").await.expect("touch");

    let entry = tracker.get_entry("net-1").await.expect("get_entry");
    assert!(entry.is_some(), "Entry should exist after touch");
    assert_eq!(entry.unwrap().state, ActivityState::Hot);

    ctx.cleanup().await.ok();
}

#[tokio::test]
async fn test_activity_is_hot_after_touch() {
    use crate::hibernation::ActivityTracker;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    let kv = create_activity_kv(&ctx.jetstream, &ctx.prefix).await;
    let tracker = ActivityTracker::new(
        kv,
        Duration::from_secs(300),
        crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
    );

    tracker.touch("net-1").await.expect("touch");
    assert!(tracker.is_hot("net-1").await.expect("is_hot"));

    ctx.cleanup().await.ok();
}

#[tokio::test]
async fn test_activity_is_hot_returns_false_for_unknown() {
    use crate::hibernation::ActivityTracker;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    let kv = create_activity_kv(&ctx.jetstream, &ctx.prefix).await;
    let tracker = ActivityTracker::new(
        kv,
        Duration::from_secs(300),
        crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
    );

    assert!(!tracker.is_hot("nonexistent").await.expect("is_hot"));

    ctx.cleanup().await.ok();
}

#[tokio::test]
async fn test_activity_mark_hibernating() {
    use crate::hibernation::ActivityTracker;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    let kv = create_activity_kv(&ctx.jetstream, &ctx.prefix).await;
    let tracker = ActivityTracker::new(
        kv,
        Duration::from_secs(300),
        crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
    );

    tracker.touch("net-1").await.expect("touch");
    assert!(tracker.is_hot("net-1").await.expect("is_hot"));

    tracker
        .mark_hibernating("net-1")
        .await
        .expect("mark_hibernating");
    assert!(!tracker.is_hot("net-1").await.expect("is_hot after mark"));

    ctx.cleanup().await.ok();
}

#[tokio::test]
async fn test_activity_remove_deletes_entry() {
    use crate::hibernation::ActivityTracker;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    let kv = create_activity_kv(&ctx.jetstream, &ctx.prefix).await;
    let tracker = ActivityTracker::new(
        kv,
        Duration::from_secs(300),
        crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
    );

    tracker.touch("net-1").await.expect("touch");
    assert!(tracker.get_entry("net-1").await.expect("get").is_some());

    tracker.remove("net-1").await.expect("remove");
    // After delete, NATS KV returns None for the key
    let entry = tracker.get_entry("net-1").await.expect("get after remove");
    assert!(entry.is_none(), "Entry should be gone after remove");

    ctx.cleanup().await.ok();
}

// =============================================================================
// HibernationMaster Integration Tests
// =============================================================================

#[tokio::test]
async fn test_hibernation_triggers_on_timeout() {
    use crate::hibernation::{ActivityTracker, HibernationMaster, NetHibernator};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// Mock hibernator that records which nets were hibernated.
    struct MockHibernator {
        hibernated: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl NetHibernator for MockHibernator {
        async fn hibernate(&self, net_id: &str) -> Result<(), String> {
            self.hibernated.lock().await.push(net_id.to_string());
            Ok(())
        }
    }

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    let kv = create_activity_kv(&ctx.jetstream, &ctx.prefix).await;

    let idle_timeout = Duration::from_millis(200);
    let activity = Arc::new(ActivityTracker::new(
        kv,
        idle_timeout,
        crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
    ));
    let mock_hibernator = Arc::new(MockHibernator {
        hibernated: Mutex::new(Vec::new()),
    });

    // Touch net before starting master
    activity.touch("net-idle").await.expect("touch");

    let master = Arc::new(HibernationMaster::new(
        activity.clone(),
        mock_hibernator.clone(),
    ));

    // Run master in background
    let master_handle = tokio::spawn({
        let master = master.clone();
        async move { master.run().await }
    });

    // Wait for idle timeout + generous buffer for sleep task to fire
    tokio::time::sleep(Duration::from_millis(1000)).await;

    let hibernated = mock_hibernator.hibernated.lock().await;
    assert!(
        hibernated.contains(&"net-idle".to_string()),
        "Net should have been hibernated after idle timeout. Hibernated: {:?}",
        *hibernated
    );

    master_handle.abort();
    ctx.cleanup().await.ok();
}

#[tokio::test]
async fn test_hibernation_resets_on_retouch() {
    use crate::hibernation::{ActivityTracker, HibernationMaster, NetHibernator};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    struct MockHibernator {
        hibernated: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl NetHibernator for MockHibernator {
        async fn hibernate(&self, net_id: &str) -> Result<(), String> {
            self.hibernated.lock().await.push(net_id.to_string());
            Ok(())
        }
    }

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    let kv = create_activity_kv(&ctx.jetstream, &ctx.prefix).await;

    let idle_timeout = Duration::from_millis(500);
    let activity = Arc::new(ActivityTracker::new(
        kv,
        idle_timeout,
        crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
    ));
    let mock_hibernator = Arc::new(MockHibernator {
        hibernated: Mutex::new(Vec::new()),
    });

    // Touch net before starting master
    activity.touch("net-active").await.expect("touch");

    let master = Arc::new(HibernationMaster::new(
        activity.clone(),
        mock_hibernator.clone(),
    ));

    let master_handle = tokio::spawn({
        let master = master.clone();
        async move { master.run().await }
    });

    // Wait 250ms (half of timeout), then re-touch
    tokio::time::sleep(Duration::from_millis(250)).await;
    activity.touch("net-active").await.expect("retouch");

    // Wait another 300ms — total 550ms from start, but only 300ms from retouch
    // (less than 500ms timeout)
    tokio::time::sleep(Duration::from_millis(300)).await;

    let hibernated = mock_hibernator.hibernated.lock().await;
    assert!(
        !hibernated.contains(&"net-active".to_string()),
        "Net should NOT be hibernated (retouch reset the timer). Hibernated: {:?}",
        *hibernated
    );

    master_handle.abort();
    ctx.cleanup().await.ok();
}

// =============================================================================
// NetMetadataProjection KV Integration Tests
// =============================================================================

/// Helper: create a unique KV bucket for metadata per test.
async fn create_metadata_kv(
    jetstream: &async_nats::jetstream::Context,
    prefix: &str,
) -> async_nats::jetstream::kv::Store {
    let bucket_name = format!("METADATA_{}", prefix.replace('_', "").to_uppercase());
    let kv_config = async_nats::jetstream::kv::Config {
        bucket: bucket_name.clone(),
        history: 1,
        ..Default::default()
    };
    jetstream
        .create_key_value(kv_config)
        .await
        .expect("create metadata KV bucket")
}

#[tokio::test]
async fn test_metadata_projection_get_returns_none_for_unknown() {
    use crate::net_metadata::NetMetadataProjection;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    let kv = create_metadata_kv(&ctx.jetstream, &ctx.prefix).await;

    let projection = NetMetadataProjection::new(ctx.jetstream.clone(), kv);

    let result = projection.get("nonexistent").await.expect("get");
    assert!(result.is_none());

    ctx.cleanup().await.ok();
}

#[tokio::test]
async fn test_metadata_kv_put_and_get_roundtrip() {
    use crate::net_metadata::{NetMetadata, NetMetadataProjection, NetStatus};

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    let kv = create_metadata_kv(&ctx.jetstream, &ctx.prefix).await;

    // Manually put metadata (simulating what MetadataHandler does)
    let meta = NetMetadata {
        net_id: "test-net".to_string(),
        status: NetStatus::Created,
            workspace_id: crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
        template_id: Some("tmpl-1".to_string()),
        parameters: Some(serde_json::json!({"gpu": 4})),
        created_at: chrono::Utc::now().to_rfc3339(),
        created_by: Some("test-user".to_string()),
        label: None,
        completed_at: None,
        exit_code: None,
        cancelled_at: None,
        cancelled_by: None,
        cancel_reason: None,
    };

    let value = serde_json::to_vec(&meta).unwrap();
    kv.put("test-net", value.into()).await.expect("put");

    let projection = NetMetadataProjection::new(ctx.jetstream.clone(), kv);

    let fetched = projection
        .get("test-net")
        .await
        .expect("get")
        .expect("should exist");
    assert_eq!(fetched.net_id, "test-net");
    assert_eq!(fetched.status, NetStatus::Created);
    assert_eq!(fetched.template_id, Some("tmpl-1".to_string()));
    assert_eq!(fetched.created_by, Some("test-user".to_string()));

    ctx.cleanup().await.ok();
}

#[tokio::test]
async fn test_metadata_projection_list_all() {
    use crate::net_metadata::{NetMetadata, NetMetadataProjection, NetStatus};

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    let kv = create_metadata_kv(&ctx.jetstream, &ctx.prefix).await;

    // Insert 3 nets
    for i in 1..=3 {
        let meta = NetMetadata {
            net_id: format!("net-{}", i),
            status: NetStatus::Running,
            workspace_id: crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
            template_id: None,
            parameters: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            created_by: None,
            label: None,
            completed_at: None,
            exit_code: None,
            cancelled_at: None,
            cancelled_by: None,
            cancel_reason: None,
        };
        let value = serde_json::to_vec(&meta).unwrap();
        kv.put(&format!("net-{}", i), value.into())
            .await
            .expect("put");
    }

    let projection = NetMetadataProjection::new(ctx.jetstream.clone(), kv);

    let all = projection.list_all().await.expect("list_all");
    assert_eq!(all.len(), 3, "Should list all 3 nets");

    ctx.cleanup().await.ok();
}

#[tokio::test]
async fn test_metadata_status_transitions() {
    use crate::net_metadata::{NetMetadata, NetMetadataProjection, NetStatus};

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    let kv = create_metadata_kv(&ctx.jetstream, &ctx.prefix).await;

    // Simulate full lifecycle: Created → Running → Completed
    let mut meta = NetMetadata {
        net_id: "lifecycle-net".to_string(),
        status: NetStatus::Created,
            workspace_id: crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
        template_id: None,
        parameters: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        created_by: Some("admin".to_string()),
        label: None,
        completed_at: None,
        exit_code: None,
        cancelled_at: None,
        cancelled_by: None,
        cancel_reason: None,
    };
    let value = serde_json::to_vec(&meta).unwrap();
    kv.put("lifecycle-net", value.into())
        .await
        .expect("put created");

    let projection = NetMetadataProjection::new(ctx.jetstream.clone(), kv.clone());

    // Update to Running
    meta.status = NetStatus::Running;
    let value = serde_json::to_vec(&meta).unwrap();
    kv.put("lifecycle-net", value.into())
        .await
        .expect("put running");

    let fetched = projection.get("lifecycle-net").await.expect("get").unwrap();
    assert_eq!(fetched.status, NetStatus::Running);

    // Update to Completed with exit code
    meta.status = NetStatus::Completed;
    meta.completed_at = Some(chrono::Utc::now().to_rfc3339());
    meta.exit_code = Some(serde_json::json!(0));
    let value = serde_json::to_vec(&meta).unwrap();
    kv.put("lifecycle-net", value.into())
        .await
        .expect("put completed");

    let fetched = projection.get("lifecycle-net").await.expect("get").unwrap();
    assert_eq!(fetched.status, NetStatus::Completed);
    assert_eq!(fetched.exit_code, Some(serde_json::json!(0)));
    assert!(fetched.completed_at.is_some());

    ctx.cleanup().await.ok();
}

#[tokio::test]
async fn test_metadata_cancelled_status() {
    use crate::net_metadata::{NetMetadata, NetMetadataProjection, NetStatus};

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    let kv = create_metadata_kv(&ctx.jetstream, &ctx.prefix).await;

    let meta = NetMetadata {
        net_id: "cancelled-net".to_string(),
        status: NetStatus::Cancelled,
            workspace_id: crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
        template_id: None,
        parameters: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        created_by: None,
        label: None,
        completed_at: None,
        exit_code: None,
        cancelled_at: Some(chrono::Utc::now().to_rfc3339()),
        cancelled_by: Some("admin".to_string()),
        cancel_reason: Some("test cancellation".to_string()),
    };
    let value = serde_json::to_vec(&meta).unwrap();
    kv.put("cancelled-net", value.into()).await.expect("put");

    let projection = NetMetadataProjection::new(ctx.jetstream.clone(), kv);

    let fetched = projection.get("cancelled-net").await.expect("get").unwrap();
    assert_eq!(fetched.status, NetStatus::Cancelled);
    assert_eq!(fetched.cancelled_by, Some("admin".to_string()));
    assert_eq!(fetched.cancel_reason, Some("test cancellation".to_string()));
    assert!(fetched.cancelled_at.is_some());

    ctx.cleanup().await.ok();
}

// =============================================================================
// Tombstone Rejection Tests (Metadata KV)
// =============================================================================

/// Completed net's metadata acts as a tombstone — can be detected and rejected.
#[tokio::test]
async fn test_metadata_tombstone_completed_net() {
    use crate::net_metadata::{NetMetadata, NetMetadataProjection, NetStatus};

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    let kv = create_metadata_kv(&ctx.jetstream, &ctx.prefix).await;

    // Simulate net lifecycle: Created → Running → Completed
    let meta = NetMetadata {
        net_id: "tombstone-net".to_string(),
        status: NetStatus::Completed,
            workspace_id: crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
        template_id: None,
        parameters: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        created_by: None,
        label: None,
        completed_at: Some(chrono::Utc::now().to_rfc3339()),
        exit_code: Some(serde_json::json!(0)),
        cancelled_at: None,
        cancelled_by: None,
        cancel_reason: None,
    };
    let value = serde_json::to_vec(&meta).unwrap();
    kv.put("tombstone-net", value.into()).await.expect("put");

    let projection = NetMetadataProjection::new(ctx.jetstream.clone(), kv.clone());

    // Tombstone check: reading back should show Completed
    let fetched = projection.get("tombstone-net").await.expect("get").unwrap();
    assert_eq!(fetched.status, NetStatus::Completed);

    // This is the pattern used in RegistryResolver to reject signals:
    let is_tombstone =
        fetched.status == NetStatus::Completed || fetched.status == NetStatus::Cancelled;
    assert!(is_tombstone, "Completed net should be treated as tombstone");

    ctx.cleanup().await.ok();
}

/// Cancelled net's metadata acts as a tombstone.
#[tokio::test]
async fn test_metadata_tombstone_cancelled_net() {
    use crate::net_metadata::{NetMetadata, NetMetadataProjection, NetStatus};

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    let kv = create_metadata_kv(&ctx.jetstream, &ctx.prefix).await;

    let meta = NetMetadata {
        net_id: "cancelled-tombstone".to_string(),
        status: NetStatus::Cancelled,
            workspace_id: crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
        template_id: None,
        parameters: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        created_by: None,
        label: None,
        completed_at: None,
        exit_code: None,
        cancelled_at: Some(chrono::Utc::now().to_rfc3339()),
        cancelled_by: Some("admin".to_string()),
        cancel_reason: Some("manual stop".to_string()),
    };
    let value = serde_json::to_vec(&meta).unwrap();
    kv.put("cancelled-tombstone", value.into())
        .await
        .expect("put");

    let projection = NetMetadataProjection::new(ctx.jetstream.clone(), kv.clone());

    let fetched = projection
        .get("cancelled-tombstone")
        .await
        .expect("get")
        .unwrap();
    let is_tombstone =
        fetched.status == NetStatus::Completed || fetched.status == NetStatus::Cancelled;
    assert!(is_tombstone, "Cancelled net should be treated as tombstone");

    ctx.cleanup().await.ok();
}

/// Running net should NOT be treated as a tombstone.
#[tokio::test]
async fn test_metadata_running_net_not_tombstone() {
    use crate::net_metadata::{NetMetadata, NetMetadataProjection, NetStatus};

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    let kv = create_metadata_kv(&ctx.jetstream, &ctx.prefix).await;

    let meta = NetMetadata {
        net_id: "running-net".to_string(),
        status: NetStatus::Running,
            workspace_id: crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
        template_id: None,
        parameters: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        created_by: None,
        label: None,
        completed_at: None,
        exit_code: None,
        cancelled_at: None,
        cancelled_by: None,
        cancel_reason: None,
    };
    let value = serde_json::to_vec(&meta).unwrap();
    kv.put("running-net", value.into()).await.expect("put");

    let projection = NetMetadataProjection::new(ctx.jetstream.clone(), kv.clone());

    let fetched = projection.get("running-net").await.expect("get").unwrap();
    let is_tombstone =
        fetched.status == NetStatus::Completed || fetched.status == NetStatus::Cancelled;
    assert!(
        !is_tombstone,
        "Running net should NOT be treated as tombstone"
    );

    ctx.cleanup().await.ok();
}

// =============================================================================
// GlobalSignalListener + Tombstone Rejection E2E Tests
// =============================================================================

/// E2E test: GlobalSignalListener routes signals through a resolver that checks
/// the metadata KV for tombstones. Signals to completed/cancelled nets are rejected;
/// signals to running nets are accepted and injected.
#[tokio::test]
async fn test_global_signal_rejects_tombstone_accepts_running() {
    use crate::global_signal_listener::{
        GlobalSignalListener, NetResolver, SignalInjectError, SignalTarget,
    };
    use crate::net_metadata::{NetMetadata, NetStatus};
    use crate::subjects::Subjects;
    use petri_domain::{ExternalSignal, TokenColor};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // --- Mock SignalTarget: records injected signals ---
    #[derive(Default)]
    struct MockSignalTarget {
        injections: Mutex<Vec<(String, TokenColor)>>,
    }

    #[async_trait::async_trait]
    impl SignalTarget for MockSignalTarget {
        async fn inject_signal_with_meta(
            &self,
            place_name: &str,
            color: TokenColor,
            _reply_routing: Option<petri_domain::ReplyRouting>,
            _signal_key: Option<String>,
            _dedup_id: Option<String>,
        ) -> Result<(), SignalInjectError> {
            self.injections
                .lock()
                .await
                .push((place_name.to_string(), color));
            Ok(())
        }
        fn notify_eval(&self) {}
    }

    // --- Mock NetResolver: checks metadata KV for tombstones ---
    // Rejects unknown nets (no metadata) to isolate from parallel tests.
    struct TombstoneCheckingResolver {
        metadata_kv: async_nats::jetstream::kv::Store,
        target: Arc<MockSignalTarget>,
        resolve_calls: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl NetResolver for TombstoneCheckingResolver {
        async fn resolve_net(&self, net_id: &str) -> Result<Arc<dyn SignalTarget>, String> {
            self.resolve_calls.lock().await.push(net_id.to_string());

            // Check metadata KV — same pattern as RegistryResolver in main.rs
            match self.metadata_kv.get(net_id).await {
                Ok(Some(entry)) => {
                    if let Ok(meta) = serde_json::from_slice::<NetMetadata>(&entry) {
                        if meta.status == NetStatus::Completed
                            || meta.status == NetStatus::Cancelled
                        {
                            return Err(format!(
                                "Net '{}' is {:?} — cannot accept signals",
                                net_id, meta.status
                            ));
                        }
                    }
                    Ok(self.target.clone())
                }
                // Unknown net — reject (not managed by this resolver)
                _ => Err(format!("Net '{}' not found", net_id)),
            }
        }
    }

    // --- Setup ---
    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");

    // Ensure PETRI_GLOBAL stream exists
    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    // Create metadata KV bucket
    let metadata_kv = create_metadata_kv(&ctx.jetstream, &ctx.prefix).await;

    // Pre-populate: completed net (tombstone)
    let completed_meta = NetMetadata {
        net_id: "completed-net".to_string(),
        status: NetStatus::Completed,
            workspace_id: crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
        template_id: None,
        parameters: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        created_by: None,
        label: None,
        completed_at: Some(chrono::Utc::now().to_rfc3339()),
        exit_code: Some(serde_json::json!(0)),
        cancelled_at: None,
        cancelled_by: None,
        cancel_reason: None,
    };
    let value = serde_json::to_vec(&completed_meta).unwrap();
    metadata_kv
        .put("completed-net", value.into())
        .await
        .expect("put completed");

    // Pre-populate: running net (should accept signals)
    let running_meta = NetMetadata {
        net_id: "running-net".to_string(),
        status: NetStatus::Running,
            workspace_id: crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
        template_id: None,
        parameters: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        created_by: None,
        label: None,
        completed_at: None,
        exit_code: None,
        cancelled_at: None,
        cancelled_by: None,
        cancel_reason: None,
    };
    let value = serde_json::to_vec(&running_meta).unwrap();
    metadata_kv
        .put("running-net", value.into())
        .await
        .expect("put running");

    // Build resolver with tombstone checking
    let mock_target = Arc::new(MockSignalTarget::default());
    let resolver = Arc::new(TombstoneCheckingResolver {
        metadata_kv: metadata_kv.clone(),
        target: mock_target.clone(),
        resolve_calls: Mutex::new(Vec::new()),
    });

    // Start GlobalSignalListener with unique consumer name
    let consumer_name = format!("gsl-test-tombstone-{}", ctx.prefix);
    let listener = Arc::new(GlobalSignalListener::with_consumer_name(
        ctx.jetstream.clone(),
        resolver.clone(),
        None,
        consumer_name,
    ));
    let listener_handle = listener.start();

    // Give the consumer a moment to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // --- Act: publish signal to COMPLETED net (should be rejected) ---
    let signal_to_completed = ExternalSignal {
        source: "test".to_string(),
        signal_key: "corr-1".to_string(),
        payload: serde_json::json!({"data": "for-completed"}),
        timestamp: chrono::Utc::now(),
        dedup_id: None,
    };
    let subject =
        Subjects::signal_transfer(Subjects::DEFAULT_WORKSPACE, "completed-net", "some_place");
    let payload = serde_json::to_vec(&signal_to_completed).unwrap();
    ctx.jetstream
        .publish(subject, payload.into())
        .await
        .expect("publish signal to completed")
        .await
        .expect("ack");

    // --- Act: publish signal to RUNNING net (should be accepted) ---
    let signal_to_running = ExternalSignal {
        source: "test".to_string(),
        signal_key: "corr-2".to_string(),
        payload: serde_json::json!({"data": "for-running"}),
        timestamp: chrono::Utc::now(),
        dedup_id: None,
    };
    let subject = Subjects::signal_transfer(Subjects::DEFAULT_WORKSPACE, "running-net", "inbox");
    let payload = serde_json::to_vec(&signal_to_running).unwrap();
    ctx.jetstream
        .publish(subject, payload.into())
        .await
        .expect("publish signal to running")
        .await
        .expect("ack");

    // Wait for messages to be processed
    tokio::time::sleep(Duration::from_secs(2)).await;

    // --- Assert ---

    // Resolver should have been called for both our nets
    let calls = resolver.resolve_calls.lock().await;
    assert!(
        calls.contains(&"completed-net".to_string()),
        "Resolver should have been called for completed-net. Calls: {:?}",
        *calls
    );
    assert!(
        calls.contains(&"running-net".to_string()),
        "Resolver should have been called for running-net. Calls: {:?}",
        *calls
    );

    // Only the running net's signal should have been injected.
    // The completed net's signal should have been rejected by the tombstone check.
    // Signals from other parallel tests are rejected as "not found".
    let injections = mock_target.injections.lock().await;
    assert_eq!(
        injections.len(),
        1,
        "Exactly one signal should have been injected (to running-net). Injections: {:?}",
        *injections
    );
    assert_eq!(
        injections[0].0, "inbox",
        "Signal should have been injected into 'inbox' place"
    );

    listener_handle.abort();
    ctx.cleanup().await.ok();
}

/// E2E test: GlobalSignalListener rejects signals to cancelled nets.
#[tokio::test]
async fn test_global_signal_rejects_cancelled_net() {
    use crate::global_signal_listener::{
        GlobalSignalListener, NetResolver, SignalInjectError, SignalTarget,
    };
    use crate::net_metadata::{NetMetadata, NetStatus};
    use crate::subjects::Subjects;
    use petri_domain::{ExternalSignal, TokenColor};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[derive(Default)]
    struct MockSignalTarget {
        injections: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl SignalTarget for MockSignalTarget {
        async fn inject_signal_with_meta(
            &self,
            place_name: &str,
            _color: TokenColor,
            _reply_routing: Option<petri_domain::ReplyRouting>,
            _signal_key: Option<String>,
            _dedup_id: Option<String>,
        ) -> Result<(), SignalInjectError> {
            self.injections.lock().await.push(place_name.to_string());
            Ok(())
        }
        fn notify_eval(&self) {}
    }

    struct TombstoneResolver {
        metadata_kv: async_nats::jetstream::kv::Store,
        target: Arc<MockSignalTarget>,
    }

    #[async_trait::async_trait]
    impl NetResolver for TombstoneResolver {
        async fn resolve_net(&self, net_id: &str) -> Result<Arc<dyn SignalTarget>, String> {
            match self.metadata_kv.get(net_id).await {
                Ok(Some(entry)) => {
                    if let Ok(meta) = serde_json::from_slice::<NetMetadata>(&entry) {
                        if meta.status == NetStatus::Completed
                            || meta.status == NetStatus::Cancelled
                        {
                            return Err(format!("Net '{}' is finished", net_id));
                        }
                    }
                    Ok(self.target.clone())
                }
                _ => Err(format!("Net '{}' not found", net_id)),
            }
        }
    }

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("stream");

    let metadata_kv = create_metadata_kv(&ctx.jetstream, &ctx.prefix).await;

    // Pre-populate: cancelled net
    let meta = NetMetadata {
        net_id: "cancelled-net".to_string(),
        status: NetStatus::Cancelled,
            workspace_id: crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
        template_id: None,
        parameters: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        created_by: None,
        label: None,
        completed_at: None,
        exit_code: None,
        cancelled_at: Some(chrono::Utc::now().to_rfc3339()),
        cancelled_by: Some("admin".to_string()),
        cancel_reason: Some("manual stop".to_string()),
    };
    let value = serde_json::to_vec(&meta).unwrap();
    metadata_kv
        .put("cancelled-net", value.into())
        .await
        .expect("put");

    let mock_target = Arc::new(MockSignalTarget::default());
    let resolver = Arc::new(TombstoneResolver {
        metadata_kv: metadata_kv.clone(),
        target: mock_target.clone(),
    });

    let consumer_name = format!("gsl-test-cancelled-{}", ctx.prefix);
    let listener = Arc::new(GlobalSignalListener::with_consumer_name(
        ctx.jetstream.clone(),
        resolver,
        None,
        consumer_name,
    ));
    let listener_handle = listener.start();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Publish signal to cancelled net
    let signal = ExternalSignal {
        source: "test".to_string(),
        signal_key: "corr-cancel".to_string(),
        payload: serde_json::json!({"data": "should-be-rejected"}),
        timestamp: chrono::Utc::now(),
        dedup_id: None,
    };
    let subject = Subjects::signal_transfer(Subjects::DEFAULT_WORKSPACE, "cancelled-net", "inbox");
    let payload = serde_json::to_vec(&signal).unwrap();
    ctx.jetstream
        .publish(subject, payload.into())
        .await
        .expect("publish")
        .await
        .expect("ack");

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Signal should NOT have been injected
    let injections = mock_target.injections.lock().await;
    assert!(
        injections.is_empty(),
        "Cancelled net should NOT have received the signal. Injections: {:?}",
        *injections
    );

    listener_handle.abort();
    ctx.cleanup().await.ok();
}

// =============================================================================
// CreateNetListener NATS Publish Integration Tests
// =============================================================================

#[tokio::test]
async fn test_create_net_request_nats_publish_roundtrip() {
    use crate::create_net_listener::CreateNetRequest;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");

    // Ensure PETRI_GLOBAL stream exists
    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    // Create consumer FIRST with DeliverPolicy::New to avoid seeing stale messages
    let stream = ctx
        .jetstream
        .get_stream("PETRI_GLOBAL")
        .await
        .expect("get stream");
    let consumer = stream
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name: Some(format!("{}_create_net", ctx.prefix)),
            filter_subject: crate::subjects::Subjects::command_create_net(
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
            ),
            deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::New,
            ..Default::default()
        })
        .await
        .expect("consumer");

    let request = CreateNetRequest {
        net_id: "new-net".to_string(),
        scenario: serde_json::json!({"places": [], "transitions": []}),
        template_id: Some("template-1".to_string()),
        parameters: Some(serde_json::json!({"replicas": 3})),
        created_by: Some("test-system".to_string()),
        label: None,
        initial_tokens: None,
    };

    // Publish to NATS
    let payload = serde_json::to_vec(&request).unwrap();
    ctx.jetstream
        .publish(
            crate::subjects::Subjects::command_create_net(
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
            ),
            payload.into(),
        )
        .await
        .expect("publish")
        .await
        .expect("ack");

    // Read back
    let mut messages = consumer.messages().await.expect("messages");
    let msg = tokio::time::timeout(Duration::from_secs(5), messages.next())
        .await
        .expect("timeout")
        .expect("no msg")
        .expect("err");

    let parsed: CreateNetRequest =
        serde_json::from_slice(&msg.payload).expect("parse CreateNetRequest");
    assert_eq!(parsed.net_id, "new-net");
    assert_eq!(parsed.template_id, Some("template-1".to_string()));
    assert_eq!(parsed.created_by, Some("test-system".to_string()));

    ctx.cleanup().await.ok();
}

// =============================================================================
// CreateNetListener + NetCreator Integration Tests (initial_tokens)
// =============================================================================

/// A test NetCreator that captures received requests for assertion.
struct CapturingNetCreator {
    received: std::sync::Arc<tokio::sync::Mutex<Vec<crate::create_net_listener::CreateNetRequest>>>,
}

#[async_trait::async_trait]
impl crate::create_net_listener::NetCreator for CapturingNetCreator {
    async fn create_and_load(
        &self,
        request: &crate::create_net_listener::CreateNetRequest,
        _workspace: &str,
    ) -> Result<(), String> {
        self.received.lock().await.push(request.clone());
        Ok(())
    }
}

#[tokio::test]
async fn test_create_net_listener_delivers_initial_tokens() {
    use crate::create_net_listener::{CreateNetListener, CreateNetRequest, InitialToken};

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");

    // Ensure PETRI_GLOBAL stream
    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    // Set up capturing NetCreator
    let received = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let creator = std::sync::Arc::new(CapturingNetCreator {
        received: received.clone(),
    });

    // Start CreateNetListener
    let listener = std::sync::Arc::new(
        CreateNetListener::new(ctx.jetstream.clone(), creator)
            .with_consumer_name(format!("create-net-{}", ctx.prefix)),
    );
    let listener_handle = listener.start();

    // Give listener time to set up consumer
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Publish a CreateNetRequest WITH initial_tokens
    let request = CreateNetRequest {
        net_id: format!("child-{}", ctx.prefix),
        scenario: serde_json::json!({
            "places": [{"id": "inbox", "name": "inbox"}],
            "transitions": []
        }),
        template_id: None,
        parameters: Some(serde_json::json!({"parent_net_id": "parent-abc"})),
        created_by: Some("spawn:parent-abc".to_string()),
        label: None,
        initial_tokens: Some(vec![InitialToken {
            place_id: "inbox".to_string(),
            token: serde_json::json!({"job_id": "j1", "spec": {"model": "gpt-4"}}),
            reply_routing: None,
        }]),
    };

    let payload = serde_json::to_vec(&request).unwrap();
    ctx.jetstream
        .publish(
            crate::subjects::Subjects::command_create_net(
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
            ),
            payload.into(),
        )
        .await
        .expect("publish")
        .await
        .expect("ack");

    // Wait for listener to process our specific message (filter by net_id)
    let expected_net_id = format!("child-{}", ctx.prefix);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let requests = received.lock().await;
        if requests.iter().any(|r| r.net_id == expected_net_id) {
            break;
        }
        drop(requests);
        if tokio::time::Instant::now() > deadline {
            panic!("Timeout: CreateNetListener did not process the message");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Verify the request was delivered with initial_tokens intact
    let requests = received.lock().await;
    let req = requests
        .iter()
        .find(|r| r.net_id == expected_net_id)
        .expect("our request should be present");
    assert_eq!(req.created_by, Some("spawn:parent-abc".to_string()));

    let tokens = req
        .initial_tokens
        .as_ref()
        .expect("initial_tokens should be present");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].place_id, "inbox");
    assert_eq!(tokens[0].token["job_id"], "j1");
    assert_eq!(tokens[0].token["spec"]["model"], "gpt-4");

    // Parameters should include parent_net_id
    let params = req.parameters.as_ref().expect("parameters");
    assert_eq!(params["parent_net_id"], "parent-abc");

    listener_handle.abort();
    ctx.cleanup().await.ok();
}

#[tokio::test]
async fn test_create_net_listener_works_without_initial_tokens() {
    use crate::create_net_listener::{CreateNetListener, CreateNetRequest};

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");

    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    let received = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let creator = std::sync::Arc::new(CapturingNetCreator {
        received: received.clone(),
    });

    let listener = std::sync::Arc::new(
        CreateNetListener::new(ctx.jetstream.clone(), creator)
            .with_consumer_name(format!("create-net-{}", ctx.prefix)),
    );
    let listener_handle = listener.start();

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Publish a CreateNetRequest WITHOUT initial_tokens (backward compat)
    let request = CreateNetRequest {
        net_id: format!("standalone-{}", ctx.prefix),
        scenario: serde_json::json!({
            "places": [{"id": "start", "name": "start"}],
            "transitions": []
        }),
        template_id: Some("my-template".to_string()),
        parameters: None,
        created_by: Some("api-user".to_string()),
        label: None,
        initial_tokens: None,
    };

    let payload = serde_json::to_vec(&request).unwrap();
    ctx.jetstream
        .publish(
            crate::subjects::Subjects::command_create_net(
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
            ),
            payload.into(),
        )
        .await
        .expect("publish")
        .await
        .expect("ack");

    let expected_net_id = format!("standalone-{}", ctx.prefix);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let requests = received.lock().await;
        if requests.iter().any(|r| r.net_id == expected_net_id) {
            break;
        }
        drop(requests);
        if tokio::time::Instant::now() > deadline {
            panic!("Timeout: CreateNetListener did not process the message");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let requests = received.lock().await;
    let req = requests
        .iter()
        .find(|r| r.net_id == expected_net_id)
        .expect("our request should be present");
    assert_eq!(req.template_id, Some("my-template".to_string()));
    assert!(
        req.initial_tokens.is_none(),
        "initial_tokens should be None"
    );

    listener_handle.abort();
    ctx.cleanup().await.ok();
}

#[tokio::test]
async fn test_create_net_listener_multiple_initial_tokens() {
    use crate::create_net_listener::{CreateNetListener, CreateNetRequest, InitialToken};

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");

    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    let received = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let creator = std::sync::Arc::new(CapturingNetCreator {
        received: received.clone(),
    });

    let listener = std::sync::Arc::new(
        CreateNetListener::new(ctx.jetstream.clone(), creator)
            .with_consumer_name(format!("create-net-{}", ctx.prefix)),
    );
    let listener_handle = listener.start();

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Publish request with MULTIPLE initial tokens to different places
    let request = CreateNetRequest {
        net_id: format!("multi-token-{}", ctx.prefix),
        scenario: serde_json::json!({
            "places": [
                {"id": "inbox", "name": "inbox"},
                {"id": "config", "name": "config"}
            ],
            "transitions": []
        }),
        template_id: None,
        parameters: Some(serde_json::json!({"parent_net_id": "orchestrator"})),
        created_by: Some("spawn:orchestrator".to_string()),
        label: None,
        initial_tokens: Some(vec![
            InitialToken {
                place_id: "inbox".to_string(),
                token: serde_json::json!({"job_id": "j1"}),
                reply_routing: None,
            },
            InitialToken {
                place_id: "config".to_string(),
                token: serde_json::json!({"max_retries": 3, "timeout": 30}),
                reply_routing: None,
            },
        ]),
    };

    let payload = serde_json::to_vec(&request).unwrap();
    ctx.jetstream
        .publish(
            crate::subjects::Subjects::command_create_net(
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
            ),
            payload.into(),
        )
        .await
        .expect("publish")
        .await
        .expect("ack");

    let expected_net_id = format!("multi-token-{}", ctx.prefix);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let requests = received.lock().await;
        if requests.iter().any(|r| r.net_id == expected_net_id) {
            break;
        }
        drop(requests);
        if tokio::time::Instant::now() > deadline {
            panic!("Timeout: CreateNetListener did not process the message");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let requests = received.lock().await;
    let req = requests
        .iter()
        .find(|r| r.net_id == expected_net_id)
        .expect("our request should be present");

    let tokens = req.initial_tokens.as_ref().expect("initial_tokens");
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].place_id, "inbox");
    assert_eq!(tokens[0].token["job_id"], "j1");
    assert_eq!(tokens[1].place_id, "config");
    assert_eq!(tokens[1].token["max_retries"], 3);
    assert_eq!(tokens[1].token["timeout"], 30);

    listener_handle.abort();
    ctx.cleanup().await.ok();
}

// =============================================================================
// EventConsumer Re-hydration After Hibernation
// =============================================================================

/// Tests the full hibernate → wake → re-hydrate cycle.
///
/// 1. Create NatsEventStore + EventConsumer for a net
/// 2. Publish events (initialize, create tokens)
/// 3. Simulate hibernation: cancel the consumer, drop stores
/// 4. Re-create fresh stores + consumer for the same net_id
/// 5. Verify all events are re-hydrated from NATS
///
/// This test validates the fix for stale durable consumers:
/// without deleting the old `event-store-{net_id}` consumer, re-hydration
/// would skip all previously-acked events and the net would wake up empty.
#[tokio::test]
async fn test_event_consumer_rehydrates_after_hibernation() {
    use crate::event_consumer::EventConsumer;
    use crate::NatsConfig;
    use crate::NatsEventStore;
    use petri_application::{EventRepository, TopologyRepository};
    use petri_domain::{DomainEvent, PetriNet, Place, TokenColor};
    use petri_infrastructure::{MemoryEventStore, MemoryTopologyStore};
    use std::sync::Arc;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");

    // Ensure PETRI_GLOBAL stream exists
    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    let net_id = format!("hibernate-test-{}", ctx.prefix);

    // ── Phase 1: Create stores + consumer, publish events ──
    let shutdown = tokio_util::sync::CancellationToken::new();

    let cache1 = Arc::new(MemoryEventStore::new());
    let topo1 = Arc::new(MemoryTopologyStore::new());
    let (applied_tx1, applied_rx1) = tokio::sync::watch::channel(0u64);
    let (ready_tx1, ready_rx1) = tokio::sync::oneshot::channel();

    let consumer1 = EventConsumer::new(cache1.clone(), topo1.clone(), applied_tx1, ready_tx1);

    let js1 = ctx.jetstream.clone();
    let net_id1 = net_id.clone();
    let shutdown1 = shutdown.clone();
    tokio::spawn(async move {
        if let Err(e) = consumer1
            .start(
                &js1,
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
                &net_id1,
                shutdown1,
            )
            .await
        {
            tracing::error!(error = %e, "Consumer1 error");
        }
    });

    // Wait for hydration (no events yet, should be instant)
    tokio::time::timeout(Duration::from_secs(5), ready_rx1)
        .await
        .expect("ready timeout")
        .expect("ready rx");

    let mut config = NatsConfig::from_env();
    config.net_id = Some(net_id.clone());
    let store1 = NatsEventStore::new(cache1.clone(), ctx.jetstream.clone(), config, applied_rx1);

    // Initialize topology
    let mut net = PetriNet::new();
    let place_a = net.add_place(Place::internal("place_a"));
    let place_b = net.add_place(Place::internal("place_b"));

    store1
        .append(DomainEvent::NetInitialized { net: net.clone() })
        .await
        .expect("init");

    // Create some tokens
    store1
        .append(DomainEvent::TokenCreated {
            token: petri_domain::Token::new(TokenColor::Data(serde_json::json!({"value": 42}))),
            place_id: place_a.clone(),
            place_name: Some("place_a".to_string()),
            workflow_id: None,
            signal_key: None,
            dedup_id: None,
        })
        .await
        .expect("create token 1");

    store1
        .append(DomainEvent::TokenCreated {
            token: petri_domain::Token::new(TokenColor::Data(serde_json::json!({"value": 99}))),
            place_id: place_b.clone(),
            place_name: Some("place_b".to_string()),
            workflow_id: None,
            signal_key: None,
            dedup_id: None,
        })
        .await
        .expect("create token 2");

    // Verify we have 3 events (init + 2 tokens)
    let events_before = store1.all_events().await;
    assert_eq!(
        events_before.len(),
        3,
        "Should have 3 events before hibernation"
    );

    // ── Phase 2: Simulate hibernation — cancel consumer, drop stores ──
    shutdown.cancel();
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(store1);

    // ── Phase 3: Re-create stores + consumer (simulate wake-up) ──
    let shutdown2 = tokio_util::sync::CancellationToken::new();

    let cache2 = Arc::new(MemoryEventStore::new());
    let topo2 = Arc::new(MemoryTopologyStore::new());
    let (applied_tx2, applied_rx2) = tokio::sync::watch::channel(0u64);
    let (ready_tx2, ready_rx2) = tokio::sync::oneshot::channel();

    // Verify the cache starts empty
    assert_eq!(
        cache2.all_events().await.len(),
        0,
        "Cache should be empty before re-hydration"
    );

    let consumer2 = EventConsumer::new(cache2.clone(), topo2.clone(), applied_tx2, ready_tx2);

    let js2 = ctx.jetstream.clone();
    let net_id2 = net_id.clone();
    let shutdown2_clone = shutdown2.clone();
    tokio::spawn(async move {
        if let Err(e) = consumer2
            .start(
                &js2,
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
                &net_id2,
                shutdown2_clone,
            )
            .await
        {
            tracing::error!(error = %e, "Consumer2 error");
        }
    });

    // Wait for hydration to complete
    tokio::time::timeout(Duration::from_secs(5), ready_rx2)
        .await
        .expect("ready2 timeout")
        .expect("ready2 rx");

    // ── Phase 4: Verify all events are re-hydrated ──
    let events_after = cache2.all_events().await;
    assert_eq!(
        events_after.len(),
        3,
        "Should have all 3 events after re-hydration, got {}",
        events_after.len()
    );

    // Verify event types
    assert!(
        matches!(&events_after[0].event, DomainEvent::NetInitialized { .. }),
        "First event should be NetInitialized"
    );
    assert!(
        matches!(&events_after[1].event, DomainEvent::TokenCreated { .. }),
        "Second event should be TokenCreated"
    );
    assert!(
        matches!(&events_after[2].event, DomainEvent::TokenCreated { .. }),
        "Third event should be TokenCreated"
    );

    // Verify topology was re-hydrated
    let topology = topo2.get_topology();
    assert!(
        topology.is_some(),
        "Topology should be re-hydrated from NetInitialized event"
    );

    // Verify we can append new events after wake (store still works)
    let mut config2 = NatsConfig::from_env();
    config2.net_id = Some(net_id.clone());
    let store2 = NatsEventStore::new(cache2.clone(), ctx.jetstream.clone(), config2, applied_rx2);

    store2
        .append(DomainEvent::TokenCreated {
            token: petri_domain::Token::new(TokenColor::Data(serde_json::json!({"value": 200}))),
            place_id: place_a.clone(),
            place_name: Some("place_a".to_string()),
            workflow_id: None,
            signal_key: None,
            dedup_id: None,
        })
        .await
        .expect("create token after wake");

    let events_final = store2.all_events().await;
    assert_eq!(
        events_final.len(),
        4,
        "Should have 4 events after post-wake append"
    );

    shutdown2.cancel();
    ctx.cleanup().await.ok();
}

// =============================================================================
// Full-Cycle Integration Test: Create → Eval → Hibernate → Wake → Eval
// =============================================================================

/// Tests the complete hibernation wake-up cycle with real NATS:
///
/// 1. Create NATS-backed stores + PetriNetService
/// 2. Initialize a simple net: [signal_in] → (transform) → [result]
/// 3. Inject a token, evaluate → token moves to [result]
/// 4. Hibernate: cancel consumer, drop all stores + service
/// 5. Re-create fresh stores + consumer (EventConsumer re-hydrates from NATS)
/// 6. Build new PetriNetService with re-hydrated stores
/// 7. Verify topology restored (→ RunMode::Running in production)
/// 8. Verify marking correct (token in [result] from first eval)
/// 9. Inject another token on [signal_in] (simulating signal after wake)
/// 10. Evaluate → transition fires again
/// 11. Verify 2 tokens in [result]
#[tokio::test]
async fn test_full_cycle_create_hibernate_wake_eval() {
    use crate::event_consumer::EventConsumer;
    use crate::NatsConfig;
    use crate::NatsEventStore;
    use petri_application::{EventRepository, PetriNetService, TopologyRepository};
    use petri_domain::{
        Arc as PetriArc, DomainEvent, PetriNet, Place, Port, TokenColor, Transition,
    };
    use petri_infrastructure::{MarkingProjection, MemoryEventStore, MemoryTopologyStore};
    use std::sync::Arc;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");

    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    let net_id = format!("full-cycle-{}", ctx.prefix);

    // ── Build topology: [signal_in] → (transform) → [result] ──
    let mut net = PetriNet::new();
    let signal_in_id = net.add_place(Place::signal("signal_in"));
    let result_id = net.add_place(Place::internal("result"));

    let transform = Transition::new("transform", "#{output: input}")
        .with_input_port(Port::new("input"))
        .with_output_port(Port::new("output"));
    let transform_id = net.add_transition(transform);

    net.add_arc(PetriArc::input(
        signal_in_id.clone(),
        transform_id.clone(),
        "input",
    ));
    net.add_arc(PetriArc::output(
        transform_id.clone(),
        "output",
        result_id.clone(),
    ));

    // ── Phase 1: Create stores, initialize net, inject token, evaluate ──
    let shutdown1 = tokio_util::sync::CancellationToken::new();
    let cache1 = Arc::new(MemoryEventStore::new());
    let topo1 = Arc::new(MemoryTopologyStore::new());
    let (applied_tx1, applied_rx1) = tokio::sync::watch::channel(0u64);
    let (ready_tx1, ready_rx1) = tokio::sync::oneshot::channel();

    let consumer1 = EventConsumer::new(cache1.clone(), topo1.clone(), applied_tx1, ready_tx1);
    let js1 = ctx.jetstream.clone();
    let net_id1 = net_id.clone();
    let shutdown1_clone = shutdown1.clone();
    tokio::spawn(async move {
        if let Err(e) = consumer1
            .start(
                &js1,
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
                &net_id1,
                shutdown1_clone,
            )
            .await
        {
            tracing::error!(error = %e, "Consumer1 error");
        }
    });

    tokio::time::timeout(Duration::from_secs(5), ready_rx1)
        .await
        .expect("ready1 timeout")
        .expect("ready1 rx");

    let mut config1 = NatsConfig::from_env();
    config1.net_id = Some(net_id.clone());
    let event_store1 = Arc::new(NatsEventStore::new(
        cache1.clone(),
        ctx.jetstream.clone(),
        config1,
        applied_rx1,
    ));

    let service1 = Arc::new(PetriNetService::new(
        event_store1.clone(),
        topo1.clone(),
        Arc::new(MarkingProjection::new()),
    ));

    // Initialize topology
    service1
        .append_event(DomainEvent::NetInitialized { net: net.clone() })
        .await
        .expect("init");

    // Inject a token on signal_in
    service1
        .create_token(
            signal_in_id.clone(),
            TokenColor::Data(serde_json::json!({"job": "alpha"})),
        )
        .await
        .expect("create token 1");

    // Evaluate — transition should fire, moving token to result
    let eval_result = service1.evaluate_until_quiescent(100).await.expect("eval");
    assert!(
        eval_result.steps_executed > 0,
        "Transition should have fired, steps={}",
        eval_result.steps_executed
    );

    // Verify marking: signal_in empty, result has 1 token
    let marking1 = service1.get_marking().await;
    assert_eq!(
        marking1.tokens_at(&signal_in_id).len(),
        0,
        "signal_in should be empty after eval"
    );
    assert_eq!(
        marking1.tokens_at(&result_id).len(),
        1,
        "result should have 1 token after eval"
    );

    let events_before = service1.get_events().await;
    let event_count_before = events_before.len();
    assert!(
        event_count_before >= 3,
        "Should have at least 3 events (init + token_created + transition_fired), got {}",
        event_count_before
    );

    // ── Phase 2: Hibernate — cancel consumer, drop everything ──
    shutdown1.cancel();
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(service1);
    drop(event_store1);

    // ── Phase 3: Wake — re-create stores + consumer from NATS ──
    let shutdown2 = tokio_util::sync::CancellationToken::new();
    let cache2 = Arc::new(MemoryEventStore::new());
    let topo2 = Arc::new(MemoryTopologyStore::new());
    let (applied_tx2, applied_rx2) = tokio::sync::watch::channel(0u64);
    let (ready_tx2, ready_rx2) = tokio::sync::oneshot::channel();

    assert!(
        topo2.get_topology().is_none(),
        "Topology should be empty before re-hydration"
    );

    let consumer2 = EventConsumer::new(cache2.clone(), topo2.clone(), applied_tx2, ready_tx2);
    let js2 = ctx.jetstream.clone();
    let net_id2 = net_id.clone();
    let shutdown2_clone = shutdown2.clone();
    tokio::spawn(async move {
        if let Err(e) = consumer2
            .start(
                &js2,
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
                &net_id2,
                shutdown2_clone,
            )
            .await
        {
            tracing::error!(error = %e, "Consumer2 error");
        }
    });

    tokio::time::timeout(Duration::from_secs(5), ready_rx2)
        .await
        .expect("ready2 timeout")
        .expect("ready2 rx");

    // ── Phase 4: Verify hydration ──
    // Topology should be restored from NetInitialized event
    assert!(
        topo2.get_topology().is_some(),
        "Topology should be restored after re-hydration (→ RunMode::Running in production)"
    );

    // All events should be present in the cache
    let events_after = cache2.all_events().await;
    assert_eq!(
        events_after.len(),
        event_count_before,
        "All {} events should be re-hydrated, got {}",
        event_count_before,
        events_after.len()
    );

    // Build new PetriNetService with hydrated stores
    let mut config2 = NatsConfig::from_env();
    config2.net_id = Some(net_id.clone());
    let event_store2 = Arc::new(NatsEventStore::new(
        cache2.clone(),
        ctx.jetstream.clone(),
        config2,
        applied_rx2,
    ));

    let service2 = Arc::new(PetriNetService::new(
        event_store2.clone(),
        topo2.clone(),
        Arc::new(MarkingProjection::new()),
    ));

    // Verify marking is correct after hydration
    let marking2 = service2.get_marking().await;
    assert_eq!(
        marking2.tokens_at(&signal_in_id).len(),
        0,
        "signal_in should still be empty after hydration"
    );
    assert_eq!(
        marking2.tokens_at(&result_id).len(),
        1,
        "result should still have 1 token after hydration"
    );

    // ── Phase 5: Post-wake signal injection + evaluation ──
    // Simulate an external signal arriving (e.g., human result / adapter signal)
    service2
        .create_token(
            signal_in_id.clone(),
            TokenColor::Data(serde_json::json!({"job": "beta"})),
        )
        .await
        .expect("create token after wake");

    // Evaluate — transition should fire again
    let eval_result2 = service2.evaluate_until_quiescent(100).await.expect("eval2");
    assert!(
        eval_result2.steps_executed > 0,
        "Transition should have fired after wake, steps={}",
        eval_result2.steps_executed
    );

    // Verify final marking: 2 tokens in result (one from before hibernation, one after)
    let marking_final = service2.get_marking().await;
    assert_eq!(
        marking_final.tokens_at(&signal_in_id).len(),
        0,
        "signal_in should be empty after post-wake eval"
    );
    assert_eq!(
        marking_final.tokens_at(&result_id).len(),
        2,
        "result should have 2 tokens (1 pre-hibernate + 1 post-wake)"
    );

    shutdown2.cancel();
    ctx.cleanup().await.ok();
}

// =============================================================================
// Ephemeral EventConsumer Tests (Fix 1)
// =============================================================================

/// Test A: Two sequential EventConsumer instances (simulating hibernate → wake)
/// don't conflict. With ephemeral consumers, the second consumer simply creates
/// a new ephemeral consumer — no "consumer deleted" race condition.
#[tokio::test]
async fn test_event_consumer_ephemeral_no_conflict() {
    use crate::event_consumer::EventConsumer;
    use crate::NatsConfig;
    use crate::NatsEventStore;
    use petri_application::{EventRepository, TopologyRepository};
    use petri_domain::{DomainEvent, PetriNet, Place, TokenColor};
    use petri_infrastructure::MemoryEventStore;
    use petri_infrastructure::MemoryTopologyStore;
    use std::sync::Arc;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");

    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    let net_id = format!("ephemeral-noconflict-{}", ctx.prefix);

    // ── Phase 1: Start consumer #1, publish 3 events ──
    let shutdown1 = tokio_util::sync::CancellationToken::new();
    let cache1 = Arc::new(MemoryEventStore::new());
    let topo1 = Arc::new(MemoryTopologyStore::new());
    let (applied_tx1, applied_rx1) = tokio::sync::watch::channel(0u64);
    let (ready_tx1, ready_rx1) = tokio::sync::oneshot::channel();

    let consumer1 = EventConsumer::new(cache1.clone(), topo1.clone(), applied_tx1, ready_tx1);
    let js1 = ctx.jetstream.clone();
    let net_id1 = net_id.clone();
    let shutdown1_clone = shutdown1.clone();
    let handle1 =
        tokio::spawn(async move {
            consumer1
                .start(
                    &js1,
                    crate::subjects::Subjects::DEFAULT_WORKSPACE,
                    &net_id1,
                    shutdown1_clone,
                )
                .await
        });

    tokio::time::timeout(Duration::from_secs(5), ready_rx1)
        .await
        .expect("ready1 timeout")
        .expect("ready1 rx");

    let mut config1 = NatsConfig::from_env();
    config1.net_id = Some(net_id.clone());
    let store1 = NatsEventStore::new(cache1.clone(), ctx.jetstream.clone(), config1, applied_rx1);

    let mut net = PetriNet::new();
    let place_a = net.add_place(Place::internal("place_a"));

    store1
        .append(DomainEvent::NetInitialized { net: net.clone() })
        .await
        .expect("init");
    store1
        .append(DomainEvent::TokenCreated {
            token: petri_domain::Token::new(TokenColor::Data(serde_json::json!({"v": 1}))),
            place_id: place_a.clone(),
            place_name: Some("place_a".to_string()),
            workflow_id: None,
            signal_key: None,
            dedup_id: None,
        })
        .await
        .expect("token 1");
    store1
        .append(DomainEvent::TokenCreated {
            token: petri_domain::Token::new(TokenColor::Data(serde_json::json!({"v": 2}))),
            place_id: place_a.clone(),
            place_name: Some("place_a".to_string()),
            workflow_id: None,
            signal_key: None,
            dedup_id: None,
        })
        .await
        .expect("token 2");

    assert_eq!(cache1.all_events().await.len(), 3);

    // ── Phase 2: Simulate hibernation — cancel consumer #1 ──
    shutdown1.cancel();
    // Wait for consumer task to finish cleanly (should NOT error)
    let result1 = tokio::time::timeout(Duration::from_secs(5), handle1)
        .await
        .expect("consumer1 join timeout")
        .expect("consumer1 join");
    assert!(
        result1.is_ok(),
        "Consumer #1 should stop cleanly, got: {:?}",
        result1.err()
    );

    drop(store1);
    tokio::time::sleep(Duration::from_millis(200)).await;

    // ── Phase 3: Start consumer #2 for same net_id ──
    let shutdown2 = tokio_util::sync::CancellationToken::new();
    let cache2 = Arc::new(MemoryEventStore::new());
    let topo2 = Arc::new(MemoryTopologyStore::new());
    let (applied_tx2, _applied_rx2) = tokio::sync::watch::channel(0u64);
    let (ready_tx2, ready_rx2) = tokio::sync::oneshot::channel();

    assert_eq!(
        cache2.all_events().await.len(),
        0,
        "Cache2 should start empty"
    );

    let consumer2 = EventConsumer::new(cache2.clone(), topo2.clone(), applied_tx2, ready_tx2);
    let js2 = ctx.jetstream.clone();
    let net_id2 = net_id.clone();
    let shutdown2_clone = shutdown2.clone();
    let handle2 =
        tokio::spawn(async move {
            consumer2
                .start(
                    &js2,
                    crate::subjects::Subjects::DEFAULT_WORKSPACE,
                    &net_id2,
                    shutdown2_clone,
                )
                .await
        });

    // Wait for hydration — should succeed with all 3 events
    tokio::time::timeout(Duration::from_secs(5), ready_rx2)
        .await
        .expect("ready2 timeout")
        .expect("ready2 rx");

    let events_after = cache2.all_events().await;
    assert_eq!(
        events_after.len(),
        3,
        "Consumer #2 should re-hydrate all 3 events, got {}",
        events_after.len()
    );

    // Verify topology was re-hydrated
    assert!(
        topo2.get_topology().is_some(),
        "Topology should be restored from NetInitialized event"
    );

    // Clean shutdown — consumer #2 should NOT error
    shutdown2.cancel();
    let result2 = tokio::time::timeout(Duration::from_secs(5), handle2)
        .await
        .expect("consumer2 join timeout")
        .expect("consumer2 join");
    assert!(
        result2.is_ok(),
        "Consumer #2 should stop cleanly, got: {:?}",
        result2.err()
    );

    ctx.cleanup().await.ok();
}

/// Test B: Ephemeral consumers for different nets don't interfere.
/// Each consumer only hydrates events for its own net_id.
#[tokio::test]
async fn test_event_consumer_concurrent_nets_no_conflict() {
    use crate::event_consumer::EventConsumer;
    use crate::NatsConfig;
    use crate::NatsEventStore;
    use petri_application::EventRepository;
    use petri_domain::{DomainEvent, PetriNet, Place, TokenColor};
    use petri_infrastructure::MemoryEventStore;
    use petri_infrastructure::MemoryTopologyStore;
    use std::sync::Arc;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");

    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    let net_a = format!("concurrent-a-{}", ctx.prefix);
    let net_b = format!("concurrent-b-{}", ctx.prefix);

    // ── Publish events for net_a (2 events) ──
    {
        let shutdown = tokio_util::sync::CancellationToken::new();
        let cache = Arc::new(MemoryEventStore::new());
        let topo = Arc::new(MemoryTopologyStore::new());
        let (applied_tx, applied_rx) = tokio::sync::watch::channel(0u64);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

        let consumer = EventConsumer::new(cache.clone(), topo.clone(), applied_tx, ready_tx);
        let js = ctx.jetstream.clone();
        let nid = net_a.clone();
        let s = shutdown.clone();
        tokio::spawn(async move {
            consumer
                .start(&js, crate::subjects::Subjects::DEFAULT_WORKSPACE, &nid, s)
                .await
        });

        tokio::time::timeout(Duration::from_secs(5), ready_rx)
            .await
            .expect("timeout")
            .expect("rx");

        let mut config = NatsConfig::from_env();
        config.net_id = Some(net_a.clone());
        let store = NatsEventStore::new(cache.clone(), ctx.jetstream.clone(), config, applied_rx);

        let mut net = PetriNet::new();
        let p = net.add_place(Place::internal("p"));
        store
            .append(DomainEvent::NetInitialized { net })
            .await
            .expect("init a");
        store
            .append(DomainEvent::TokenCreated {
                token: petri_domain::Token::new(TokenColor::Data(serde_json::json!({"net": "a"}))),
                place_id: p,
                place_name: Some("p".to_string()),
                workflow_id: None,
                signal_key: None,
                dedup_id: None,
            })
            .await
            .expect("token a");

        shutdown.cancel();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // ── Publish events for net_b (3 events) ──
    {
        let shutdown = tokio_util::sync::CancellationToken::new();
        let cache = Arc::new(MemoryEventStore::new());
        let topo = Arc::new(MemoryTopologyStore::new());
        let (applied_tx, applied_rx) = tokio::sync::watch::channel(0u64);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

        let consumer = EventConsumer::new(cache.clone(), topo.clone(), applied_tx, ready_tx);
        let js = ctx.jetstream.clone();
        let nid = net_b.clone();
        let s = shutdown.clone();
        tokio::spawn(async move {
            consumer
                .start(&js, crate::subjects::Subjects::DEFAULT_WORKSPACE, &nid, s)
                .await
        });

        tokio::time::timeout(Duration::from_secs(5), ready_rx)
            .await
            .expect("timeout")
            .expect("rx");

        let mut config = NatsConfig::from_env();
        config.net_id = Some(net_b.clone());
        let store = NatsEventStore::new(cache.clone(), ctx.jetstream.clone(), config, applied_rx);

        let mut net = PetriNet::new();
        let p = net.add_place(Place::internal("p"));
        store
            .append(DomainEvent::NetInitialized { net })
            .await
            .expect("init b");
        store
            .append(DomainEvent::TokenCreated {
                token: petri_domain::Token::new(TokenColor::Data(
                    serde_json::json!({"net": "b", "i": 1}),
                )),
                place_id: p.clone(),
                place_name: Some("p".to_string()),
                workflow_id: None,
                signal_key: None,
                dedup_id: None,
            })
            .await
            .expect("token b1");
        store
            .append(DomainEvent::TokenCreated {
                token: petri_domain::Token::new(TokenColor::Data(
                    serde_json::json!({"net": "b", "i": 2}),
                )),
                place_id: p,
                place_name: Some("p".to_string()),
                workflow_id: None,
                signal_key: None,
                dedup_id: None,
            })
            .await
            .expect("token b2");

        shutdown.cancel();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // ── Start concurrent consumers for both nets ──
    let shutdown_a = tokio_util::sync::CancellationToken::new();
    let cache_a = Arc::new(MemoryEventStore::new());
    let topo_a = Arc::new(MemoryTopologyStore::new());
    let (applied_tx_a, _rx_a) = tokio::sync::watch::channel(0u64);
    let (ready_tx_a, ready_rx_a) = tokio::sync::oneshot::channel();

    let shutdown_b = tokio_util::sync::CancellationToken::new();
    let cache_b = Arc::new(MemoryEventStore::new());
    let topo_b = Arc::new(MemoryTopologyStore::new());
    let (applied_tx_b, _rx_b) = tokio::sync::watch::channel(0u64);
    let (ready_tx_b, ready_rx_b) = tokio::sync::oneshot::channel();

    let consumer_a = EventConsumer::new(cache_a.clone(), topo_a.clone(), applied_tx_a, ready_tx_a);
    let consumer_b = EventConsumer::new(cache_b.clone(), topo_b.clone(), applied_tx_b, ready_tx_b);

    let js_a = ctx.jetstream.clone();
    let nid_a = net_a.clone();
    let sa = shutdown_a.clone();
    tokio::spawn(async move {
        consumer_a
            .start(
                &js_a,
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
                &nid_a,
                sa,
            )
            .await
    });

    let js_b = ctx.jetstream.clone();
    let nid_b = net_b.clone();
    let sb = shutdown_b.clone();
    tokio::spawn(async move {
        consumer_b
            .start(
                &js_b,
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
                &nid_b,
                sb,
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(5), ready_rx_a)
        .await
        .expect("ready_a timeout")
        .expect("ready_a rx");
    tokio::time::timeout(Duration::from_secs(5), ready_rx_b)
        .await
        .expect("ready_b timeout")
        .expect("ready_b rx");

    // ── Verify each hydrated only its own events ──
    let events_a = cache_a.all_events().await;
    let events_b = cache_b.all_events().await;

    assert_eq!(
        events_a.len(),
        2,
        "Net A should have 2 events (init + 1 token), got {}",
        events_a.len()
    );
    assert_eq!(
        events_b.len(),
        3,
        "Net B should have 3 events (init + 2 tokens), got {}",
        events_b.len()
    );

    shutdown_a.cancel();
    shutdown_b.cancel();
    ctx.cleanup().await.ok();
}

// =============================================================================
// Global Bridge Listener Tests (Fix 2)
// =============================================================================

/// Test C: GlobalBridgeListener delivers bridge tokens published during downtime.
/// The durable consumer survives restarts, so messages are not lost.
///
/// 1. Start listener #1, deliver a message, stop it
/// 2. Publish a message while the listener is DOWN
/// 3. Start listener #2 with the same consumer name
/// 4. Verify the "during-downtime" message is delivered (no gap)
/// 5. Publish another message, verify it's also delivered
#[tokio::test]
async fn test_global_bridge_listener_no_message_gap_on_restart() {
    use crate::cross_net_bridge::CrossNetTokenTransfer;
    use crate::global_bridge_listener::{
        BridgeInjectError, BridgeResolveError, BridgeResolver, BridgeTarget, GlobalBridgeListener,
    };
    use crate::subjects::Subjects;
    use petri_domain::{ReplyRouting, TokenColor};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");

    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    let consumer_name = format!("test-bridge-{}", ctx.prefix);
    let target_net_id = format!("bridge-target-{}", ctx.prefix);

    // ── Mock infrastructure ──
    #[derive(Clone)]
    struct MockBridgeTarget {
        injections: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    }

    #[async_trait::async_trait]
    impl BridgeTarget for MockBridgeTarget {
        async fn inject_bridge_token(
            &self,
            place_name: &str,
            color: TokenColor,
            _reply_routing: Option<ReplyRouting>,
            _signal_key: Option<String>,
            _dedup_id: Option<String>,
        ) -> Result<(), BridgeInjectError> {
            let data = match &color {
                TokenColor::Data(v) => v.clone(),
                _ => serde_json::json!(null),
            };
            self.injections
                .lock()
                .await
                .push((place_name.to_string(), data));
            Ok(())
        }

        fn notify_eval(&self) {}
    }

    let injections = Arc::new(Mutex::new(Vec::<(String, serde_json::Value)>::new()));
    let mock_target = Arc::new(MockBridgeTarget {
        injections: injections.clone(),
    });

    struct MockBridgeResolver {
        target: Arc<MockBridgeTarget>,
    }

    #[async_trait::async_trait]
    impl BridgeResolver for MockBridgeResolver {
        async fn resolve_net(
            &self,
            _net_id: &str,
        ) -> Result<Arc<dyn BridgeTarget>, BridgeResolveError> {
            Ok(self.target.clone())
        }
    }

    let resolver = Arc::new(MockBridgeResolver {
        target: mock_target.clone(),
    });

    // ── Phase 1: Start listener #1, deliver a message ──
    let listener1 = Arc::new(GlobalBridgeListener::with_consumer_name(
        ctx.jetstream.clone(),
        resolver.clone(),
        None,
        consumer_name.clone(),
    ));
    let handle1 = listener1.start();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let transfer1 = CrossNetTokenTransfer {
        source_net_id: "source-net".to_string(),
        source_place_name: "outbox".to_string(),
        token_color: serde_json::json!({"value": "first"}),
        signal_key: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        reply_to: None,
        reply_channels: None,
        dedup_id: None,
    };

    let subject =
        Subjects::bridge_transfer(Subjects::DEFAULT_WORKSPACE, &target_net_id, "inbox");
    ctx.jetstream
        .publish(
            subject.clone(),
            serde_json::to_vec(&transfer1).unwrap().into(),
        )
        .await
        .expect("publish 1")
        .await
        .expect("ack 1");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !injections.lock().await.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("Delivery 1 timed out");

    assert_eq!(injections.lock().await.len(), 1);

    // ── Phase 2: Stop listener #1 (simulates engine shutdown) ──
    handle1.abort();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // ── Phase 3: Publish a message while the listener is DOWN ──
    let transfer2 = CrossNetTokenTransfer {
        source_net_id: "source-net".to_string(),
        source_place_name: "outbox".to_string(),
        token_color: serde_json::json!({"value": "during-downtime"}),
        signal_key: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        reply_to: None,
        reply_channels: None,
        dedup_id: None,
    };

    ctx.jetstream
        .publish(
            subject.clone(),
            serde_json::to_vec(&transfer2).unwrap().into(),
        )
        .await
        .expect("publish during downtime")
        .await
        .expect("ack during downtime");

    // ── Phase 4: Start listener #2 — durable consumer resumes, no gap ──
    let listener2 = Arc::new(GlobalBridgeListener::with_consumer_name(
        ctx.jetstream.clone(),
        resolver.clone(),
        None,
        consumer_name.clone(),
    ));
    let handle2 = listener2.start();

    // The "during-downtime" message should be delivered from the durable consumer's backlog
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if injections.lock().await.len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("During-downtime message was NOT delivered — durable consumer lost it");

    assert_eq!(
        injections.lock().await[1].1,
        serde_json::json!({"value": "during-downtime"}),
        "Second injection should be the during-downtime message"
    );

    // ── Phase 5: Publish one more after restart, verify it's also delivered ──
    let transfer3 = CrossNetTokenTransfer {
        source_net_id: "source-net".to_string(),
        source_place_name: "outbox".to_string(),
        token_color: serde_json::json!({"value": "after-restart"}),
        signal_key: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        reply_to: None,
        reply_channels: None,
        dedup_id: None,
    };

    ctx.jetstream
        .publish(subject, serde_json::to_vec(&transfer3).unwrap().into())
        .await
        .expect("publish 3")
        .await
        .expect("ack 3");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if injections.lock().await.len() >= 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("Post-restart message delivery timed out");

    let final_injections = injections.lock().await;
    assert_eq!(final_injections.len(), 3, "Should have 3 injections total");
    assert_eq!(
        final_injections[2].1,
        serde_json::json!({"value": "after-restart"})
    );

    handle2.abort();
    ctx.cleanup().await.ok();
}

/// Test D: Idempotent durable consumer creation — starting the listener
/// twice with the same consumer name reuses the existing consumer.
/// Verifies create_consumer is idempotent for matching configs.
#[tokio::test]
async fn test_global_bridge_listener_idempotent_consumer_creation() {
    use crate::global_bridge_listener::{
        BridgeInjectError, BridgeResolveError, BridgeResolver, BridgeTarget, GlobalBridgeListener,
    };
    use petri_domain::{ReplyRouting, TokenColor};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");

    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    let consumer_name = format!("test-idempotent-bridge-{}", ctx.prefix);

    // ── Minimal mock ──
    let injections = Arc::new(Mutex::new(Vec::<(String, serde_json::Value)>::new()));

    #[derive(Clone)]
    struct MockTarget {
        injections: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    }

    #[async_trait::async_trait]
    impl BridgeTarget for MockTarget {
        async fn inject_bridge_token(
            &self,
            place_name: &str,
            color: TokenColor,
            _reply_routing: Option<ReplyRouting>,
            _signal_key: Option<String>,
            _dedup_id: Option<String>,
        ) -> Result<(), BridgeInjectError> {
            let data = match &color {
                TokenColor::Data(v) => v.clone(),
                _ => serde_json::json!(null),
            };
            self.injections
                .lock()
                .await
                .push((place_name.to_string(), data));
            Ok(())
        }
        fn notify_eval(&self) {}
    }

    let mock_target = Arc::new(MockTarget {
        injections: injections.clone(),
    });

    struct MockResolver {
        target: Arc<MockTarget>,
    }

    #[async_trait::async_trait]
    impl BridgeResolver for MockResolver {
        async fn resolve_net(
            &self,
            _net_id: &str,
        ) -> Result<Arc<dyn BridgeTarget>, BridgeResolveError> {
            Ok(self.target.clone())
        }
    }

    let resolver = Arc::new(MockResolver {
        target: mock_target.clone(),
    });

    // ── Start listener #1, let it create the durable consumer ──
    let listener1 = Arc::new(GlobalBridgeListener::with_consumer_name(
        ctx.jetstream.clone(),
        resolver.clone(),
        None,
        consumer_name.clone(),
    ));
    let handle1 = listener1.start();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify the durable consumer exists
    let stream = ctx
        .jetstream
        .get_or_create_stream(crate::stream_config())
        .await
        .expect("get stream");
    let info1 = stream
        .consumer_info(&consumer_name)
        .await
        .expect("consumer should exist after listener #1 start");
    let created_ts = info1.created;

    // ── Stop listener #1 ──
    handle1.abort();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // ── Start listener #2 with same name — should reuse consumer (idempotent) ──
    let listener2 = Arc::new(GlobalBridgeListener::with_consumer_name(
        ctx.jetstream.clone(),
        resolver.clone(),
        None,
        consumer_name.clone(),
    ));
    let handle2 = listener2.start();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify same consumer was reused (created timestamp unchanged)
    let info2 = stream
        .consumer_info(&consumer_name)
        .await
        .expect("consumer should still exist after listener #2 start");
    assert_eq!(
        created_ts, info2.created,
        "Consumer should be reused (same created timestamp), not recreated"
    );

    handle2.abort();
    ctx.cleanup().await.ok();
}

// =============================================================================
// Net Metadata Discovery Test (Fix 3)
// =============================================================================

/// Test E: KV_NET_METADATA can be used to discover nets across lifecycle states.
/// Simulates the cross-reference between KV metadata and an in-memory set.
#[tokio::test]
async fn test_net_metadata_discovery_across_lifecycle() {
    use crate::net_metadata::{NetMetadata, NetStatus, METADATA_KV_BUCKET};

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");

    // Create a unique KV bucket for this test
    let bucket_name = format!(
        "{}_{}",
        METADATA_KV_BUCKET,
        ctx.prefix.replace('-', "_").to_uppercase()
    );

    let kv = ctx
        .jetstream
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: bucket_name.clone(),
            history: 1,
            ..Default::default()
        })
        .await
        .expect("create kv bucket");

    // ── Pre-populate metadata for nets in different lifecycle states ──
    let nets = vec![
        ("orchestrator-net", NetStatus::Running),
        ("job-net-1", NetStatus::Running),
        ("job-net-2", NetStatus::Created),
        ("old-net", NetStatus::Completed),
        ("cancelled-net", NetStatus::Cancelled),
    ];

    for (net_id, status) in &nets {
        let metadata = NetMetadata {
            net_id: net_id.to_string(),
            status: status.clone(),
            workspace_id: crate::subjects::Subjects::DEFAULT_WORKSPACE.to_string(),
            template_id: None,
            parameters: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            created_by: None,
            label: None,
            completed_at: None,
            exit_code: None,
            cancelled_at: None,
            cancelled_by: None,
            cancel_reason: None,
        };
        kv.put(net_id, serde_json::to_vec(&metadata).unwrap().into())
            .await
            .unwrap_or_else(|_| panic!("put metadata for {}", net_id));
    }

    // ── Read all entries from KV and deserialize ──
    let mut discovered: Vec<NetMetadata> = Vec::new();
    let mut keys = kv.keys().await.expect("list keys");
    while let Some(key) = keys.next().await {
        let key = key.expect("key");
        if let Some(entry) = kv.get(&key).await.expect("get entry") {
            let metadata: NetMetadata =
                serde_json::from_slice(&entry).expect("deserialize metadata");
            discovered.push(metadata);
        }
    }

    assert_eq!(discovered.len(), 5, "Should discover all 5 nets from KV");

    // ── Filter to active nets (running + created) — what the UI should show ──
    let active_nets: Vec<&NetMetadata> = discovered
        .iter()
        .filter(|m| m.status == NetStatus::Running || m.status == NetStatus::Created)
        .collect();

    assert_eq!(
        active_nets.len(),
        3,
        "Should have 3 active nets (2 running + 1 created)"
    );

    let active_ids: Vec<&str> = active_nets.iter().map(|m| m.net_id.as_str()).collect();
    assert!(active_ids.contains(&"orchestrator-net"));
    assert!(active_ids.contains(&"job-net-1"));
    assert!(active_ids.contains(&"job-net-2"));
    assert!(!active_ids.contains(&"old-net"));
    assert!(!active_ids.contains(&"cancelled-net"));

    // ── Cross-reference with in-memory set (simulate hot vs hibernated) ──
    let in_memory: std::collections::HashSet<&str> = vec!["orchestrator-net"].into_iter().collect();

    for net in &active_nets {
        let is_hot = in_memory.contains(net.net_id.as_str());
        match net.net_id.as_str() {
            "orchestrator-net" => assert!(is_hot, "orchestrator-net should be in memory (hot)"),
            "job-net-1" => assert!(!is_hot, "job-net-1 should NOT be in memory (hibernated)"),
            "job-net-2" => assert!(
                !is_hot,
                "job-net-2 should NOT be in memory (created, not yet loaded)"
            ),
            other => panic!("Unexpected active net: {}", other),
        }
    }

    ctx.cleanup().await.ok();
}

// =============================================================================
// DLQ / Message Loop Error Semantics Integration Tests
// =============================================================================

mod dlq_tests {
    use super::*;
    use crate::dlq::{dlq_stream_config, DlqEntry, DlqErrorClass, DlqPublisher};
    use crate::message_loop::{run_message_loop_cancellable, MessageHandler, ProcessError};
    use async_nats::jetstream::consumer::pull::Config as PullConfig;
    use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy, PullConsumer};
    use petri_api_types::subjects::Subjects;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    enum FailKind {
        Parse,
        Business,
        Internal,
    }

    /// Handler that fails every message with a fixed error class.
    struct FailingHandler {
        name: String,
        kind: FailKind,
        deliveries: Arc<AtomicU64>,
    }

    #[async_trait::async_trait]
    impl MessageHandler for FailingHandler {
        fn listener_name(&self) -> &str {
            &self.name
        }

        async fn process_message(
            &self,
            _msg: &async_nats::jetstream::Message,
        ) -> Result<(), ProcessError> {
            self.deliveries.fetch_add(1, Ordering::SeqCst);
            match self.kind {
                FailKind::Parse => Err(ProcessError::Parse("unparseable".to_string())),
                FailKind::Business => Err(ProcessError::Business("rejected".to_string())),
                FailKind::Internal => Err(ProcessError::Internal("boom".to_string())),
            }
        }
    }

    /// DeliverPolicy::New consumer on the PETRI_DLQ stream for one error class.
    /// Created BEFORE the message loop runs so only this test's entries arrive.
    async fn create_dlq_consumer(ctx: &NatsTestContext, class: &str) -> PullConsumer {
        let stream = ctx
            .jetstream
            .get_or_create_stream(dlq_stream_config())
            .await
            .expect("ensure DLQ stream");
        stream
            .create_consumer(PullConfig {
                durable_name: Some(format!("dlq_{}_{}", class, ctx.prefix)),
                filter_subject: Subjects::dlq_subject(Subjects::DEFAULT_WORKSPACE, class),
                deliver_policy: DeliverPolicy::New,
                ack_policy: AckPolicy::Explicit,
                ..Default::default()
            })
            .await
            .expect("create DLQ consumer")
    }

    /// Read DLQ entries until one from `listener` arrives (other tests'
    /// listeners share the stream; the class filter plus the unique
    /// listener name isolate this test's entry).
    async fn wait_for_dlq_entry(consumer: PullConsumer, listener: &str) -> DlqEntry {
        let mut messages = consumer.messages().await.expect("DLQ message stream");
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(30), messages.next())
                .await
                .expect("timed out waiting for DLQ entry")
                .expect("DLQ stream ended")
                .expect("DLQ message error");
            let entry: DlqEntry = serde_json::from_slice(&msg.payload).expect("parse DlqEntry");
            msg.ack().await.ok();
            if entry.listener == listener {
                return entry;
            }
        }
    }

    /// Poll the loop's command consumer until everything published is ACKed.
    async fn assert_fully_acked(ctx: &NatsTestContext, consumer_name: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let stream = ctx
                .jetstream
                .get_stream(&ctx.commands_stream)
                .await
                .expect("get commands stream");
            let mut consumer: PullConsumer = stream
                .get_consumer(&format!("{}_{}", ctx.prefix, consumer_name))
                .await
                .expect("get loop consumer");
            let info = consumer.info().await.expect("consumer info");
            if info.num_ack_pending == 0 && info.num_pending == 0 {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "message was never ACKed: ack_pending={} pending={}",
                info.num_ack_pending,
                info.num_pending
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    struct LoopUnderTest {
        cancel: CancellationToken,
        handle: tokio::task::JoinHandle<()>,
        deliveries: Arc<AtomicU64>,
        listener_name: String,
    }

    /// Spawn the message loop with a failing handler + real DLQ publisher.
    async fn spawn_failing_loop(
        ctx: &NatsTestContext,
        name: &str,
        kind: FailKind,
    ) -> LoopUnderTest {
        let consumer = ctx
            .create_commands_consumer(name)
            .await
            .expect("create loop consumer");
        let deliveries = Arc::new(AtomicU64::new(0));
        let listener_name = format!("{}-{}", name, ctx.prefix);
        let handler = FailingHandler {
            name: listener_name.clone(),
            kind,
            deliveries: deliveries.clone(),
        };
        let cancel = CancellationToken::new();
        let dlq = DlqPublisher::new(ctx.jetstream.clone());
        let loop_cancel = cancel.clone();
        let handle = tokio::spawn(async move {
            let _ = run_message_loop_cancellable(consumer, &handler, Some(loop_cancel), Some(dlq))
                .await;
        });
        LoopUnderTest {
            cancel,
            handle,
            deliveries,
            listener_name,
        }
    }

    #[tokio::test]
    async fn test_parse_error_dead_letters_and_acks() {
        let url = shared_nats_url().await;
        let ctx = NatsTestContext::with_url(url).await.expect("ctx");

        let dlq_consumer = create_dlq_consumer(&ctx, "parse").await;

        let payload = serde_json::json!({"not": "a valid request"});
        ctx.jetstream
            .publish(
                ctx.inject_subject.clone(),
                serde_json::to_vec(&payload).unwrap().into(),
            )
            .await
            .expect("publish")
            .await
            .expect("publish ack");

        let lut = spawn_failing_loop(&ctx, "dlq_parse", FailKind::Parse).await;

        let entry = wait_for_dlq_entry(dlq_consumer, &lut.listener_name).await;
        assert_eq!(entry.error_class, DlqErrorClass::Parse);
        assert_eq!(entry.error, "unparseable");
        assert_eq!(entry.original_subject, ctx.inject_subject);
        assert_eq!(entry.payload, Some(payload), "original payload intact");
        assert!(entry.payload_base64.is_none());

        // ACKed after dead-lettering — no redelivery
        assert_fully_acked(&ctx, "dlq_parse").await;
        assert_eq!(lut.deliveries.load(Ordering::SeqCst), 1);

        lut.cancel.cancel();
        lut.handle.await.ok();
        ctx.cleanup().await.ok();
    }

    #[tokio::test]
    async fn test_business_error_dead_letters_and_acks() {
        let url = shared_nats_url().await;
        let ctx = NatsTestContext::with_url(url).await.expect("ctx");

        let dlq_consumer = create_dlq_consumer(&ctx, "business").await;

        let payload = serde_json::json!({"place_id": "p_missing", "color": {}});
        ctx.jetstream
            .publish(
                ctx.inject_subject.clone(),
                serde_json::to_vec(&payload).unwrap().into(),
            )
            .await
            .expect("publish")
            .await
            .expect("publish ack");

        let lut = spawn_failing_loop(&ctx, "dlq_business", FailKind::Business).await;

        let entry = wait_for_dlq_entry(dlq_consumer, &lut.listener_name).await;
        assert_eq!(entry.error_class, DlqErrorClass::Business);
        assert_eq!(entry.error, "rejected");
        assert_eq!(entry.payload, Some(payload));

        assert_fully_acked(&ctx, "dlq_business").await;
        assert_eq!(lut.deliveries.load(Ordering::SeqCst), 1);

        lut.cancel.cancel();
        lut.handle.await.ok();
        ctx.cleanup().await.ok();
    }

    #[tokio::test]
    async fn test_internal_error_retries_then_dead_letters() {
        let url = shared_nats_url().await;
        let ctx = NatsTestContext::with_url(url).await.expect("ctx");

        let dlq_consumer = create_dlq_consumer(&ctx, "internal").await;

        let payload = serde_json::json!({"work": "transiently broken"});
        ctx.jetstream
            .publish(
                ctx.inject_subject.clone(),
                serde_json::to_vec(&payload).unwrap().into(),
            )
            .await
            .expect("publish")
            .await
            .expect("publish ack");

        let lut = spawn_failing_loop(&ctx, "dlq_internal", FailKind::Internal).await;

        // 4 NAKs with escalating delay (0.5+1+1.5+2 = 5s), dead-lettered on
        // the 5th delivery.
        let entry = wait_for_dlq_entry(dlq_consumer, &lut.listener_name).await;
        assert_eq!(entry.error_class, DlqErrorClass::Internal);
        assert_eq!(entry.delivered, 5, "dead-lettered on the 5th delivery");
        assert_eq!(entry.payload, Some(payload));

        assert_fully_acked(&ctx, "dlq_internal").await;
        assert_eq!(
            lut.deliveries.load(Ordering::SeqCst),
            5,
            "redelivered until the retry budget, then ACKed"
        );

        lut.cancel.cancel();
        lut.handle.await.ok();
        ctx.cleanup().await.ok();
    }
}

// =============================================================================
// Durable Idempotency Cache Integration Tests
// =============================================================================

#[tokio::test]
async fn test_idempotency_cache_kv_survives_restart() {
    use crate::idempotency::{CachedResult, IdempotencyCache, IdempotencyCacheConfig};

    let url = shared_nats_url().await;
    let client = async_nats::connect(url).await.expect("connect");
    let jetstream = async_nats::jetstream::new(client);

    // Unique bucket per test run (the prod bucket name is shared engine-wide)
    let bucket = format!("IDEMP_{}", uuid::Uuid::new_v4().simple()).to_uppercase();
    let kv = jetstream
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: bucket.clone(),
            max_age: Duration::from_secs(3600),
            history: 1,
            ..Default::default()
        })
        .await
        .expect("create KV bucket");

    let cache1 = IdempotencyCache::with_kv(IdempotencyCacheConfig::default(), kv.clone());
    cache1
        .insert(
            "PETRI_GLOBAL:42".to_string(),
            CachedResult::Success {
                event_sequence: 42,
                token_id: Some("token-abc".to_string()),
            },
        )
        .await;

    // Fresh cache over the same bucket = simulated engine restart
    let cache2 = IdempotencyCache::with_kv(IdempotencyCacheConfig::default(), kv.clone());
    assert!(cache2.is_empty(), "fresh cache starts with empty memory");

    let got = cache2
        .get("PETRI_GLOBAL:42")
        .await
        .expect("entry must survive the restart via KV");
    match got {
        CachedResult::Success {
            event_sequence,
            token_id,
        } => {
            assert_eq!(event_sequence, 42);
            assert_eq!(token_id, Some("token-abc".to_string()));
        }
        other => panic!("expected Success, got {:?}", other),
    }

    // KV hit repopulates memory
    assert_eq!(cache2.len(), 1);

    // Unknown keys still miss through both layers
    assert!(cache2.get("PETRI_GLOBAL:9999").await.is_none());

    jetstream.delete_key_value(&bucket).await.ok();
}

// =============================================================================
// Per-Workspace Consume-Side Isolation (Multi-Tenancy, ADR-09)
// =============================================================================
//
// These tests prove the LOAD-BEARING isolation delivered by the phase-2
// consume-side cut: two nets hosted in ONE engine/jetstream under DISTINCT
// workspaces (`wsA` vs `wsB`) never cross-contaminate on the read side.
//
// The publisher already routes per-net (events go to `petri.{ws}.{net}.events.*`,
// signals to `petri.{ws}.{net}.signal.{place}`). What this phase fixes is the
// CONSUMER: each net's event consumer and signal inbox filter on the net's REAL
// workspace (from `LoadScenarioRequest.workspace_id`), not the process fallback.
// We assert that at the NATS server edge — a workspace-B consumer subscribing
// `petri.wsB.{net}.events.>` receives ZERO of workspace-A's events even though
// both nets share the single `PETRI_GLOBAL` stream.
//
// Each test mints UNIQUE workspace ids (`wsA-{prefix}` / `wsB-{prefix}`) so the
// per-tenant KV buckets (`KV_NET_METADATA_{ws}`) and stream subjects don't
// collide with parallel tests on the shared NATS testcontainer.

/// Publish a `PersistedEvent`-wrapped domain event onto the global stream under
/// an explicit `petri.{ws}.{net}.events.*` subject. This is the raw publish a
/// net's `NatsEventStore` performs once its workspace cell is stamped; doing it
/// directly lets the test assert the server-edge consumer filter in isolation
/// without standing up a full service.
async fn publish_event_for(
    jetstream: &async_nats::jetstream::Context,
    ws: &str,
    net_id: &str,
    event: petri_domain::DomainEvent,
) {
    use crate::subjects::Subjects;
    let subject = Subjects::for_event(&event, ws, Some(net_id));
    let persisted = petri_domain::PersistedEvent {
        sequence: 0,
        event,
        timestamp: chrono::Utc::now(),
        hash: String::new(),
        previous_hash: None,
    };
    let payload = serde_json::to_vec(&persisted).expect("serialize persisted event");
    jetstream
        .publish(subject, payload.into())
        .await
        .expect("publish event")
        .await
        .expect("ack event");
}

/// Drain every currently-available message from an ephemeral pull consumer
/// (best-effort, batched with a short idle deadline). Returns the delivered
/// subjects so the test can assert WHICH nets/workspaces leaked through.
async fn drain_subjects(
    consumer: &async_nats::jetstream::consumer::PullConsumer,
    max: usize,
) -> Vec<String> {
    let mut subjects = Vec::new();
    let mut batch = consumer
        .batch()
        .max_messages(max)
        .expires(Duration::from_millis(750))
        .messages()
        .await
        .expect("batch");
    while let Some(Ok(msg)) = batch.next().await {
        subjects.push(msg.subject.to_string());
        let _ = msg.ack().await;
    }
    subjects
}

/// Create an ephemeral pull consumer on the global stream with an explicit
/// filter subject and `DeliverPolicy::All` (replays everything already on the
/// stream so ordering against the publish is not load-bearing).
async fn global_consumer_on(
    jetstream: &async_nats::jetstream::Context,
    filter_subject: String,
) -> async_nats::jetstream::consumer::PullConsumer {
    let stream = jetstream
        .get_stream(crate::subjects::Subjects::STREAM_GLOBAL)
        .await
        .expect("get global stream");
    stream
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            filter_subject,
            deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::All,
            ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
            ..Default::default()
        })
        .await
        .expect("create filtered consumer")
}

/// (1) + (2): Two nets in ONE jetstream under distinct workspaces. An event
/// fired in wsA's net lands on `petri.wsA.{net}.events.*`; wsB's per-net event
/// consumer (filtering `petri.wsB.{net}.events.>`) receives ZERO of wsA's
/// events. (5) NEGATIVE CONTROL: a consumer that filters on wsA's subject DOES
/// receive them — proving the workspace segment is the load-bearing
/// discriminator, not a vacuous pass.
#[tokio::test]
async fn test_per_workspace_event_consumer_isolation() {
    use crate::subjects::Subjects;
    use petri_domain::{DomainEvent, PetriNet, Place, Token, TokenColor};

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    let ws_a = format!("wsA-{}", ctx.prefix);
    let ws_b = format!("wsB-{}", ctx.prefix);
    // Distinct net ids (mekhan-A-i1 / mekhan-B-i1 style) so even a ws-blind
    // (buggy) consumer could not alias the two nets by id.
    let net_a = format!("mekhan-A-{}", ctx.prefix);
    let net_b = format!("mekhan-B-{}", ctx.prefix);

    // wsB's per-net event consumer — the REAL consume-side filter the engine
    // builds from `net_events_filter(realws, net)`.
    let consumer_b =
        global_consumer_on(&ctx.jetstream, Subjects::net_events_filter(&ws_b, &net_b)).await;
    // NEGATIVE CONTROL: a consumer scoped to wsA's net. If the workspace segment
    // were NOT load-bearing, consumer_b would behave like this one.
    let consumer_a =
        global_consumer_on(&ctx.jetstream, Subjects::net_events_filter(&ws_a, &net_a)).await;

    // Initialize both nets (NetInitialized) under their respective workspaces.
    let mut topo_a = PetriNet::new();
    topo_a.add_place(Place::internal("p"));
    publish_event_for(
        &ctx.jetstream,
        &ws_a,
        &net_a,
        DomainEvent::NetInitialized { net: topo_a },
    )
    .await;

    let mut topo_b = PetriNet::new();
    topo_b.add_place(Place::internal("p"));
    publish_event_for(
        &ctx.jetstream,
        &ws_b,
        &net_b,
        DomainEvent::NetInitialized { net: topo_b },
    )
    .await;

    // Fire a token in wsA's net only.
    let place_a = petri_domain::PlaceId::new();
    publish_event_for(
        &ctx.jetstream,
        &ws_a,
        &net_a,
        DomainEvent::TokenCreated {
            token: Token::new(TokenColor::Data(serde_json::json!({"value": 42}))),
            place_id: place_a,
            place_name: Some("p".to_string()),
            workflow_id: None,
            signal_key: None,
            dedup_id: None,
        },
    )
    .await;

    // wsB's consumer must receive ONLY wsB's NetInitialized — never wsA's.
    let b_subjects = drain_subjects(&consumer_b, 32).await;
    assert!(
        !b_subjects.is_empty(),
        "wsB consumer should at least see wsB's own NetInitialized"
    );
    assert!(
        b_subjects.iter().all(|s| s.contains(&format!(".{ws_b}."))),
        "wsB consumer leaked a non-wsB subject: {b_subjects:?}"
    );
    assert!(
        b_subjects.iter().all(|s| !s.contains(&net_a)),
        "wsB consumer received wsA's net events — ISOLATION BREACH: {b_subjects:?}"
    );
    assert!(
        b_subjects
            .iter()
            .all(|s| !s.ends_with("events.token.created")),
        "wsB consumer received the token.created fired in wsA: {b_subjects:?}"
    );

    // NEGATIVE CONTROL: the wsA-scoped consumer DOES see wsA's token.created.
    // If this fails, the publish never happened and the isolation assert above
    // is vacuous.
    let a_subjects = drain_subjects(&consumer_a, 32).await;
    assert!(
        a_subjects
            .iter()
            .any(|s| s.ends_with("events.token.created") && s.contains(&net_a)),
        "negative control: wsA consumer should have received wsA's token.created, \
         proving the event was actually published (got {a_subjects:?})"
    );

    ctx.cleanup().await.ok();
}

/// (3): An external signal injected on `petri.wsA.{net}.signal.{place}` is
/// visible to wsA's per-net signal inbox filter and INVISIBLE to wsB's. This is
/// the server-edge guarantee behind a net's signal-inbox listener filtering on
/// its REAL workspace — wsB's marking can never change from a wsA signal.
#[tokio::test]
async fn test_per_workspace_signal_inbox_isolation() {
    use crate::subjects::Subjects;
    use petri_domain::ExternalSignal;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    let ws_a = format!("wsA-{}", ctx.prefix);
    let ws_b = format!("wsB-{}", ctx.prefix);
    let net_a = format!("mekhan-A-{}", ctx.prefix);
    let net_b = format!("mekhan-B-{}", ctx.prefix);

    // Each net's signal-inbox filter: `petri.{realws}.{net}.signal.>`.
    let inbox_a =
        global_consumer_on(&ctx.jetstream, Subjects::signal_inbox_filter(&ws_a, &net_a)).await;
    let inbox_b =
        global_consumer_on(&ctx.jetstream, Subjects::signal_inbox_filter(&ws_b, &net_b)).await;

    // Inject a signal addressed to wsA's net + place.
    let signal = ExternalSignal {
        source: "test".to_string(),
        signal_key: "corr-iso".to_string(),
        payload: serde_json::json!({"data": "for-wsA"}),
        timestamp: chrono::Utc::now(),
        dedup_id: None,
    };
    let subject = Subjects::signal_transfer(&ws_a, &net_a, "inbox");
    ctx.jetstream
        .publish(subject, serde_json::to_vec(&signal).unwrap().into())
        .await
        .expect("publish signal")
        .await
        .expect("ack signal");

    // wsA's inbox receives the signal.
    let a_subjects = drain_subjects(&inbox_a, 16).await;
    assert_eq!(
        a_subjects.len(),
        1,
        "wsA signal inbox should receive exactly the one wsA signal, got {a_subjects:?}"
    );
    assert_eq!(
        a_subjects[0],
        Subjects::signal_transfer(&ws_a, &net_a, "inbox")
    );

    // wsB's inbox receives NOTHING — its marking is unchanged.
    let b_subjects = drain_subjects(&inbox_b, 16).await;
    assert!(
        b_subjects.is_empty(),
        "wsB signal inbox must NOT receive a signal addressed to wsA — ISOLATION BREACH: {b_subjects:?}"
    );

    ctx.cleanup().await.ok();
}

/// (4): The per-tenant net-metadata KV buckets hold only their own workspace's
/// nets. The `NetMetadataProjection` consumes the global event stream, derives
/// the workspace from each event subject, and dual-writes a per-tenant
/// `KV_NET_METADATA_{ws}` bucket. After publishing NetCreated for net A under
/// wsA and net B under wsB, `KV_NET_METADATA_{wsA}` holds A and not B, and vice
/// versa — no cross-tenant key.
#[tokio::test]
async fn test_per_workspace_metadata_kv_isolation() {
    use crate::net_metadata::{NetMetadataProjection, METADATA_KV_BUCKET};
    use petri_domain::DomainEvent;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    let ws_a = format!("wsA-{}", ctx.prefix);
    let ws_b = format!("wsB-{}", ctx.prefix);
    let net_a = format!("mekhan-A-{}", ctx.prefix);
    let net_b = format!("mekhan-B-{}", ctx.prefix);

    // Global index bucket the projection also writes (net_id-keyed). Created
    // here so `NetMetadataProjection::new` has a store; per-ws buckets are
    // opened lazily by the projection as it observes each workspace.
    let index_kv = create_metadata_kv(&ctx.jetstream, &ctx.prefix).await;
    let projection = NetMetadataProjection::new(ctx.jetstream.clone(), index_kv);
    let projection_handle = projection.start();

    // Give the projection's consumer a moment to attach before publishing.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // NetCreated for A under wsA, B under wsB.
    publish_event_for(
        &ctx.jetstream,
        &ws_a,
        &net_a,
        DomainEvent::NetCreated {
            net_id: net_a.clone(),
            template_id: None,
            parameters: None,
            created_by: None,
            label: None,
        },
    )
    .await;
    publish_event_for(
        &ctx.jetstream,
        &ws_b,
        &net_b,
        DomainEvent::NetCreated {
            net_id: net_b.clone(),
            template_id: None,
            parameters: None,
            created_by: None,
            label: None,
        },
    )
    .await;

    // Poll until both per-ws buckets reflect their own net (the projection runs
    // asynchronously).
    let bucket_a = crate::kv_bucket_for(METADATA_KV_BUCKET, &ws_a);
    let bucket_b = crate::kv_bucket_for(METADATA_KV_BUCKET, &ws_b);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let kv_a = ctx.jetstream.get_key_value(&bucket_a).await.ok();
        let kv_b = ctx.jetstream.get_key_value(&bucket_b).await.ok();
        let a_has_a = match &kv_a {
            Some(kv) => kv.get(&net_a).await.ok().flatten().is_some(),
            None => false,
        };
        let b_has_b = match &kv_b {
            Some(kv) => kv.get(&net_b).await.ok().flatten().is_some(),
            None => false,
        };
        if a_has_a && b_has_b {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!(
                "metadata projection did not populate per-ws buckets in time \
                 (a_has_a={a_has_a}, b_has_b={b_has_b})"
            );
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    let kv_a = ctx
        .jetstream
        .get_key_value(&bucket_a)
        .await
        .expect("wsA metadata bucket");
    let kv_b = ctx
        .jetstream
        .get_key_value(&bucket_b)
        .await
        .expect("wsB metadata bucket");

    // wsA's bucket holds A, NOT B. (4) No cross-tenant key.
    assert!(
        kv_a.get(&net_a).await.ok().flatten().is_some(),
        "KV_NET_METADATA_{{wsA}} should hold net A"
    );
    assert!(
        kv_a.get(&net_b).await.ok().flatten().is_none(),
        "KV_NET_METADATA_{{wsA}} leaked net B — CROSS-TENANT KEY"
    );
    // wsB's bucket holds B, NOT A.
    assert!(
        kv_b.get(&net_b).await.ok().flatten().is_some(),
        "KV_NET_METADATA_{{wsB}} should hold net B"
    );
    assert!(
        kv_b.get(&net_a).await.ok().flatten().is_none(),
        "KV_NET_METADATA_{{wsB}} leaked net A — CROSS-TENANT KEY"
    );

    // Workspace stamping is correct on the stored metadata (subject-derived).
    let meta_a: crate::net_metadata::NetMetadata =
        serde_json::from_slice(&kv_a.get(&net_a).await.unwrap().unwrap()).unwrap();
    assert_eq!(
        meta_a.workspace_id, ws_a,
        "net A metadata must be stamped wsA"
    );
    let meta_b: crate::net_metadata::NetMetadata =
        serde_json::from_slice(&kv_b.get(&net_b).await.unwrap().unwrap()).unwrap();
    assert_eq!(
        meta_b.workspace_id, ws_b,
        "net B metadata must be stamped wsB"
    );

    projection_handle.abort();
    // Per-ws buckets are uniquely named per test run; clean them up.
    ctx.jetstream.delete_key_value(&bucket_a).await.ok();
    ctx.jetstream.delete_key_value(&bucket_b).await.ok();
    ctx.cleanup().await.ok();
}

// =============================================================================
// Net Snapshot KV Integration Tests (PART C: snapshot-on-hibernate)
// =============================================================================

#[tokio::test]
async fn test_net_snapshot_round_trip() {
    use crate::net_snapshot::NetSnapshotStore;
    use petri_application::net_snapshot::{NetSnapshot, SnapshotStore, SNAPSHOT_VERSION};
    use petri_domain::Marking;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url)
        .await
        .expect("Failed to create context");

    // Unique per-test workspace so the per-ws bucket can't collide with other
    // tests sharing the NATS server.
    let ws = format!("snapws-{}", ctx.prefix);
    let net_id = "snap-net";
    let store = NetSnapshotStore::new(ctx.jetstream.clone());

    // Absent → None (no snapshot yet).
    assert!(
        store.get(&ws, net_id).await.is_none(),
        "no snapshot should exist initially"
    );

    let snap = NetSnapshot {
        marking: Marking::new(),
        dedup: vec![],
        last_hash: Some("deadbeef".to_string()),
        event_count: 7,
        next_sequence: 7,
        last_stream_seq: 42,
        version: SNAPSHOT_VERSION,
    };
    store.put(&ws, net_id, &snap).await;

    let got = store
        .get(&ws, net_id)
        .await
        .expect("snapshot must round-trip");
    assert_eq!(got.last_hash, Some("deadbeef".to_string()));
    assert_eq!(got.event_count, 7);
    assert_eq!(got.next_sequence, 7);
    assert_eq!(got.last_stream_seq, 42);

    // Delete reclaims it → None again.
    store.delete(&ws, net_id).await;
    assert!(
        store.get(&ws, net_id).await.is_none(),
        "snapshot must be gone after delete"
    );

    // Cleanup the per-ws bucket.
    let bucket = crate::kv_bucket_for(crate::net_snapshot::SNAPSHOT_KV_BUCKET, &ws);
    ctx.jetstream.delete_key_value(&bucket).await.ok();
    ctx.cleanup().await.ok();
}

#[tokio::test]
async fn test_net_snapshot_future_version_ignored() {
    use crate::net_snapshot::NetSnapshotStore;
    use petri_application::net_snapshot::{NetSnapshot, SnapshotStore, SNAPSHOT_VERSION};
    use petri_domain::Marking;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url)
        .await
        .expect("Failed to create context");

    let ws = format!("snapvws-{}", ctx.prefix);
    let net_id = "snap-net-v";
    let store = NetSnapshotStore::new(ctx.jetstream.clone());

    // A snapshot written by a NEWER engine (version > supported) must be
    // ignored on read → wake falls back to full replay.
    let snap = NetSnapshot {
        marking: Marking::new(),
        dedup: vec![],
        last_hash: None,
        event_count: 0,
        next_sequence: 0,
        last_stream_seq: 0,
        version: SNAPSHOT_VERSION + 1,
    };
    store.put(&ws, net_id, &snap).await;
    assert!(
        store.get(&ws, net_id).await.is_none(),
        "a newer-versioned snapshot must be ignored (→ full replay)"
    );

    let bucket = crate::kv_bucket_for(crate::net_snapshot::SNAPSHOT_KV_BUCKET, &ws);
    ctx.jetstream.delete_key_value(&bucket).await.ok();
    ctx.cleanup().await.ok();
}

/// MAJOR 2a regression: a snapshot wake with an EMPTY post-snapshot delta must
/// still seed the NATS store's authoritative write state, so the first live
/// `append` continues from `snapshot.next_sequence` (NOT 0), and an SSE
/// broadcast cursor initialized at the post-wake tip emits that event.
///
/// Pre-fix reproduction: without `seed_write_state`, `WriteState.next_sequence`
/// stays at the construction-time `*applied_rx.borrow()` (0) because the
/// resumed consumer (started at `last_stream_seq + 1`, past the end) applies
/// nothing and never ticks `applied_rx`. The first append then mints
/// `.sequence == 0` — colliding with the pre-hibernate prefix and (because the
/// SSE cursor was anchored on the large pre-wake sequence) never broadcast.
#[tokio::test]
async fn snapshot_wake_empty_delta_seeds_write_state_and_sse_cursor() {
    use crate::event_consumer::EventConsumer;
    use crate::NatsConfig;
    use crate::NatsEventStore;
    use petri_application::{EventRepository, SnapshotInputs};
    use petri_domain::{DomainEvent, PetriNet, Place, TokenColor};
    use petri_infrastructure::{MemoryEventStore, MemoryTopologyStore};
    use std::sync::Arc;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    let net_id = format!("snap2a-{}", ctx.prefix);

    // ── Phase 1: publish a prefix of events through a live store. ──
    let shutdown1 = tokio_util::sync::CancellationToken::new();
    let cache1 = Arc::new(MemoryEventStore::new());
    let topo1 = Arc::new(MemoryTopologyStore::new());
    let (applied_tx1, applied_rx1) = tokio::sync::watch::channel(0u64);
    let (ready_tx1, ready_rx1) = tokio::sync::oneshot::channel();
    let consumer1 = EventConsumer::new(cache1.clone(), topo1.clone(), applied_tx1, ready_tx1);
    let js1 = ctx.jetstream.clone();
    let net_id1 = net_id.clone();
    let shutdown1c = shutdown1.clone();
    tokio::spawn(async move {
        let _ = consumer1
            .start(
                &js1,
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
                &net_id1,
                shutdown1c,
            )
            .await;
    });
    tokio::time::timeout(Duration::from_secs(5), ready_rx1)
        .await
        .expect("ready1")
        .expect("ready1 rx");

    let mut config1 = NatsConfig::from_env();
    config1.net_id = Some(net_id.clone());
    let store1 = NatsEventStore::new(cache1.clone(), ctx.jetstream.clone(), config1, applied_rx1);

    let mut net = PetriNet::new();
    let place_a = net.add_place(Place::internal("place_a"));
    store1
        .append(DomainEvent::NetInitialized { net: net.clone() })
        .await
        .expect("init");
    for v in 0..4u64 {
        store1
            .append(DomainEvent::TokenCreated {
                token: petri_domain::Token::new(TokenColor::Data(serde_json::json!({ "v": v }))),
                place_id: place_a.clone(),
                place_name: Some("place_a".to_string()),
                workflow_id: None,
                signal_key: None,
                dedup_id: None,
            })
            .await
            .expect("tc");
    }
    // 5 events total → next live sequence is 5.
    let inputs: SnapshotInputs = store1.snapshot_inputs().await;
    let snapshot = inputs.into_snapshot();
    assert_eq!(snapshot.next_sequence, 5, "5 events appended → next seq 5");
    assert!(snapshot.last_stream_seq >= 5, "consumer applied 5 events");
    let snap_next_seq = snapshot.next_sequence;
    let snap_last_hash = snapshot.last_hash.clone();

    // ── Phase 2: hibernate — cancel consumer, drop store. ──
    shutdown1.cancel();
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(store1);

    // ── Phase 3: wake with an EMPTY delta — seed from snapshot, resume the
    //    consumer at last_stream_seq + 1 (past the end → nothing replays). ──
    let shutdown2 = tokio_util::sync::CancellationToken::new();
    let cache2 = Arc::new(MemoryEventStore::new());
    let topo2 = Arc::new(MemoryTopologyStore::new());
    let (applied_tx2, applied_rx2) = tokio::sync::watch::channel(0u64);
    let (ready_tx2, ready_rx2) = tokio::sync::oneshot::channel();

    // Seed the read cache from the snapshot (as the registry wake path does).
    cache2.seed_from_snapshot(&snapshot);

    let consumer2 = EventConsumer::new(cache2.clone(), topo2.clone(), applied_tx2, ready_tx2)
        .with_resume_from(snapshot.last_stream_seq);
    let js2 = ctx.jetstream.clone();
    let net_id2 = net_id.clone();
    let shutdown2c = shutdown2.clone();
    tokio::spawn(async move {
        let _ = consumer2
            .start(
                &js2,
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
                &net_id2,
                shutdown2c,
            )
            .await;
    });
    tokio::time::timeout(Duration::from_secs(5), ready_rx2)
        .await
        .expect("ready2")
        .expect("ready2 rx");

    // The delta was empty: applied_rx2 was never ticked past its init (0).
    assert_eq!(
        *applied_rx2.borrow(),
        0,
        "empty post-snapshot delta → consumer never ticked applied_rx (this is the trap)"
    );

    let mut config2 = NatsConfig::from_env();
    config2.net_id = Some(net_id.clone());
    let store2 = Arc::new(NatsEventStore::new(
        cache2.clone(),
        ctx.jetstream.clone(),
        config2,
        applied_rx2,
    ));

    // THE FIX (MAJOR 2a): seed the write authority from the snapshot. Without
    // this call the next append below mints `.sequence == 0`.
    store2
        .seed_write_state(snap_next_seq, snap_last_hash.clone())
        .await;

    // Compute the SSE broadcast cursor as the eval loop now does: post-wake tip.
    let sse_cursor = store2.current_sequence().await.saturating_sub(1);
    assert_eq!(sse_cursor, snap_next_seq - 1, "SSE cursor anchors at the tip");

    // First live append after the empty-delta wake.
    let appended = store2
        .append(DomainEvent::TokenCreated {
            token: petri_domain::Token::new(TokenColor::Data(serde_json::json!({ "v": 100 }))),
            place_id: place_a.clone(),
            place_name: Some("place_a".to_string()),
            workflow_id: None,
            signal_key: None,
            dedup_id: None,
        })
        .await
        .expect("post-wake append");

    // (a) sequence continues from the snapshot, NOT 0.
    assert_eq!(
        appended.sequence, snap_next_seq,
        "post-wake append must continue from snapshot.next_sequence, not 0 \
         (pre-fix: minted 0)"
    );
    // (b) hash chain links to the snapshot tip.
    assert_eq!(
        appended.previous_hash, snap_last_hash,
        "post-wake append must chain to the snapshot tip"
    );
    // (c) the SSE broadcast cursor WOULD emit this event.
    assert!(
        appended.sequence > sse_cursor,
        "post-wake event (seq {}) must be > SSE cursor (seq {}) so it is broadcast",
        appended.sequence,
        sse_cursor
    );

    shutdown2.cancel();
    ctx.cleanup().await.ok();
}

/// MAJOR 2b regression: the snapshot's `(marking, last_stream_seq)` pair is
/// captured coherently under one store lock, so a consumer applying an event in
/// the window AROUND `write_snapshot` cannot skew them by one event. We exercise
/// the race by appending an extra "racing" event to the live store and waiting
/// for the consumer to apply it, then snapshotting: the snapshot's
/// `last_stream_seq` must reflect the SAME prefix as its marking (i.e. include
/// the racing event), and a wake that resumes at `last_stream_seq + 1` must
/// reproduce the full-replay marking with NO duplicate and NO loss.
#[tokio::test]
async fn snapshot_last_stream_seq_is_coherent_with_marking_no_dup_no_loss() {
    use crate::event_consumer::EventConsumer;
    use crate::NatsConfig;
    use crate::NatsEventStore;
    use petri_application::{EventRepository, StateProjection};
    use petri_domain::{DomainEvent, PetriNet, Place, TokenColor};
    use petri_infrastructure::{MarkingProjection, MemoryEventStore, MemoryTopologyStore};
    use std::sync::Arc;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    let net_id = format!("snap2b-{}", ctx.prefix);

    let shutdown1 = tokio_util::sync::CancellationToken::new();
    let cache1 = Arc::new(MemoryEventStore::new());
    let topo1 = Arc::new(MemoryTopologyStore::new());
    let (applied_tx1, applied_rx1) = tokio::sync::watch::channel(0u64);
    let (ready_tx1, ready_rx1) = tokio::sync::oneshot::channel();
    let consumer1 = EventConsumer::new(cache1.clone(), topo1.clone(), applied_tx1, ready_tx1);
    let js1 = ctx.jetstream.clone();
    let net_id1 = net_id.clone();
    let shutdown1c = shutdown1.clone();
    tokio::spawn(async move {
        let _ = consumer1
            .start(
                &js1,
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
                &net_id1,
                shutdown1c,
            )
            .await;
    });
    tokio::time::timeout(Duration::from_secs(5), ready_rx1)
        .await
        .expect("ready1")
        .expect("ready1 rx");

    let mut config1 = NatsConfig::from_env();
    config1.net_id = Some(net_id.clone());
    let store1 = NatsEventStore::new(cache1.clone(), ctx.jetstream.clone(), config1, applied_rx1);

    let mut net = PetriNet::new();
    let place_a = net.add_place(Place::internal("place_a"));
    store1
        .append(DomainEvent::NetInitialized { net: net.clone() })
        .await
        .expect("init");
    // Park 3 tokens.
    for v in 0..3u64 {
        store1
            .append(DomainEvent::TokenCreated {
                token: petri_domain::Token::new(TokenColor::Data(serde_json::json!({ "v": v }))),
                place_id: place_a.clone(),
                place_name: Some("place_a".to_string()),
                workflow_id: None,
                signal_key: None,
                dedup_id: None,
            })
            .await
            .expect("tc");
    }

    // "Racing" event applied just before the snapshot read. With the under-one-
    // lock capture, the snapshot's last_stream_seq advances together with the
    // marking that folds this 4th token — they cannot disagree.
    store1
        .append(DomainEvent::TokenCreated {
            token: petri_domain::Token::new(TokenColor::Data(serde_json::json!({ "v": 99 }))),
            place_id: place_a.clone(),
            place_name: Some("place_a".to_string()),
            workflow_id: None,
            signal_key: None,
            dedup_id: None,
        })
        .await
        .expect("racing tc");

    // Wait for the consumer to have applied all 5 events (init + 4 tokens) to
    // cache1, so the snapshot reflects them.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if cache1.len().await == 5 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cache1 applied all 5 events");

    let snapshot = store1.snapshot_inputs().await.into_snapshot();

    // Full replay marking from the live store's base ⊕ tail.
    let proj = MarkingProjection::new();
    let full_marking = {
        let (b, t, _extent) = store1.marking_base().await;
        proj.project_onto(&b, &t)
    };
    assert_eq!(full_marking.token_count(&place_a), 4, "4 tokens parked");

    // COHERENCE: the snapshot marking equals the full replay marking, AND the
    // last_stream_seq reflects the SAME prefix (all 5 applied events).
    assert_eq!(
        snapshot.marking.token_count(&place_a),
        4,
        "snapshot marking must fold the racing event too (coherent capture)"
    );
    assert_eq!(
        snapshot.event_count, 5,
        "snapshot event_count must include the racing event"
    );
    assert!(
        snapshot.last_stream_seq >= 5,
        "last_stream_seq must reflect all 5 applied events (coherent with marking), got {}",
        snapshot.last_stream_seq
    );

    // ── Wake: seed from snapshot, resume at last_stream_seq + 1. The delta is
    //    empty (nothing was published after the snapshot), so the woken marking
    //    must equal the full marking — no double-fold of the racing event, no
    //    loss. ──
    shutdown1.cancel();
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(store1);

    let shutdown2 = tokio_util::sync::CancellationToken::new();
    let cache2 = Arc::new(MemoryEventStore::new());
    let topo2 = Arc::new(MemoryTopologyStore::new());
    let (applied_tx2, applied_rx2) = tokio::sync::watch::channel(0u64);
    let (ready_tx2, ready_rx2) = tokio::sync::oneshot::channel();
    cache2.seed_from_snapshot(&snapshot);
    let consumer2 = EventConsumer::new(cache2.clone(), topo2.clone(), applied_tx2, ready_tx2)
        .with_resume_from(snapshot.last_stream_seq);
    let js2 = ctx.jetstream.clone();
    let net_id2 = net_id.clone();
    let shutdown2c = shutdown2.clone();
    tokio::spawn(async move {
        let _ = consumer2
            .start(
                &js2,
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
                &net_id2,
                shutdown2c,
            )
            .await;
    });
    tokio::time::timeout(Duration::from_secs(5), ready_rx2)
        .await
        .expect("ready2")
        .expect("ready2 rx");
    let _ = applied_rx2;

    let woken_marking = {
        let (b, t, _extent) = cache2.marking_base().await;
        proj.project_onto(&b, &t)
    };
    assert_eq!(
        woken_marking.token_count(&place_a),
        full_marking.token_count(&place_a),
        "woken marking must equal full replay — no dup, no loss of the racing event"
    );
    assert_eq!(
        woken_marking.token_count(&place_a),
        4,
        "exactly 4 tokens after wake"
    );

    shutdown2.cancel();
    ctx.cleanup().await.ok();
}

/// Regression: a snapshot wake with a NON-EMPTY post-snapshot delta must seed
/// the write authority from the POST-REPLAY tip, not from the stale snapshot
/// baseline. This exercises the exact registry wake ordering (PART C):
/// `seed_from_snapshot` → `seed_write_state(snap.next_sequence, snap.last_hash)`
/// (called BEFORE hydration, as `net_registry::get_or_create` does) → consumer
/// resumes at `last_stream_seq + 1` and replays the delta.
///
/// After the delta replays, the cache's real chain tip is the LAST DELTA event's
/// hash and the next free `.sequence` is `snap.next_sequence + delta_len`. But
/// `seed_write_state` pinned `WriteState.last_hash = Some(snap.last_hash)` (the
/// PRE-delta tip) and `WriteState.next_sequence = snap.next_sequence`. Because
/// `last_hash` is now `Some(_)`, the append's lazy hash-chain recovery
/// (`if next_sequence > 0 && last_hash.is_none()`) is SUPPRESSED — so the first
/// live append links its `previous_hash` to the stale `snap.last_hash` instead
/// of the actual post-delta tip, FORKING the hash chain (two events both
/// pointing at `snap.last_hash`).
///
/// Pre-fix this asserts `previous_hash == snap.last_hash` (the bug). Post-fix
/// (re-seed write state from the post-replay cache AFTER hydration) it links to
/// the last delta event's hash.
#[tokio::test]
async fn snapshot_wake_nonempty_delta_chains_append_to_post_delta_tip() {
    use crate::event_consumer::EventConsumer;
    use crate::NatsConfig;
    use crate::NatsEventStore;
    use petri_application::EventRepository;
    use petri_domain::{DomainEvent, PetriNet, Place, TokenColor};
    use petri_infrastructure::{MemoryEventStore, MemoryTopologyStore};
    use std::sync::Arc;

    let url = shared_nats_url().await;
    let ctx = NatsTestContext::with_url(url).await.expect("context");
    petri_test_harness::nats::ensure_global_stream(&ctx.jetstream)
        .await
        .expect("ensure stream");

    let net_id = format!("snap-nonempty-{}", ctx.prefix);

    // ── Phase 1: live store, publish a prefix, snapshot, then publish MORE
    //    events (the "delta" that lands while hibernated). ──
    let shutdown1 = tokio_util::sync::CancellationToken::new();
    let cache1 = Arc::new(MemoryEventStore::new());
    let topo1 = Arc::new(MemoryTopologyStore::new());
    let (applied_tx1, applied_rx1) = tokio::sync::watch::channel(0u64);
    let (ready_tx1, ready_rx1) = tokio::sync::oneshot::channel();
    let consumer1 = EventConsumer::new(cache1.clone(), topo1.clone(), applied_tx1, ready_tx1);
    let js1 = ctx.jetstream.clone();
    let net_id1 = net_id.clone();
    let shutdown1c = shutdown1.clone();
    tokio::spawn(async move {
        let _ = consumer1
            .start(
                &js1,
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
                &net_id1,
                shutdown1c,
            )
            .await;
    });
    tokio::time::timeout(Duration::from_secs(5), ready_rx1)
        .await
        .expect("ready1")
        .expect("ready1 rx");

    let mut config1 = NatsConfig::from_env();
    config1.net_id = Some(net_id.clone());
    let store1 = NatsEventStore::new(cache1.clone(), ctx.jetstream.clone(), config1, applied_rx1);

    let mut net = PetriNet::new();
    let place_a = net.add_place(Place::internal("place_a"));
    store1
        .append(DomainEvent::NetInitialized { net: net.clone() })
        .await
        .expect("init");
    // Prefix: seq 1,2 (NetInitialized = seq 0).
    for v in 0..2u64 {
        store1
            .append(DomainEvent::TokenCreated {
                token: petri_domain::Token::new(TokenColor::Data(serde_json::json!({ "v": v }))),
                place_id: place_a.clone(),
                place_name: Some("place_a".to_string()),
                workflow_id: None,
                signal_key: None,
                dedup_id: None,
            })
            .await
            .expect("prefix tc");
    }
    // Wait for the consumer to apply the 3 prefix events to cache1.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if cache1.len().await == 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cache1 applied 3");

    // SNAPSHOT taken HERE — captures the prefix (3 events). next_sequence == 3.
    let snapshot = store1.snapshot_inputs().await.into_snapshot();
    assert_eq!(snapshot.next_sequence, 3, "snapshot at 3 events");

    // ── DELTA: two MORE events land while "hibernated" (bridge/signal inject).
    //    These get .sequence 3,4 and chain off the prefix tip. ──
    for v in 100..102u64 {
        store1
            .append(DomainEvent::TokenCreated {
                token: petri_domain::Token::new(TokenColor::Data(serde_json::json!({ "v": v }))),
                place_id: place_a.clone(),
                place_name: Some("place_a".to_string()),
                workflow_id: None,
                signal_key: None,
                dedup_id: None,
            })
            .await
            .expect("delta tc");
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if cache1.len().await == 5 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cache1 applied 5");
    // The real post-delta chain tip (what a correct wake must link to).
    let post_delta_tip = cache1.last_hash();
    assert_ne!(
        post_delta_tip, snapshot.last_hash,
        "delta advanced the chain tip past the snapshot tip"
    );

    shutdown1.cancel();
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(store1);

    // ── Phase 2: wake EXACTLY as net_registry::get_or_create does. ──
    let shutdown2 = tokio_util::sync::CancellationToken::new();
    let cache2 = Arc::new(MemoryEventStore::new());
    let topo2 = Arc::new(MemoryTopologyStore::new());
    let (applied_tx2, applied_rx2) = tokio::sync::watch::channel(0u64);
    let (ready_tx2, ready_rx2) = tokio::sync::oneshot::channel();

    // Registry wake: seed store base, then seed write state from the SNAPSHOT
    // (pre-hydration), then resume the consumer at last_stream_seq + 1.
    cache2.seed_from_snapshot(&snapshot);
    let consumer2 = EventConsumer::new(cache2.clone(), topo2.clone(), applied_tx2, ready_tx2)
        .with_resume_from(snapshot.last_stream_seq);
    let js2 = ctx.jetstream.clone();
    let net_id2 = net_id.clone();
    let shutdown2c = shutdown2.clone();

    let mut config2 = NatsConfig::from_env();
    config2.net_id = Some(net_id.clone());
    let store2 = Arc::new(NatsEventStore::new(
        cache2.clone(),
        ctx.jetstream.clone(),
        config2,
        applied_rx2,
    ));
    // This is the registry's pre-hydration seed (MAJOR 2a) — note it seeds the
    // SNAPSHOT tip/next_seq, which is correct ONLY for an empty delta.
    store2
        .seed_write_state(snapshot.next_sequence, snapshot.last_hash.clone())
        .await;

    tokio::spawn(async move {
        let _ = consumer2
            .start(
                &js2,
                crate::subjects::Subjects::DEFAULT_WORKSPACE,
                &net_id2,
                shutdown2c,
            )
            .await;
    });
    tokio::time::timeout(Duration::from_secs(5), ready_rx2)
        .await
        .expect("ready2")
        .expect("ready2 rx");
    // Delta replayed: cache2 now holds the 2 delta events; its tip is the real
    // post-delta tip and its next sequence is 5.
    assert_eq!(cache2.len().await, 5, "wake replayed the 2-event delta");
    assert_eq!(cache2.last_hash(), post_delta_tip, "cache2 tip == post-delta tip");

    // POST-HYDRATION RE-SEED (the fix in net_registry::get_or_create): after the
    // delta has replayed, re-seed the write authority from the post-replay cache
    // so the next append links to the REAL chain head, not the stale snapshot
    // baseline. Comment this block out to reproduce the pre-fix fork.
    let post_next_seq = store2.current_sequence().await;
    let post_tip = store2.last_hash().await;
    store2.seed_write_state(post_next_seq, post_tip).await;

    // First live append after the non-empty-delta wake.
    let appended = store2
        .append(DomainEvent::TokenCreated {
            token: petri_domain::Token::new(TokenColor::Data(serde_json::json!({ "v": 999 }))),
            place_id: place_a.clone(),
            place_name: Some("place_a".to_string()),
            workflow_id: None,
            signal_key: None,
            dedup_id: None,
        })
        .await
        .expect("post-wake append");

    // The append MUST chain to the POST-DELTA tip, not the stale snapshot tip.
    assert_eq!(
        appended.previous_hash, post_delta_tip,
        "post-wake append must chain to the post-delta tip (the real chain head), \
         NOT the stale snapshot tip — otherwise the hash chain forks: both the \
         first delta event and this append point at snapshot.last_hash"
    );
    assert_ne!(
        appended.previous_hash, snapshot.last_hash,
        "must NOT link to the stale snapshot tip"
    );
    // And the sequence must continue past the delta (5), not collide at 3.
    assert_eq!(
        appended.sequence, 5,
        "post-wake append must continue from the post-delta next_sequence (5), \
         not the stale snapshot.next_sequence (3)"
    );

    shutdown2.cancel();
    ctx.cleanup().await.ok();
}
