//! Live ROS action tests — `#[ignore]` by default (require a running
//! turtlesim + rosbridge_suite).
//!
//! ## How to run
//!
//! Bring up the dev ROS stack (turtlesim + rosbridge on a slot-aware host
//! port) and point this test at the bridge:
//!
//! ```bash
//! just dev ros-up                       # builds + runs turtlesim + rosbridge
//! # the bridge URL is the slot-aware EXECUTOR_ROS__WS_URL (e.g. ws://localhost:19190)
//! ROS_TEST_WS_URL=ws://localhost:19190 \
//!   cargo test -p aithericon-executor-ros --test live_action -- --ignored --nocapture
//! ```
//!
//! Without `ROS_TEST_WS_URL` the test defaults to `ws://localhost:9090` (the
//! plain rosbridge default). Both tests are `#[ignore]` so the normal
//! `cargo test` lane stays fully offline.

use std::time::Duration;

use aithericon_executor_ros::client::RosbridgeClient;
use serde_json::json;

fn ws_url() -> String {
    std::env::var("ROS_TEST_WS_URL").unwrap_or_else(|_| "ws://localhost:9090".to_string())
}

const ACTION: &str = "/turtle1/rotate_absolute";
const ACTION_TYPE: &str = "turtlesim/action/RotateAbsolute";

/// Happy path: a small rotate goal streams ≥1 distinct feedback and resolves
/// with a SUCCEEDED result carrying `delta`. Proves the full action protocol
/// (send_action_goal → action_feedback → action_result) end-to-end.
#[tokio::test]
#[ignore = "requires a live turtlesim + rosbridge (just dev ros-up)"]
async fn live_rotate_streams_feedback_and_resolves() {
    let client = RosbridgeClient::connect(&ws_url())
        .await
        .expect("connect to rosbridge");

    // Teleport to theta=0 first so a 0.4 rad target is always a real rotation.
    client
        .call_service(
            "/turtle1/teleport_absolute",
            &json!({ "x": 5.5, "y": 5.5, "theta": 0.0 }),
            Duration::from_secs(5),
        )
        .await
        .expect("teleport");

    let (_id, mut feedback_rx, result_rx) = client
        .send_action_goal(ACTION, ACTION_TYPE, &json!({ "theta": 0.4 }))
        .await
        .expect("send_action_goal");

    let mut distinct = 0usize;
    let mut last: Option<serde_json::Value> = None;
    let result = {
        tokio::pin!(result_rx);
        loop {
            tokio::select! {
                fb = feedback_rx.recv() => {
                    if let Some(v) = fb {
                        if last.as_ref() != Some(&v) {
                            distinct += 1;
                            last = Some(v);
                        }
                    }
                }
                res = &mut result_rx => break res.expect("action result resolves"),
            }
        }
    };

    assert!(result.ok, "rotate should succeed: {result:?}");
    assert_eq!(result.status, 4, "GoalStatus should be SUCCEEDED (4)");
    assert!(
        result.values.get("delta").is_some(),
        "result carries delta: {:?}",
        result.values
    );
    assert!(distinct >= 1, "at least one distinct feedback streamed");
    eprintln!(
        "live rotate ok: {distinct} distinct feedback(s), delta={:?}",
        result.values.get("delta")
    );
}

/// Cancel mid-rotate: dispatch a near-2π rotation (long-running, hundreds of
/// feedbacks), wait for the first feedback, then `cancel_action_goal`. Asserts
/// safe-stop: the goal does NOT resolve as SUCCEEDED — either the result frame
/// reports an aborted/canceled status (`ok == false` or `status != 4`) or the
/// routes are dropped so the awaiter wakes without a success.
#[tokio::test]
#[ignore = "requires a live turtlesim + rosbridge (just dev ros-up)"]
async fn live_cancel_mid_rotate_safe_stops() {
    let client = RosbridgeClient::connect(&ws_url())
        .await
        .expect("connect to rosbridge");

    client
        .call_service(
            "/turtle1/teleport_absolute",
            &json!({ "x": 5.5, "y": 5.5, "theta": 0.0 }),
            Duration::from_secs(5),
        )
        .await
        .expect("teleport");

    // A near-full rotation gives us a long window to cancel mid-flight.
    let (goal_id, mut feedback_rx, result_rx) = client
        .send_action_goal(ACTION, ACTION_TYPE, &json!({ "theta": 3.1 }))
        .await
        .expect("send_action_goal");

    // Wait for the first feedback so we're genuinely mid-rotation.
    let first = tokio::time::timeout(Duration::from_secs(5), feedback_rx.recv())
        .await
        .expect("first feedback arrives before timeout");
    assert!(first.is_some(), "received a feedback frame");

    // Safe-stop.
    client
        .cancel_action_goal(ACTION, &goal_id)
        .await
        .expect("cancel_action_goal sent");

    // The goal must NOT resolve as a clean success. cancel drops the result
    // route, so the oneshot most likely errors (Closed); if rosbridge still
    // delivers a terminal result frame for the canceled goal it must report a
    // non-SUCCEEDED status.
    let outcome = tokio::time::timeout(Duration::from_secs(5), result_rx).await;
    match outcome {
        Ok(Ok(result)) => {
            assert!(
                !result.ok || result.status != 4,
                "canceled goal must not report SUCCEEDED: {result:?}"
            );
        }
        Ok(Err(_)) | Err(_) => {
            // Result route dropped / no terminal success delivered — safe-stop.
        }
    }
    eprintln!("live cancel ok: goal safe-stopped");
}
