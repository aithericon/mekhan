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
/// goal to an action server. This is the source of truth for which rosbridge
/// op the backend issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RosOperation {
    #[default]
    PublishTopic,
    CallService,
    AwaitTopic,
    SendActionGoal,
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
}
