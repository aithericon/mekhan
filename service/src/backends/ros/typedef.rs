//! rosapi TypeDef → service [`Port`] mapper.
//!
//! `rosbridge`'s `/rosapi/message_details` (and `/rosapi/service_request_details`
//! / `..._response_details`) return a **flat array** of [`TypeDef`] entries: the
//! ROOT typedef plus every nested typedef referenced transitively, each keyed by
//! its `type`. This module resolves a chosen root type against that array and
//! lowers it into a [`Port`] of [`PortField`]s, applying the rosapi PRIMITIVE
//! VOCABULARY (which is NOT the `.msg` vocabulary — `float64`/`float32` arrive
//! as `double`/`float` here).
//!
//! ## Primitive vocabulary (rosapi `fieldtypes` strings → [`FieldKind`])
//!
//! | rosapi type                                   | FieldKind | notes |
//! |-----------------------------------------------|-----------|-------|
//! | `double`, `float`, `float64`, `float32`       | `Number`  | rosapi may rename (float64→double) or keep the original name |
//! | `int8`…`int64`, `uint8`…`uint64`, `byte`, `char`, `octet` | `Number` | |
//! | `bool`, `boolean`                             | `Bool`    | Jazzy reports `boolean` (IDL name) |
//! | `string`, `wstring`                           | `Text`    | |
//! | `time`, `duration`, `builtin_interfaces/*`    | `Json`    | opaque stamp |
//! | anything containing `/` (a nested message)    | `Json`    | + recursive JSON-Schema `schema` override |
//! | `fieldarraylen[i] != -1` (array field)        | `Json`    | + array JSON-Schema `schema` override |
//!
//! Nested-message and array fields carry a JSON-Schema `schema` override on the
//! `PortField` (the same mechanism Postgres uses to surface projected columns),
//! so the editor's variable picker can descend into the structure even though
//! the flat `kind` vocabulary collapses them all to `Json`.

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::models::template::{FieldKind, Port, PortField};

/// One rosapi typedef entry as returned in a `message_details` /
/// `service_*_details` array. `constnames` / `constvalues` are present on the
/// wire but ignored (the `#[serde(default)]` lets them be absent too).
#[derive(Debug, Clone, Deserialize)]
pub struct TypeDef {
    /// Fully-qualified `pkg/Type` (already in message-details form — no
    /// `/msg/` infix). For services this is `pkg/Type_Request` /
    /// `pkg/Type_Response`.
    #[serde(rename = "type")]
    pub type_name: String,
    #[serde(default)]
    pub fieldnames: Vec<String>,
    #[serde(default)]
    pub fieldtypes: Vec<String>,
    #[serde(default)]
    pub fieldarraylen: Vec<i64>,
}

/// Normalize a topic/service/action type name into the form rosapi uses inside
/// `message_details` `type` keys: strip the `/msg/`, `/srv/`, `/action/`
/// infixes so `"geometry_msgs/msg/Twist"` and `"geometry_msgs/Twist"` both
/// collapse to `"geometry_msgs/Twist"`. Service request/response suffixes
/// (`_Request` / `_Response`) are left intact.
pub fn normalize_type_name(raw: &str) -> String {
    let raw = raw.trim();
    for infix in ["/msg/", "/srv/", "/action/"] {
        if let Some(idx) = raw.find(infix) {
            let pkg = &raw[..idx];
            let rest = &raw[idx + infix.len()..];
            return format!("{pkg}/{rest}");
        }
    }
    raw.to_string()
}

/// Resolve the typedef whose `type` equals `type_name` from a typedef list.
/// Tries the literal name first, then the `/msg/`-normalized form (so callers
/// can pass either shape).
fn resolve<'a>(typedefs: &'a [TypeDef], type_name: &str) -> Option<&'a TypeDef> {
    if let Some(td) = typedefs.iter().find(|t| t.type_name == type_name) {
        return Some(td);
    }
    let normalized = normalize_type_name(type_name);
    typedefs.iter().find(|t| t.type_name == normalized)
}

/// True for the rosapi primitive scalar vocabulary (no `/`, not an
/// array). These map directly to a scalar [`FieldKind`].
fn primitive_kind(rosapi_type: &str) -> Option<FieldKind> {
    match rosapi_type {
        // Numeric. rosapi may report floats renamed (float64→double,
        // float32→float) OR under their original names depending on the rosidl
        // version, so accept both. `octet`/`byte`/`char` are byte-width ints.
        "double" | "float" | "float64" | "float32" => Some(FieldKind::Number),
        "int8" | "int16" | "int32" | "int64" | "uint8" | "uint16" | "uint32" | "uint64"
        | "byte" | "char" | "octet" => Some(FieldKind::Number),
        // rosapi reports bool as `boolean` (the IDL name) on Jazzy; older/other
        // versions use `bool`. Accept both.
        "bool" | "boolean" => Some(FieldKind::Bool),
        "string" | "wstring" => Some(FieldKind::Text),
        // time / duration are opaque stamps → Json.
        "time" | "duration" => Some(FieldKind::Json),
        _ => None,
    }
}

/// JSON-Schema for a primitive ROS leaf appearing INSIDE a schema override
/// (an array element or a nested-message field). Numbers are emitted as
/// **nullable** (`["number", "null"]`): rosbridge serializes the IEEE-754
/// specials ROS floats routinely carry — `NaN` / `±Inf` (e.g. the
/// uninitialized `effort` array a ros2_control fake JointState publishes) — as
/// JSON `null`, and a strict `{"type":"number"}` would reject those otherwise-
/// valid messages mid-net at the runtime `Data__*` schema gate. Bool / text /
/// opaque-json keep their base schema (those wire types are never `null`).
fn leaf_schema(kind: FieldKind) -> Value {
    match kind {
        FieldKind::Number => json!({ "type": ["number", "null"] }),
        other => other.base_schema(),
    }
}

/// JSON-Schema for a single (non-array) rosapi field type. Primitives get their
/// bare scalar schema; nested messages get a recursively-resolved object schema
/// (cycle-guarded via `in_flight`); `builtin_interfaces/*` and unknown `/`
/// types collapse to a permissive `{}`.
fn type_schema(rosapi_type: &str, typedefs: &[TypeDef], in_flight: &mut Vec<String>) -> Value {
    if let Some(kind) = primitive_kind(rosapi_type) {
        return leaf_schema(kind);
    }
    // A nested message type (contains `/`). builtin_interfaces/* are opaque
    // stamps; everything else we resolve recursively from the typedef list.
    if rosapi_type.starts_with("builtin_interfaces/") {
        return json!({});
    }
    if rosapi_type.contains('/') {
        if let Some(nested) = resolve(typedefs, rosapi_type) {
            return object_schema(nested, typedefs, in_flight);
        }
        // Unresolvable nested type — permissive object.
        return json!({ "type": "object" });
    }
    // Unknown bare scalar — permissive.
    json!({})
}

/// Build a JSON-Schema `object` for a typedef's fields, recursing into nested
/// messages and wrapping array fields. Cycle-guarded: a type already on the
/// resolution stack collapses to a permissive `{}` so a self-referential
/// message can't blow the stack.
fn object_schema(td: &TypeDef, typedefs: &[TypeDef], in_flight: &mut Vec<String>) -> Value {
    if in_flight.contains(&td.type_name) {
        return json!({ "type": "object" });
    }
    in_flight.push(td.type_name.clone());

    let mut props = Map::new();
    for (i, name) in td.fieldnames.iter().enumerate() {
        let ftype = td.fieldtypes.get(i).map(String::as_str).unwrap_or("");
        let is_array = td.fieldarraylen.get(i).copied().unwrap_or(-1) != -1;
        let elem = type_schema(ftype, typedefs, in_flight);
        let schema = if is_array {
            json!({ "type": "array", "items": elem })
        } else {
            elem
        };
        props.insert(name.clone(), schema);
    }

    in_flight.pop();

    json!({
        "type": "object",
        "properties": Value::Object(props),
    })
}

/// Map a rosapi typedef list + a chosen root type name into a service [`Port`].
///
/// Each top-level field of the root typedef becomes one [`PortField`]:
/// - scalar primitives → the matching scalar [`FieldKind`] (no schema override);
/// - nested messages (`pkg/Type`) → [`FieldKind::Json`] with a recursively-built
///   object `schema` override;
/// - array fields (`fieldarraylen != -1`) → [`FieldKind::Json`] with an array
///   `schema` override wrapping the element schema;
/// - `time` / `duration` / `builtin_interfaces/*` → [`FieldKind::Json`].
///
/// An empty / unresolvable root (e.g. an `_Response` ack with no fields) yields
/// a [`Port`] with no fields. `port_id` / `port_label` set the wrapper port's
/// identity (the deriver uses `"out"` / `"Output"`).
pub fn typedefs_to_port(
    typedefs: &[TypeDef],
    root_type: &str,
    port_id: &str,
    port_label: &str,
) -> Port {
    let mut fields = Vec::new();

    if let Some(root) = resolve(typedefs, root_type) {
        for (i, name) in root.fieldnames.iter().enumerate() {
            let ftype = root.fieldtypes.get(i).map(String::as_str).unwrap_or("");
            let is_array = root.fieldarraylen.get(i).copied().unwrap_or(-1) != -1;

            let scalar_kind = (!is_array)
                .then(|| primitive_kind(ftype))
                .flatten()
                .filter(|k| !matches!(k, FieldKind::Json));

            let (kind, schema) = match scalar_kind {
                // A plain scalar primitive (not time/duration which map to Json):
                // map to the scalar FieldKind, no schema override.
                Some(kind) => (kind, None),
                // Array, nested message, or opaque stamp → Json + schema override.
                None => {
                    let mut in_flight = Vec::new();
                    let elem = type_schema(ftype, typedefs, &mut in_flight);
                    let schema = if is_array {
                        json!({ "type": "array", "items": elem })
                    } else {
                        elem
                    };
                    (FieldKind::Json, Some(schema))
                }
            };

            fields.push(PortField {
                name: name.clone(),
                label: name.clone(),
                kind,
                required: false,
                options: None,
                description: None,
                accept: None,
                schema,
            });
        }
    }

    Port {
        id: port_id.to_string(),
        label: port_label.to_string(),
        fields,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Vec<TypeDef> {
        serde_json::from_str(json).expect("typedef snapshot parses")
    }

    #[test]
    fn pose_maps_to_five_number_fields() {
        let td = parse(include_str!("bundled/turtlesim__Pose.json"));
        let port = typedefs_to_port(&td, "turtlesim/Pose", "out", "Output");
        assert_eq!(port.fields.len(), 5);
        for f in &port.fields {
            assert_eq!(f.kind, FieldKind::Number, "{} should be Number", f.name);
            assert!(f.schema.is_none(), "{} scalar carries no schema", f.name);
        }
        let names: Vec<&str> = port.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["x", "y", "theta", "linear_velocity", "angular_velocity"]
        );
    }

    #[test]
    fn twist_maps_to_nested_linear_angular() {
        let td = parse(include_str!("bundled/geometry_msgs__Twist.json"));
        let port = typedefs_to_port(&td, "geometry_msgs/Twist", "out", "Output");
        assert_eq!(port.fields.len(), 2);

        let linear = port.fields.iter().find(|f| f.name == "linear").unwrap();
        assert_eq!(linear.kind, FieldKind::Json, "nested message → Json");
        let schema = linear
            .schema
            .as_ref()
            .expect("nested message carries schema");
        assert_eq!(schema["type"], "object");
        // Vector3's three double fields resolve to nullable-Number schemas
        // (rosbridge renders NaN/±Inf as JSON null — see `leaf_schema`).
        let props = &schema["properties"];
        assert_eq!(props["x"], json!({ "type": ["number", "null"] }));
        assert_eq!(props["y"], json!({ "type": ["number", "null"] }));
        assert_eq!(props["z"], json!({ "type": ["number", "null"] }));

        let angular = port.fields.iter().find(|f| f.name == "angular").unwrap();
        assert_eq!(angular.kind, FieldKind::Json);
        assert!(angular.schema.is_some());
    }

    #[test]
    fn teleport_response_is_empty_ack() {
        let td = parse(include_str!(
            "bundled/turtlesim__TeleportAbsolute_Response.json"
        ));
        let port = typedefs_to_port(&td, "turtlesim/TeleportAbsolute_Response", "out", "Output");
        assert!(port.fields.is_empty(), "empty ack response → empty port");
    }

    #[test]
    fn spawn_response_has_one_text_field() {
        let td = parse(include_str!("bundled/turtlesim__Spawn_Response.json"));
        let port = typedefs_to_port(&td, "turtlesim/Spawn_Response", "out", "Output");
        assert_eq!(port.fields.len(), 1);
        let name = &port.fields[0];
        assert_eq!(name.name, "name");
        assert_eq!(name.kind, FieldKind::Text, "string → Text");
        assert!(name.schema.is_none());
    }

    #[test]
    fn spawn_request_mixes_float_and_string() {
        let td = parse(include_str!("bundled/turtlesim__Spawn_Request.json"));
        let port = typedefs_to_port(&td, "turtlesim/Spawn_Request", "out", "Output");
        assert_eq!(port.fields.len(), 4);
        let by_name = |n: &str| port.fields.iter().find(|f| f.name == n).unwrap();
        // x/y/theta are rosapi `float` → Number.
        assert_eq!(by_name("x").kind, FieldKind::Number);
        assert_eq!(by_name("y").kind, FieldKind::Number);
        assert_eq!(by_name("theta").kind, FieldKind::Number);
        // name is `string` → Text.
        assert_eq!(by_name("name").kind, FieldKind::Text);
    }

    #[test]
    fn double_and_float_both_map_to_number() {
        // The rosapi-specific vocabulary: float64→double, float32→float, both Number.
        assert_eq!(primitive_kind("double"), Some(FieldKind::Number));
        assert_eq!(primitive_kind("float"), Some(FieldKind::Number));
        // …but rosapi may also keep the original float names.
        assert_eq!(primitive_kind("float64"), Some(FieldKind::Number));
        assert_eq!(primitive_kind("float32"), Some(FieldKind::Number));
        assert_eq!(primitive_kind("int32"), Some(FieldKind::Number));
        assert_eq!(primitive_kind("uint8"), Some(FieldKind::Number));
        assert_eq!(primitive_kind("byte"), Some(FieldKind::Number));
        assert_eq!(primitive_kind("char"), Some(FieldKind::Number));
        assert_eq!(primitive_kind("octet"), Some(FieldKind::Number));
        // Jazzy rosapi reports bool as `boolean` (the IDL name); accept both. The
        // xArm's PlanJoint/PlanExec services are the first ROS types with a bool
        // field, so this arm was previously unexercised against real rosapi.
        assert_eq!(primitive_kind("bool"), Some(FieldKind::Bool));
        assert_eq!(primitive_kind("boolean"), Some(FieldKind::Bool));
        assert_eq!(primitive_kind("string"), Some(FieldKind::Text));
        assert_eq!(primitive_kind("wstring"), Some(FieldKind::Text));
        assert_eq!(primitive_kind("time"), Some(FieldKind::Json));
        assert_eq!(primitive_kind("duration"), Some(FieldKind::Json));
        // A nested message is NOT a primitive.
        assert_eq!(primitive_kind("geometry_msgs/Vector3"), None);
    }

    #[test]
    fn normalize_strips_msg_srv_action_infixes() {
        assert_eq!(
            normalize_type_name("geometry_msgs/msg/Twist"),
            "geometry_msgs/Twist"
        );
        assert_eq!(
            normalize_type_name("turtlesim/srv/TeleportAbsolute"),
            "turtlesim/TeleportAbsolute"
        );
        assert_eq!(
            normalize_type_name("turtlesim/action/RotateAbsolute"),
            "turtlesim/RotateAbsolute"
        );
        // Already-normalized stays put.
        assert_eq!(
            normalize_type_name("geometry_msgs/Twist"),
            "geometry_msgs/Twist"
        );
    }

    #[test]
    fn resolve_accepts_either_name_form() {
        let td = parse(include_str!("bundled/geometry_msgs__Twist.json"));
        // Both the slash-msg form and the bare form resolve.
        let port = typedefs_to_port(&td, "geometry_msgs/msg/Twist", "out", "Output");
        assert_eq!(port.fields.len(), 2);
    }

    #[test]
    fn unresolvable_root_yields_empty_port() {
        let td = parse(include_str!("bundled/turtlesim__Pose.json"));
        let port = typedefs_to_port(&td, "nonexistent/Type", "out", "Output");
        assert!(port.fields.is_empty());
    }

    #[test]
    fn rotate_absolute_result_single_number() {
        let td = parse(include_str!(
            "bundled/turtlesim__RotateAbsolute_Result.json"
        ));
        let port = typedefs_to_port(&td, "turtlesim/RotateAbsolute_Result", "out", "Output");
        assert_eq!(port.fields.len(), 1);
        assert_eq!(port.fields[0].name, "delta");
        assert_eq!(port.fields[0].kind, FieldKind::Number);
    }

    #[test]
    fn array_field_maps_to_json_with_array_schema() {
        // xarm_msgs/PlanJoint request: `float64[] target` — the FIRST ROS type
        // with an array field (every turtle type was flat scalars). rosapi
        // renames float64 → double and reports a variable-length array as
        // fieldarraylen 0 (!= -1 → array). The array path was code-complete but
        // unexercised until the xArm; this locks it.
        let td = parse(
            r#"[
              {
                "type": "xarm_msgs/PlanJoint_Request",
                "fieldnames": ["target"],
                "fieldtypes": ["double"],
                "fieldarraylen": [0]
              }
            ]"#,
        );
        let port = typedefs_to_port(&td, "xarm_msgs/PlanJoint_Request", "out", "Output");
        assert_eq!(port.fields.len(), 1);
        let target = &port.fields[0];
        assert_eq!(target.name, "target");
        assert_eq!(target.kind, FieldKind::Json, "array field → Json");
        let schema = target
            .schema
            .as_ref()
            .expect("array field carries a schema");
        assert_eq!(schema["type"], "array");
        assert_eq!(
            schema["items"],
            json!({ "type": ["number", "null"] }),
            "double[] items are nullable numbers (rosbridge renders NaN/±Inf as null)"
        );
    }

    #[test]
    fn joint_state_arrays_and_nested_header() {
        // sensor_msgs/JointState: a nested Header + a string array + three
        // primitive arrays — the `await_topic /joint_states` output port for the
        // xArm demo. Exercises array-of-primitive AND nested-message resolution
        // in one port.
        let td = parse(
            r#"[
              {
                "type": "sensor_msgs/JointState",
                "fieldnames": ["header", "name", "position", "velocity", "effort"],
                "fieldtypes": ["std_msgs/Header", "string", "double", "double", "double"],
                "fieldarraylen": [-1, 0, 0, 0, 0]
              },
              {
                "type": "std_msgs/Header",
                "fieldnames": ["stamp", "frame_id"],
                "fieldtypes": ["builtin_interfaces/Time", "string"],
                "fieldarraylen": [-1, -1]
              }
            ]"#,
        );
        let port = typedefs_to_port(&td, "sensor_msgs/JointState", "out", "Output");
        assert_eq!(port.fields.len(), 5);
        let by = |n: &str| port.fields.iter().find(|f| f.name == n).unwrap();

        // Every JointState top-level field is an array or nested message → Json
        // with a schema override the variable picker can descend into.
        for f in &port.fields {
            assert_eq!(f.kind, FieldKind::Json, "{} → Json", f.name);
            assert!(f.schema.is_some(), "{} carries a schema override", f.name);
        }

        // position: double[] → array of nullable number (NaN/±Inf → null).
        let pos = by("position").schema.clone().unwrap();
        assert_eq!(pos["type"], "array");
        assert_eq!(pos["items"], json!({ "type": ["number", "null"] }));

        // name: string[] → array of string.
        let name = by("name").schema.clone().unwrap();
        assert_eq!(name["type"], "array");
        assert_eq!(name["items"], json!({ "type": "string" }));

        // header: nested std_msgs/Header → object schema with frame_id (string)
        // and an opaque builtin_interfaces/Time stamp ({}).
        let header = by("header").schema.clone().unwrap();
        assert_eq!(header["type"], "object");
        assert_eq!(
            header["properties"]["frame_id"],
            json!({ "type": "string" })
        );
        assert_eq!(header["properties"]["stamp"], json!({}));
    }
}
