//! ROS backend declaration.
//!
//! Interacts with a ROS (Robot Operating System) graph over a rosbridge
//! WebSocket. Unlike the resource-bound query backends (Postgres/Loki/
//! Prometheus), the rosbridge endpoint is **runner-local**: the runner
//! advertises a reachable rosbridge and the executor daemon is configured with
//! its URL (`EXECUTOR_ROS__WS_URL`). There is no workspace resource binding, so
//! `ROS_META.resource_channel == None` and `resource_alias_paths` is empty.
//!
//! ## Reference surfaces
//!
//! The `fields` value (the message / request / goal payload) is a
//! `{{ slug.field }}` template surface — [`ref_scanner`] walks the JSON string
//! leaves so the borrow planner synthesizes read-arcs and stages the producer
//! envelopes. The borrow shape is `Envelope`: the backend resolves the refs
//! itself against the staged `<slug>.json` producer envelopes at execute time.
//!
//! ## Output
//!
//! `output_authoring: Derived`. P2 wires the real typedef→Port mapping: the
//! deriver reads `interface_type` + `operation`, resolves the rosapi typedef
//! snapshot (bundled ground-truth captures, see [`bundled`]), and lowers it to
//! a typed [`Port`] via [`typedef::typedefs_to_port`]. Unknown / empty
//! interfaces fall back to an empty port (the deriver is permissive — it runs
//! per editor keystroke).

use serde_json::{json, Value};

use aithericon_executor_backend_configs::ros::{RosConfig, RosOperation};
use aithericon_executor_domain::InputDeclaration;

pub mod bundled;
pub mod typedef;

use crate::compiler::placeholder_refs::scan_placeholders;
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind, Port, PortField};

use super::{BackendDecl, RefSite, ScanCtx, ValidationCtx, ROS_META};

pub static ROS_DECL: BackendDecl = BackendDecl {
    meta: &ROS_META,
    backend_type: ExecutionBackendType::Ros,
    // No static default fields — the port shape is derived per-config from the
    // ROS interface typedef (`derive_output_port`). The descriptor's
    // `default_output_port` is therefore empty; the editor re-derives on every
    // config change.
    default_output_fields: &[],
    default_editor_config,
    validate,
    ref_scanner: Some(ref_scanner),
    // No workspace resource binding — the rosbridge endpoint is runner-local.
    resource_alias_paths: &[],
    consumes_declared_outputs: false,
    pyi_introspection: false,
    // Envelope: the backend resolves `{{slug.field}}` refs itself against the
    // staged `<slug>.json` producer envelopes (Tera-rendered into the `fields`
    // payload). No per-field placeholder rewrite happens at compile time.
    borrow_shape: super::BorrowShape::Envelope,
    validate_ref_kind: super::accept_any_ref_kind,
    // The output port is config-derived (from the ROS interface type).
    output_authoring: super::OutputAuthoring::Derived,
    derive_output_port: Some(derive_output_port),
    config_schema_fn: config_schema,
    // The rosbridge endpoint is runner-local config, never an inline secret
    // leaf — nothing flat to mask.
    secret_fields: &[],
};

fn config_schema() -> Value {
    super::self_contained_config_schema::<RosConfig>()
}

/// Seed config the editor inserts when a step's backend is first set to ROS.
/// A `publish_topic` to `/turtle1/cmd_vel` with a `geometry_msgs/Twist` payload
/// so the default validates apart from being a no-op until wired.
fn default_editor_config() -> Value {
    json!({
        "operation": "publish_topic",
        "interface_name": "/turtle1/cmd_vel",
        "interface_type": "geometry_msgs/Twist",
        "fields": {
            "linear": { "x": 1.0, "y": 0, "z": 0 },
            "angular": { "x": 0, "y": 0, "z": 0 },
        },
    })
}

fn validate(
    config: &Value,
    _ctx: &ValidationCtx<'_>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    let parsed: RosConfig = serde_json::from_value(config.clone())
        .map_err(|e| CompileError::Validation(format!("invalid ros config: {e}")))?;

    if parsed.interface_name.trim().is_empty() {
        return Err(CompileError::Validation(
            "ros config: interface_name is required".into(),
        ));
    }

    if parsed.interface_type.trim().is_empty() {
        return Err(CompileError::Validation(
            "ros config: interface_type is required".into(),
        ));
    }

    let canonical_config = serde_json::to_value(&parsed)
        .map_err(|e| CompileError::Compilation(format!("failed to serialize ros config: {e}")))?;

    Ok((canonical_config, vec![]))
}

/// Scan the ROS config's `fields` payload for `{{ <head>.<attr> }}`
/// placeholders. Walks the JSON value recursively, scanning every string leaf.
/// Each placeholder becomes an `Envelope` content site — the backend resolves
/// the refs against the staged producer envelopes at execute time, so
/// `is_path_site` is inert.
fn ref_scanner(ctx: &ScanCtx<'_>) -> Vec<RefSite> {
    let mut out: Vec<RefSite> = Vec::new();
    let Some(obj) = ctx.config.as_object() else {
        return out;
    };
    if let Some(fields) = obj.get("fields") {
        scan_value(fields, &mut out);
    }
    out
}

/// Recurse a JSON value, emitting a `RefSite` for every `{{ head.attr }}`
/// placeholder found in any string leaf.
fn scan_value(value: &Value, out: &mut Vec<RefSite>) {
    match value {
        Value::String(s) => {
            for r in scan_placeholders(s) {
                out.push(RefSite {
                    head: r.head,
                    attr: r.attr,
                    is_path_site: false,
                    site_label: "fields".to_string(),
                });
            }
        }
        Value::Array(arr) => {
            for v in arr {
                scan_value(v, out);
            }
        }
        Value::Object(map) => {
            for v in map.values() {
                scan_value(v, out);
            }
        }
        _ => {}
    }
}

/// The lookup key into the bundled typedef registry for a given
/// `interface_type` + `operation`. The key is the rosapi message-details
/// `type` form (no `/msg/` infix; services use `_Request` / `_Response`;
/// action result uses `_Result`).
///
/// - `PublishTopic` carries no derived output (we surface a `{published: Bool}`
///   port instead), so this returns `None`.
/// - `AwaitTopic` derives from the topic message type itself.
/// - `CallService` derives from the service **response** type.
/// - `SendActionGoal` derives from the action **result** type.
///
/// Returns `None` for an empty interface type (mid-edit) so the deriver yields
/// an empty / synthetic port without touching the registry.
fn output_root_key(interface_type: &str, operation: RosOperation) -> Option<String> {
    let base = typedef::normalize_type_name(interface_type);
    if base.is_empty() {
        return None;
    }
    match operation {
        // No ROS-typed response: a synthetic port is surfaced in
        // `derive_output_port` instead, so these return no registry root.
        RosOperation::PublishTopic | RosOperation::MonitorScene => None,
        RosOperation::AwaitTopic => Some(base),
        RosOperation::CallService => Some(format!("{base}_Response")),
        RosOperation::SendActionGoal => Some(format!("{base}_Result")),
    }
}

/// True when `tds` contains a typedef whose (normalized) `type` matches `key`.
/// Used to decide whether the config-embedded `interface_typedefs` actually
/// describes the requested root — present-but-fieldless (an empty ack) still
/// counts as resolved, so this checks for the type's PRESENCE, never its field
/// count. Matches both the literal and `/msg/`-normalized name forms (the same
/// resolution policy `typedef::typedefs_to_port` applies).
fn root_present(tds: &[typedef::TypeDef], key: &str) -> bool {
    let normalized = typedef::normalize_type_name(key);
    tds.iter()
        .any(|td| td.type_name == key || td.type_name == normalized)
}

/// Derive the ROS step's output port from its config.
///
/// Reads `interface_type` + `operation`, resolves the bundled rosapi typedef
/// snapshot for the relevant root (response for a service call, message for an
/// awaited topic, result for an action goal), and lowers it to a typed [`Port`]
/// via [`typedef::typedefs_to_port`].
///
/// Permissive by contract — it runs per editor keystroke, so a partial /
/// unknown / empty config never errors:
/// - `PublishTopic` → a synthetic `{ published: Bool }` port (no payload back).
/// - `SendActionGoal` → the result port plus a synthetic `feedback_count`
///   Number field summarising streamed feedback.
/// - an unknown / unbundled interface type → an empty port.
fn derive_output_port(config: &Value) -> Port {
    let operation = config
        .get("operation")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_value::<RosOperation>(json!(s)).ok())
        .unwrap_or_default();

    let interface_type = config
        .get("interface_type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    // PublishTopic returns no message payload — just an ack that we published.
    if operation == RosOperation::PublishTopic {
        return Port {
            id: "out".into(),
            label: "Output".into(),
            fields: vec![published_field()],
        };
    }

    // MonitorScene drives no ROS request/response — it polls the planning scene
    // and streams it onto a data channel — so it surfaces a synthetic
    // `{ frames_streamed: Number }` count instead of a typed message port.
    if operation == RosOperation::MonitorScene {
        return Port {
            id: "out".into(),
            label: "Output".into(),
            fields: vec![PortField {
                name: "frames_streamed".into(),
                label: "Frames streamed".into(),
                kind: FieldKind::Number,
                required: false,
                options: None,
                description: Some("Number of planning-scene snapshots streamed to the twin.".into()),
                accept: None,
                schema: None,
            }],
        };
    }

    let Some(key) = output_root_key(interface_type, operation) else {
        return empty_port();
    };

    // Prefer the config-embedded `interface_typedefs` (the runner's
    // self-reported catalog, copied into the node config by the editor) so the
    // deriver generalizes to any robot's interfaces — not just the bundled
    // ground-truth captures. We only treat the embedded vec as authoritative
    // when it actually carries the root type: an empty-ack response (e.g.
    // TeleportAbsolute_Response) legitimately yields zero fields, so
    // "resolved" must mean "the root type is present", NOT "fields non-empty".
    // Otherwise fall through to the bundled snapshot.
    let typedefs = match config
        .get("interface_typedefs")
        .cloned()
        .and_then(|v| serde_json::from_value::<Vec<typedef::TypeDef>>(v).ok())
    {
        Some(embedded) if root_present(&embedded, &key) => embedded,
        _ => match bundled::lookup(&key) {
            Some(tds) => tds,
            None => return empty_port(),
        },
    };

    let mut port = typedef::typedefs_to_port(&typedefs, &key, "out", "Output");

    // An action goal also streams feedback — surface a count alongside the
    // result fields so downstream nodes can reference how many were received.
    if operation == RosOperation::SendActionGoal {
        port.fields.push(PortField {
            name: "feedback_count".into(),
            label: "Feedback count".into(),
            kind: FieldKind::Number,
            required: false,
            options: None,
            description: Some("Number of feedback messages received before the result.".into()),
            accept: None,
            schema: None,
        });
    }

    port
}

/// A bare `{ published: Bool }` output port for `PublishTopic`.
fn published_field() -> PortField {
    PortField {
        name: "published".into(),
        label: "Published".into(),
        kind: FieldKind::Bool,
        required: false,
        options: None,
        description: Some("True once the message was published to the topic.".into()),
        accept: None,
        schema: None,
    }
}

fn empty_port() -> Port {
    Port {
        id: "out".into(),
        label: "Output".into(),
        fields: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn validate_cfg(cfg: Value) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
        let files = HashMap::new();
        let ctx = ValidationCtx {
            node_id: "n1",
            node_files: &files,
        };
        validate(&cfg, &ctx)
    }

    #[test]
    fn default_editor_config_validates() {
        validate_cfg(default_editor_config()).expect("default config compiles");
    }

    #[test]
    fn empty_interface_name_rejected() {
        let cfg = json!({
            "interface_name": "  ",
            "interface_type": "geometry_msgs/Twist",
        });
        let err = validate_cfg(cfg).expect_err("empty interface_name rejected");
        assert!(err.to_string().contains("interface_name"), "got: {err}");
    }

    #[test]
    fn empty_interface_type_rejected() {
        let cfg = json!({
            "interface_name": "/turtle1/cmd_vel",
            "interface_type": "",
        });
        let err = validate_cfg(cfg).expect_err("empty interface_type rejected");
        assert!(err.to_string().contains("interface_type"), "got: {err}");
    }

    fn scan(cfg: Value) -> Vec<(String, String, String)> {
        let inline = HashMap::new();
        let ctx = ScanCtx {
            config: &cfg,
            node_id: "n1",
            inline_sources: &inline,
            entrypoint: None,
        };
        ref_scanner(&ctx)
            .into_iter()
            .map(|r| (r.head, r.attr, r.site_label))
            .collect()
    }

    #[test]
    fn scans_nested_field_refs() {
        let cfg = json!({
            "interface_name": "/turtle1/cmd_vel",
            "interface_type": "geometry_msgs/Twist",
            "fields": {
                "linear": { "x": "{{ start.speed }}" },
                "labels": ["{{ pick.label }}"],
            },
        });
        let mut got = scan(cfg);
        got.sort();
        assert_eq!(
            got,
            vec![
                ("pick".into(), "label".into(), "fields".into()),
                ("start".into(), "speed".into(), "fields".into()),
            ]
        );
    }

    #[test]
    fn no_refs_in_static_fields_is_empty() {
        let cfg = json!({
            "interface_name": "/turtle1/cmd_vel",
            "interface_type": "geometry_msgs/Twist",
            "fields": { "linear": { "x": 1.0 } },
        });
        assert!(scan(cfg).is_empty());
    }

    #[test]
    fn derive_publish_topic_is_published_bool() {
        // Default operation is publish_topic — output is a single published Bool.
        let port = derive_output_port(&json!({
            "interface_type": "geometry_msgs/Twist",
        }));
        assert_eq!(port.fields.len(), 1);
        assert_eq!(port.fields[0].name, "published");
        assert_eq!(port.fields[0].kind, FieldKind::Bool);
    }

    #[test]
    fn derive_await_topic_maps_message_port() {
        // AwaitTopic derives from the topic message type itself (Pose → 5 Numbers).
        let port = derive_output_port(&json!({
            "operation": "await_topic",
            "interface_type": "turtlesim/Pose",
        }));
        assert_eq!(port.fields.len(), 5);
        assert!(port.fields.iter().all(|f| f.kind == FieldKind::Number));
    }

    #[test]
    fn derive_call_service_maps_response_port() {
        // CallService derives from the service RESPONSE type (Spawn_Response → name Text).
        let port = derive_output_port(&json!({
            "operation": "call_service",
            "interface_type": "turtlesim/Spawn",
        }));
        assert_eq!(port.fields.len(), 1);
        assert_eq!(port.fields[0].name, "name");
        assert_eq!(port.fields[0].kind, FieldKind::Text);
    }

    #[test]
    fn derive_call_service_empty_ack_response_is_empty() {
        // TeleportAbsolute_Response is an empty ack → empty port.
        let port = derive_output_port(&json!({
            "operation": "call_service",
            "interface_type": "turtlesim/TeleportAbsolute",
        }));
        assert!(port.fields.is_empty());
    }

    #[test]
    fn derive_action_goal_result_plus_feedback_count() {
        // SendActionGoal derives the RESULT port (delta) + a feedback_count Number.
        let port = derive_output_port(&json!({
            "operation": "send_action_goal",
            "interface_type": "turtlesim/RotateAbsolute",
        }));
        assert_eq!(port.fields.len(), 2);
        let delta = port.fields.iter().find(|f| f.name == "delta").unwrap();
        assert_eq!(delta.kind, FieldKind::Number);
        let fc = port
            .fields
            .iter()
            .find(|f| f.name == "feedback_count")
            .unwrap();
        assert_eq!(fc.kind, FieldKind::Number);
    }

    #[test]
    fn derive_uses_embedded_typedefs_without_bundled() {
        // (i) A config-embedded `interface_typedefs` for turtlesim/Pose derives
        // the 5 Number fields WITHOUT touching the bundled registry — the
        // generalized path that lets any robot's catalog drive port derivation.
        // (Same flat-array shape as bundled/turtlesim__Pose.json.)
        let port = derive_output_port(&json!({
            "operation": "await_topic",
            "interface_type": "turtlesim/Pose",
            "interface_typedefs": [
                {
                    "type": "turtlesim/Pose",
                    "fieldnames": ["x", "y", "theta", "linear_velocity", "angular_velocity"],
                    "fieldtypes": ["float", "float", "float", "float", "float"],
                    "fieldarraylen": [-1, -1, -1, -1, -1]
                }
            ],
        }));
        assert_eq!(port.fields.len(), 5);
        assert!(port.fields.iter().all(|f| f.kind == FieldKind::Number));
    }

    #[test]
    fn derive_action_follow_joint_trajectory_result() {
        // control_msgs FollowJointTrajectory: the RESULT type is flat
        // (error_code: int32, error_string: string). Embedded typedefs (as the
        // live runner catalog reports them) derive the result port + a
        // synthetic feedback_count — generalizing the action-derive path to a
        // non-bundled, real industrial-arm action.
        let port = derive_output_port(&json!({
            "operation": "send_action_goal",
            "interface_type": "control_msgs/action/FollowJointTrajectory",
            "interface_typedefs": [
                {
                    "type": "control_msgs/FollowJointTrajectory_Result",
                    "fieldnames": ["error_code", "error_string"],
                    "fieldtypes": ["int32", "string"],
                    "fieldarraylen": [-1, -1]
                }
            ],
        }));
        assert_eq!(port.fields.len(), 3);
        let ec = port.fields.iter().find(|f| f.name == "error_code").unwrap();
        assert_eq!(ec.kind, FieldKind::Number);
        let es = port
            .fields
            .iter()
            .find(|f| f.name == "error_string")
            .unwrap();
        assert_eq!(es.kind, FieldKind::Text);
        let fc = port
            .fields
            .iter()
            .find(|f| f.name == "feedback_count")
            .unwrap();
        assert_eq!(fc.kind, FieldKind::Number);
    }

    #[test]
    fn derive_absent_embedded_typedefs_falls_back_to_bundled() {
        // (ii) No `interface_typedefs` key → the bundled path is unchanged.
        // Pose still derives its 5 Numbers from the bundled snapshot.
        let port = derive_output_port(&json!({
            "operation": "await_topic",
            "interface_type": "turtlesim/Pose",
        }));
        assert_eq!(port.fields.len(), 5);
        assert!(port.fields.iter().all(|f| f.kind == FieldKind::Number));

        // A malformed `interface_typedefs` also falls through to bundled
        // (the deriver is permissive — parse error must not error the derive).
        let port = derive_output_port(&json!({
            "operation": "await_topic",
            "interface_type": "turtlesim/Pose",
            "interface_typedefs": "not-an-array",
        }));
        assert_eq!(port.fields.len(), 5);
    }

    #[test]
    fn derive_embedded_typedef_not_in_bundled_generalizes() {
        // (iii) A type that is NOT in the bundled registry, supplied via the
        // embedded `interface_typedefs`, still derives its fields — proving the
        // generalization to arbitrary robot interfaces.
        let port = derive_output_port(&json!({
            "operation": "await_topic",
            "interface_type": "fake_pkg/Widget",
            "interface_typedefs": [
                {
                    "type": "fake_pkg/Widget",
                    "fieldnames": ["width", "label"],
                    "fieldtypes": ["double", "string"],
                    "fieldarraylen": [-1, -1]
                }
            ],
        }));
        assert_eq!(port.fields.len(), 2);
        let width = port.fields.iter().find(|f| f.name == "width").unwrap();
        assert_eq!(width.kind, FieldKind::Number);
        let label = port.fields.iter().find(|f| f.name == "label").unwrap();
        assert_eq!(label.kind, FieldKind::Text);

        // Sanity: this type is genuinely absent from bundled, so without the
        // embedded typedefs it derives an empty port.
        let bare = derive_output_port(&json!({
            "operation": "await_topic",
            "interface_type": "fake_pkg/Widget",
        }));
        assert!(bare.fields.is_empty());
    }

    #[test]
    fn derive_embedded_empty_ack_response_is_resolved_empty() {
        // An empty-ack response present in the embedded vec (0 fields) must be
        // treated as RESOLVED (root present) — not fall back to bundled. Here
        // there is no bundled entry for fake_pkg, so falling back would also be
        // empty; the assertion below pins that root_present sees the type.
        let embedded = vec![typedef::TypeDef {
            type_name: "fake_pkg/Ack_Response".into(),
            fieldnames: vec![],
            fieldtypes: vec![],
            fieldarraylen: vec![],
        }];
        assert!(root_present(&embedded, "fake_pkg/Ack_Response"));
        assert!(!root_present(&embedded, "fake_pkg/Other_Response"));
    }

    #[test]
    fn derive_unknown_interface_is_permissive_empty() {
        // Unknown interface (mid-edit) → empty port, never an error.
        let port = derive_output_port(&json!({
            "operation": "await_topic",
            "interface_type": "nope/Unknown",
        }));
        assert!(port.fields.is_empty());

        // Empty interface type → empty port.
        let port = derive_output_port(&json!({ "operation": "await_topic" }));
        assert!(port.fields.is_empty());
    }
}
