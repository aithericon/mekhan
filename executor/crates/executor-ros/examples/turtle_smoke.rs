//! Live rosbridge smoke test against a running turtlesim + rosbridge.
//!
//! Drives the three reply-shaped operations end-to-end through the SAME
//! [`RosbridgeClient`] the backend uses:
//!
//!   1. call `/turtle1/teleport_absolute` with `{x:1.0,y:1.0,theta:0.0}`
//!   2. publish a `Twist` to `/turtle1/cmd_vel`
//!   3. subscribe `/turtle1/pose` and print the first `Pose`
//!
//! The teleport moves the turtle to (1,1); the first pose printed after it
//! should reflect x ≈ 1.0, y ≈ 1.0 — proving the dynamic-type client works.
//!
//! Run (turtlesim+rosbridge reachable at ws://localhost:19190):
//!
//! ```bash
//! cd executor && direnv exec . \
//!   cargo run -p aithericon-executor-ros --example turtle_smoke
//! ```

use std::time::Duration;

use aithericon_executor_ros::client::RosbridgeClient;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let url = std::env::var("ROS_WS_URL").unwrap_or_else(|_| "ws://localhost:19190".into());
    println!("connecting to {url}");
    let client = RosbridgeClient::connect(&url).await?;
    println!("connected");

    // 1. Service call: teleport to (1,1,0).
    let resp = client
        .call_service(
            "/turtle1/teleport_absolute",
            &json!({ "x": 1.0, "y": 1.0, "theta": 0.0 }),
            Duration::from_secs(5),
        )
        .await?;
    println!("teleport_absolute response: {resp}");

    // 2. Publish a Twist to /turtle1/cmd_vel.
    client
        .publish(
            "/turtle1/cmd_vel",
            "geometry_msgs/Twist",
            &json!({
                "linear":  { "x": 2.0, "y": 0.0, "z": 0.0 },
                "angular": { "x": 0.0, "y": 0.0, "z": 0.0 }
            }),
        )
        .await?;
    println!("published Twist to /turtle1/cmd_vel");

    // 3. Subscribe /turtle1/pose, await the first message.
    let pose = client
        .await_first("/turtle1/pose", "turtlesim/Pose", Duration::from_secs(5))
        .await?;
    println!("first /turtle1/pose: {pose}");

    let x = pose.get("x").and_then(|v| v.as_f64());
    let y = pose.get("y").and_then(|v| v.as_f64());
    println!("pose x={x:?} y={y:?}  (expect x≈1.0, y≈1.0 after teleport)");

    Ok(())
}
