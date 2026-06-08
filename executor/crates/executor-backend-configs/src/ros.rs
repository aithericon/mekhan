//! Wire-format config types for the ROS backend.
//!
//! Deserialize-only mirrors of what `ExecutionSpec.config` carries. The
//! executor-ros crate consumes this for runtime execution; the compiler
//! consumes it for compile-time validation. Single source of truth for the
//! JSON shape — drift between authoring and execution is a build error, not
//! a runtime surprise.
//!
//! The ROS connection (the rosbridge WebSocket endpoint) is **runner-local**:
//! the runner advertises a reachable rosbridge, the executor daemon is
//! configured with its URL (`EXECUTOR_ROS__WS_URL`). There is no workspace
//! resource binding. The `fields` value carries the goal / request / message
//! field values and may contain `{{slug.field}}` placeholders resolved at
//! runtime against the staged producer envelopes.
//!
//! (P1 stub — the rosbridge client + typedef→Port mapper land in P2.)

use serde::{Deserialize, Serialize};

/// Which ROS interaction the step performs.
///
/// `PublishTopic` (the default) publishes a single message to a topic.
/// `CallService` performs a request/response service call. `AwaitTopic`
/// blocks for the next message on a topic. `SendActionGoal` dispatches a
/// goal to an action server. `MonitorScene` polls move_group's
/// `/get_planning_scene` on a cadence for a bounded duration and streams each
/// snapshot onto a DATA channel — a live planning-scene twin DECOUPLED from
/// any single motion, so one monitor can watch a whole multi-step run. This is
/// the source of truth for which rosbridge op the backend issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RosOperation {
    #[default]
    PublishTopic,
    CallService,
    AwaitTopic,
    SendActionGoal,
    MonitorScene,
}

/// Configuration for a single ROS interaction job.
///
/// Deserialised from `ExecutionSpec.config` at runtime by the executor;
/// validated against this shape at compile-time by the mekhan compiler.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct RosConfig {
    /// Which ROS interaction to perform. Defaults to `PublishTopic`.
    #[serde(default)]
    pub operation: RosOperation,

    /// The ROS interface name — the topic / service / action name, e.g.
    /// `"/turtle1/cmd_vel"`. Required.
    pub interface_name: String,

    /// The ROS interface type, e.g. `"geometry_msgs/Twist"`. Required.
    pub interface_type: String,

    /// The goal / request / message field values.
    ///
    /// May contain `{{slug.field}}` placeholders resolved at runtime against
    /// the staged producer envelopes. Defaults to a JSON null when absent.
    #[serde(default)]
    pub fields: serde_json::Value,

    /// Per-request timeout in milliseconds. Defaults to 30000.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,

    /// When set on a `send_action_goal` node that declares a DATA `out` channel,
    /// the action ALSO polls move_group's `/get_planning_scene` every this-many
    /// milliseconds during the motion and streams each scene snapshot (slim
    /// NDJSON: joints + collision objects + attached objects) onto the data
    /// channel — driving a live planning-scene digital twin. `None`/absent ⇒ the
    /// data channel carries the default per-feedback joint-state stream instead.
    #[serde(default)]
    pub scene_stream_ms: Option<u64>,

    /// How long a `monitor_scene` op keeps polling `/get_planning_scene` (in
    /// milliseconds) before it closes its data channel. Decouples the
    /// planning-scene twin from any single motion: a monitor sized to outlast
    /// the run streams the WHOLE multi-step session (arm picking/placing several
    /// samples) to one twin. `None`/absent ⇒ the op runs until its `timeout_ms`
    /// (so it always terminates). With `stop_topic` set this is only a FAILSAFE
    /// ceiling — the monitor normally stops the moment the work branch signals
    /// it. Ignored by the non-monitor operations.
    #[serde(default)]
    pub scene_duration_ms: Option<u64>,

    /// `monitor_scene` STOP signal: a ROS topic the monitor subscribes to and
    /// breaks its poll loop on the FIRST message it receives, closing the data
    /// channel cleanly. This makes a continuous monitor's lifetime track the
    /// WORK it watches instead of a guessed `scene_duration_ms` timer — the work
    /// branch publishes one `std_msgs/msg/Bool` here when its last step finishes,
    /// the monitor stops within one poll, and the sibling `join` fires
    /// immediately (no idle wait for the timer). `None`/absent ⇒ the monitor only
    /// stops on `scene_duration_ms`/`timeout_ms`/cancel. Ignored by the
    /// non-monitor operations.
    #[serde(default)]
    pub stop_topic: Option<String>,
}

fn default_timeout_ms() -> u64 {
    30_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ros_config_round_trips_through_json() {
        let cfg = RosConfig {
            operation: RosOperation::PublishTopic,
            interface_name: "/turtle1/cmd_vel".into(),
            interface_type: "geometry_msgs/Twist".into(),
            fields: serde_json::json!({ "linear": { "x": 1.0 } }),
            timeout_ms: 15_000,
            scene_stream_ms: None,
            scene_duration_ms: None,
            stop_topic: None,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let de: RosConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(de.operation, RosOperation::PublishTopic);
        assert_eq!(de.interface_name, "/turtle1/cmd_vel");
        assert_eq!(de.interface_type, "geometry_msgs/Twist");
        assert_eq!(de.fields["linear"]["x"], 1.0);
        assert_eq!(de.timeout_ms, 15_000);
    }

    #[test]
    fn ros_config_minimal_uses_defaults() {
        let json = r#"{
            "interface_name": "/turtle1/cmd_vel",
            "interface_type": "geometry_msgs/Twist"
        }"#;
        let cfg: RosConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.operation, RosOperation::PublishTopic);
        assert!(cfg.fields.is_null());
        assert_eq!(cfg.timeout_ms, 30_000);
    }

    #[test]
    fn call_service_operation_parses() {
        let json = r#"{
            "operation": "call_service",
            "interface_name": "/spawn",
            "interface_type": "turtlesim/Spawn"
        }"#;
        let cfg: RosConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.operation, RosOperation::CallService);
    }

    #[test]
    fn scene_stream_ms_parses() {
        // Present → Some(ms).
        let json = r#"{
            "operation": "send_action_goal",
            "interface_name": "/x",
            "interface_type": "y",
            "scene_stream_ms": 200
        }"#;
        let cfg: RosConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.scene_stream_ms, Some(200));

        // Omitted → None (additive, deserialize-tolerant).
        let json = r#"{
            "operation": "send_action_goal",
            "interface_name": "/x",
            "interface_type": "y"
        }"#;
        let cfg: RosConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.scene_stream_ms, None);
    }

    #[test]
    fn monitor_scene_op_and_duration_parse() {
        let json = r#"{
            "operation": "monitor_scene",
            "interface_name": "/get_planning_scene",
            "interface_type": "moveit_msgs/srv/GetPlanningScene",
            "scene_stream_ms": 200,
            "scene_duration_ms": 120000
        }"#;
        let cfg: RosConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.operation, RosOperation::MonitorScene);
        assert_eq!(cfg.scene_stream_ms, Some(200));
        assert_eq!(cfg.scene_duration_ms, Some(120_000));

        // Duration omitted → None (falls back to timeout_ms at runtime).
        let json = r#"{
            "operation": "monitor_scene",
            "interface_name": "/get_planning_scene",
            "interface_type": "moveit_msgs/srv/GetPlanningScene"
        }"#;
        let cfg: RosConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.scene_duration_ms, None);
    }

    #[test]
    fn stop_topic_parses() {
        // Present → Some(topic): the monitor stops on this topic's first message.
        let json = r#"{
            "operation": "monitor_scene",
            "interface_name": "/get_planning_scene",
            "interface_type": "moveit_msgs/srv/GetPlanningScene",
            "stop_topic": "/aithericon/monitor_stop"
        }"#;
        let cfg: RosConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.stop_topic.as_deref(), Some("/aithericon/monitor_stop"));

        // Omitted → None (additive, deserialize-tolerant).
        let json = r#"{
            "operation": "monitor_scene",
            "interface_name": "/get_planning_scene",
            "interface_type": "moveit_msgs/srv/GetPlanningScene"
        }"#;
        let cfg: RosConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.stop_topic, None);
    }
}
