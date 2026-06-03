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
//! `output_authoring: Derived`. P1 STUB — the deriver returns an empty port.
//! The real typedef→Port mapping (from the ROS interface type definition)
//! lands in P2.

use serde_json::{json, Value};

use aithericon_executor_backend_configs::ros::RosConfig;
use aithericon_executor_domain::InputDeclaration;

use crate::compiler::placeholder_refs::scan_placeholders;
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, Port};

use super::{BackendDecl, RefSite, ScanCtx, ValidationCtx, ROS_META};

pub static ROS_DECL: BackendDecl = BackendDecl {
    meta: &ROS_META,
    backend_type: ExecutionBackendType::Ros,
    // P1 stub: empty default port. The Derived deriver below also returns an
    // empty port until P2 wires the typedef→Port mapping.
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
    // The output port is config-derived (from the ROS interface type). P1 stub
    // returns an empty port; P2 maps the typedef.
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

/// Derive the ROS step's output port. P1 STUB — returns an empty port. The
/// real typedef→Port mapping (from the ROS interface type definition) lands in
/// P2.
fn derive_output_port(_config: &Value) -> Port {
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
    fn derive_output_is_empty_stub() {
        let port = derive_output_port(&json!({}));
        assert!(port.fields.is_empty());
    }
}
