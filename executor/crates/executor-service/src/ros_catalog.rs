//! Phase 3 — runner-side ROS interface-catalog publish.
//!
//! When this executor runs in **RUNNER mode** (a `runner_id` + a `[mekhan].url`
//! + a `[ros].ws_url` are all configured), it introspects its rosbridge at
//! startup and POSTs its ROS interface catalog to mekhan so the control plane
//! knows what topics/services/actions the runner's robot graph exposes.
//!
//! ## Introspection
//!
//! Discovery is done entirely over the SAME rosbridge JSON client the backend
//! uses ([`RosbridgeClient`]), via the `rosapi` service surface that
//! `rosapi_node` exposes:
//!
//! - `/rosapi/topics`         → `{ topics: [name], types: [type] }` (zipped)
//! - `/rosapi/services`       → `{ services: [name] }`; each type is then
//!                              fetched via `/rosapi/service_type {service}` →
//!                              `{ type }`.
//! - `/rosapi/action_servers` → `{ action_servers: [name] }`; each type via
//!                              `/rosapi/action_type {action}` → `{ type }`.
//!
//! ## Contract
//!
//! The catalog JSON value matches the `runner_interfaces` contract:
//!
//! ```json
//! { "topics":   [ {"name": "...", "type": "...", "typedefs": [ ... ]}, ... ],
//!   "services": [ {"name": "...", "type": "...", "typedefs": [ ... ]}, ... ],
//!   "actions":  [ {"name": "...", "type": "...", "typedefs": [ ... ]}, ... ] }
//! ```
//!
//! Each entry's OPTIONAL `typedefs` field carries the raw rosapi
//! message-details **flat array** for that interface's type(s) — an array of
//! `{ "type", "fieldnames", "fieldtypes", "fieldarraylen" }`, byte-identical to
//! the bundled snapshot format in `service/src/backends/ros/bundled/*.json`
//! (consumed by `service/src/backends/ros/typedef.rs`). It is derived from:
//!
//! - **topics**: `/rosapi/message_details { type }`.
//! - **services**: `/rosapi/service_request_details` + `service_response_details`
//!   (each `{ type }`), concatenated and deduped by `type`. The request/response
//!   root types are `<type>_Request` / `<type>_Response`.
//! - **actions**: `/rosapi/message_details` for each of `<type>_Goal`,
//!   `<type>_Result`, `<type>_Feedback`, merged and deduped by `type`. NOTE: the
//!   Jazzy `rosapi` does not resolve generated action sub-messages via
//!   `message_details` (they return an empty `typedefs`), so action entries get
//!   no `typedefs` and the mekhan deriver falls back to its bundled snapshots for
//!   action result ports. Topics + services derive live. Sub-type lookups that
//!   error are skipped.
//!
//! `typedefs` is **best-effort**: any detail lookup that errors is omitted (the
//! field is left absent) — it never drops the `{name, type}` entry nor crashes.
//!
//! The POST target is `{mekhan_url}/api/v1/runners/{runner_id}/interfaces` with
//! the runner's `rnr_` bearer (read from `runner_token_path`).
//!
//! ## Best-effort
//!
//! Every step is best-effort: a failure to connect, introspect, or POST is
//! logged at WARN and never crashes the daemon. A runner with no rosapi node, an
//! unreachable mekhan, or a missing token simply doesn't publish a catalog.

use std::time::Duration;

use serde_json::{json, Map, Value};
use tracing::{info, warn};

use aithericon_executor_ros::RosbridgeClient;
use aithericon_executor_worker::ExecutorConfig;

/// Per-service-call timeout for rosapi introspection requests.
const ROSAPI_TIMEOUT: Duration = Duration::from_secs(10);

/// One `{name, type}` interface entry in the catalog, optionally carrying the
/// rosapi message-details `typedefs` flat array. When `typedefs` is `None` (a
/// best-effort detail lookup failed), the field is left ABSENT — the
/// `{name, type}` entry is always preserved.
fn entry_with_typedefs(name: &str, ty: &str, typedefs: Option<Value>) -> Value {
    let mut obj = json!({ "name": name, "type": ty });
    if let Some(td) = typedefs {
        obj["typedefs"] = td;
    }
    obj
}

/// Spawn the runner-side catalog publish as a fire-and-forget background task.
///
/// No-op unless ALL of `runner_id`, a mekhan URL, and the runner token path are
/// resolvable from `config`. The ROS ws URL always has a default, so its
/// presence is implied by the daemon registering the ROS backend; we still read
/// it from config here. Failures inside the task are logged, never propagated.
pub fn spawn_catalog_publish(config: &ExecutorConfig) {
    let Some(runner_id) = config.runner_id.clone() else {
        return;
    };
    let Some(mekhan_url) = config.mekhan_url() else {
        info!(%runner_id, "ros catalog publish skipped: no [mekhan].url configured");
        return;
    };
    let Some(token_path) = config.runner_token_path.clone() else {
        info!(%runner_id, "ros catalog publish skipped: no runner token path");
        return;
    };
    let ws_url = config.ros_ws_url();

    tokio::spawn(async move {
        // Retry with backoff: at runner boot the robot's rosbridge AND rosapi may
        // still be coming up. A fresh dev turtle container takes ~20-40s before
        // rosapi answers `/rosapi/topics`, and a real robot's bringup is slower
        // still, so a short window races the graph. ~90s (30 × 3s) comfortably
        // covers a cold container without blocking the daemon. Best-effort
        // throughout — give up quietly after the window so the daemon never hangs.
        const MAX_ATTEMPTS: u32 = 30;
        const RETRY_DELAY: Duration = Duration::from_secs(3);
        for attempt in 1..=MAX_ATTEMPTS {
            match introspect_and_publish(&runner_id, &mekhan_url, &token_path, &ws_url).await {
                Ok(()) => return,
                Err(e) if attempt < MAX_ATTEMPTS => {
                    warn!(
                        %runner_id, %ws_url, attempt, error = %e,
                        "ros interface-catalog publish attempt failed; retrying"
                    );
                    tokio::time::sleep(RETRY_DELAY).await;
                }
                Err(e) => warn!(
                    %runner_id, %ws_url, attempt, error = %e,
                    "ros interface-catalog publish failed after {MAX_ATTEMPTS} attempts \
                     (best-effort; daemon continues)"
                ),
            }
        }
    });
}

/// Connect to the rosbridge, build the catalog, and POST it to mekhan.
async fn introspect_and_publish(
    runner_id: &str,
    mekhan_url: &str,
    token_path: &std::path::Path,
    ws_url: &str,
) -> Result<(), String> {
    let token = std::fs::read_to_string(token_path)
        .map_err(|e| format!("read runner token {}: {e}", token_path.display()))?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err(format!("runner token at {} is empty", token_path.display()));
    }

    info!(%ws_url, "introspecting ROS interfaces via rosapi");
    let client = RosbridgeClient::connect(ws_url)
        .await
        .map_err(|e| format!("rosbridge connect to {ws_url}: {e}"))?;

    let catalog = introspect_catalog(&client).await?;

    let topics = catalog["topics"].as_array().map_or(0, Vec::len);
    let services = catalog["services"].as_array().map_or(0, Vec::len);
    let actions = catalog["actions"].as_array().map_or(0, Vec::len);
    info!(topics, services, actions, "ROS interface catalog introspected");

    publish_catalog(runner_id, mekhan_url, &token, &catalog).await
}

/// Build the catalog Value from the three rosapi list services. A failure to
/// list a whole CATEGORY (topics/services/actions) is fatal to introspection
/// only for that category — but to keep the contract shape stable we surface the
/// first hard error. Individual per-item type lookups that fail fall back to an
/// empty type string so one undiscoverable item doesn't drop the whole catalog.
pub async fn introspect_catalog(client: &RosbridgeClient) -> Result<Value, String> {
    let topics = list_topics(client).await?;
    let services = list_services(client).await?;
    let actions = list_actions(client).await?;

    Ok(json!({
        "topics": topics,
        "services": services,
        "actions": actions,
    }))
}

/// `/rosapi/topics` → `{ topics: [name], types: [type] }`, zipped into entries.
/// Each entry's `typedefs` is best-effort from `/rosapi/message_details`.
async fn list_topics(client: &RosbridgeClient) -> Result<Vec<Value>, String> {
    let resp = call(client, "/rosapi/topics", &json!({})).await?;
    let names = str_array(&resp, "topics");
    let types = str_array(&resp, "types");
    let mut out = Vec::with_capacity(names.len());
    for (i, name) in names.iter().enumerate() {
        let ty = types.get(i).map(String::as_str).unwrap_or("");
        // Skip framework-infra topic types (/parameter_events, /rosout, …):
        // introspecting them crashes the Jazzy rosapi node (see is_infra_type).
        let typedefs = if ty.is_empty() || is_infra_type(ty) {
            None
        } else {
            message_typedefs(client, ty).await
        };
        out.push(entry_with_typedefs(name, ty, typedefs));
    }
    Ok(out)
}

/// `/rosapi/services` → `{ services: [name] }`, then `/rosapi/service_type` per
/// service for its type. `typedefs` is the merged request + response details.
async fn list_services(client: &RosbridgeClient) -> Result<Vec<Value>, String> {
    let resp = call(client, "/rosapi/services", &json!({})).await?;
    let names = str_array(&resp, "services");
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let ty = call(client, "/rosapi/service_type", &json!({ "service": name }))
            .await
            .ok()
            .and_then(|r| r.get("type").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        let typedefs = if ty.is_empty() {
            None
        } else {
            service_typedefs(client, &ty).await
        };
        out.push(entry_with_typedefs(&name, &ty, typedefs));
    }
    Ok(out)
}

/// `/rosapi/action_servers` → `{ action_servers: [name] }`, then
/// `/rosapi/action_type` per action for its type. `typedefs` is the merged
/// goal + result + feedback message details.
async fn list_actions(client: &RosbridgeClient) -> Result<Vec<Value>, String> {
    let resp = call(client, "/rosapi/action_servers", &json!({})).await?;
    let names = str_array(&resp, "action_servers");
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let ty = call(client, "/rosapi/action_type", &json!({ "action": name }))
            .await
            .ok()
            .and_then(|r| r.get("type").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        let typedefs = if ty.is_empty() {
            None
        } else {
            action_typedefs(client, &ty).await
        };
        out.push(entry_with_typedefs(&name, &ty, typedefs));
    }
    Ok(out)
}

/// Best-effort `/rosapi/message_details { type }` → the response `typedefs` flat
/// array. `None` on any error or a missing/empty array. (The Jazzy `rosapi`
/// `MessageDetails_Request` has NO `ros_version` field — passing one makes the
/// call fail, so we send only `{ type }`.)
async fn message_typedefs(client: &RosbridgeClient, ty: &str) -> Option<Value> {
    let resp = call(client, "/rosapi/message_details", &json!({ "type": ty }))
        .await
        .ok()?;
    nonempty_typedefs(&resp)
}

/// ROS 2 *framework infrastructure* interface packages whose typedef
/// introspection CRASHES the Jazzy `rosapi_node`. Their messages carry
/// bounded-sequence / deeply-recursive fields (e.g. `ParameterDescriptor` with
/// `FloatingPointRange[<=1]`, or `type_description_interfaces` self-referential
/// field descriptors) that rosapi's `_get_*typedef_recursive` mis-parses — it
/// tries to import a class literally named `FloatingPointRange, 1`, then hits an
/// `AssertionError` that takes down the WHOLE rosapi node, killing ALL further
/// introspection so the catalog publish never completes. Every node exposes
/// these (the six `rcl_interfaces/srv/*` parameter services, the
/// `type_description_interfaces` GetTypeDescription service, the
/// `/parameter_events` + `/rosout` infra topics, rosapi's/rosbridge's own
/// services, …). They are also never sensible authoring targets for a ROS step,
/// so we skip the typedef DETAIL fetch for any interface in one of these
/// packages. The `{name, type}` entry is still listed (P3 behaviour); it just
/// carries no `typedefs`, so the deriver falls back to bundled (none exist for
/// infra types → an empty output port, exactly as before this feature).
const INFRA_PACKAGES: &[&str] = &[
    "rcl_interfaces/",
    "type_description_interfaces/",
    "action_msgs/",
    "lifecycle_msgs/",
    "composition_interfaces/",
    "rosapi_msgs/",
    "rosbridge_msgs/",
    "statistics_msgs/",
];

/// True for an interface type in a framework-infrastructure package — see
/// [`INFRA_PACKAGES`]. Skipped from typedef introspection (rosapi crash-guard).
fn is_infra_type(ty: &str) -> bool {
    INFRA_PACKAGES.iter().any(|p| ty.starts_with(p))
}

/// Best-effort merged service request + response details, deduped by `type`.
/// Concatenates `/rosapi/service_request_details` and
/// `/rosapi/service_response_details` (each `{ type }`). `None` if both fail or
/// yield nothing — or immediately for an [`is_infra_type`] (the rosapi
/// crash-trigger; see that fn).
async fn service_typedefs(client: &RosbridgeClient, ty: &str) -> Option<Value> {
    if is_infra_type(ty) {
        return None;
    }
    let req = call(client, "/rosapi/service_request_details", &json!({ "type": ty }))
        .await
        .ok()
        .and_then(|r| nonempty_typedefs(&r));
    let resp = call(client, "/rosapi/service_response_details", &json!({ "type": ty }))
        .await
        .ok()
        .and_then(|r| nonempty_typedefs(&r));
    merge_typedefs([req, resp])
}

/// Best-effort merged action goal + result + feedback message details, deduped
/// by `type`. rosapi exposes action sub-message types under the suffixed names
/// `<type>_Goal` / `<type>_Result` / `<type>_Feedback`; each is looked up via
/// `/rosapi/message_details` and skipped if it errors. `None` if none resolve.
async fn action_typedefs(client: &RosbridgeClient, ty: &str) -> Option<Value> {
    let goal = message_typedefs(client, &format!("{ty}_Goal")).await;
    let result = message_typedefs(client, &format!("{ty}_Result")).await;
    let feedback = message_typedefs(client, &format!("{ty}_Feedback")).await;
    merge_typedefs([goal, result, feedback])
}

/// Pull a non-empty `typedefs` array out of a rosapi response, projecting each
/// entry to the four fields the mekhan mapper consumes — `{ type, fieldnames,
/// fieldtypes, fieldarraylen }` — and dropping rosapi's `examples`,
/// `constnames`, and (crucially) `constvalues`. `constvalues` carries Python
/// `repr()`s of `rosidl_parser` objects (`<… object at 0x…>`), so keeping it
/// would (a) bloat the JSONB catalog + every node's embedded config and
/// (b) make the catalog NON-DETERMINISTIC — the memory addresses differ every
/// run, so an unchanged graph's published catalog would churn on every restart.
/// Slimming yields exactly the bundled snapshot shape. `None` when the field is
/// missing, not an array, or empty (so an absent `typedefs` is never attached
/// as an empty array).
fn nonempty_typedefs(resp: &Value) -> Option<Value> {
    let arr = resp.get("typedefs")?.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let slimmed: Vec<Value> = arr
        .iter()
        .map(|td| {
            let mut o = Map::new();
            for k in ["type", "fieldnames", "fieldtypes", "fieldarraylen"] {
                if let Some(v) = td.get(k) {
                    o.insert(k.to_string(), v.clone());
                }
            }
            Value::Object(o)
        })
        .collect();
    Some(Value::Array(slimmed))
}

/// Concatenate several optional `typedefs` arrays, deduping by each element's
/// `type` field (first occurrence wins). `None` if the merged result is empty.
fn merge_typedefs<const N: usize>(parts: [Option<Value>; N]) -> Option<Value> {
    let mut merged: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for part in parts.into_iter().flatten() {
        let Value::Array(arr) = part else { continue };
        for td in arr {
            let key = td
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_default();
            if seen.insert(key) {
                merged.push(td);
            }
        }
    }
    if merged.is_empty() {
        None
    } else {
        Some(Value::Array(merged))
    }
}

/// Call a rosapi service and return its `values` payload, mapping the client
/// error to a `String` (this module's error type).
async fn call(client: &RosbridgeClient, service: &str, args: &Value) -> Result<Value, String> {
    client
        .call_service(service, args, ROSAPI_TIMEOUT)
        .await
        .map_err(|e| format!("rosapi call {service}: {e}"))
}

/// Extract a `[string]` field from a rosbridge `values` object, dropping
/// non-string elements. Missing/non-array → empty.
fn str_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// POST the catalog to mekhan's runner-interfaces endpoint with the `rnr_`
/// bearer. mekhan replies 204 (No Content) on success.
async fn publish_catalog(
    runner_id: &str,
    mekhan_url: &str,
    token: &str,
    catalog: &Value,
) -> Result<(), String> {
    let url = format!("{mekhan_url}/api/v1/runners/{runner_id}/interfaces");
    let body = json!({ "catalog": catalog });

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("mekhan rejected catalog at {url}: HTTP {status}\n{text}"));
    }

    info!(%runner_id, %url, "ROS interface catalog published to mekhan");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn str_array_extracts_strings_and_tolerates_shape() {
        let v = json!({ "topics": ["/a", "/b"], "types": ["T/a"] });
        assert_eq!(str_array(&v, "topics"), vec!["/a", "/b"]);
        assert_eq!(str_array(&v, "types"), vec!["T/a"]);
        assert!(str_array(&v, "missing").is_empty());
        // Non-string elements are dropped, not panicked on.
        let mixed = json!({ "k": ["x", 7, "y"] });
        assert_eq!(str_array(&mixed, "k"), vec!["x", "y"]);
    }

    #[test]
    fn entry_shape_matches_contract() {
        // No typedefs → the field is absent, matching the slim {name, type} shape.
        assert_eq!(
            entry_with_typedefs("/turtle1/cmd_vel", "geometry_msgs/msg/Twist", None),
            json!({ "name": "/turtle1/cmd_vel", "type": "geometry_msgs/msg/Twist" })
        );
    }

    #[test]
    fn entry_with_typedefs_includes_array_when_present() {
        let td = json!([
            { "type": "geometry_msgs/Twist", "fieldnames": ["linear"],
              "fieldtypes": ["geometry_msgs/Vector3"], "fieldarraylen": [-1] }
        ]);
        let got =
            entry_with_typedefs("/turtle1/cmd_vel", "geometry_msgs/msg/Twist", Some(td.clone()));
        assert_eq!(got["name"], "/turtle1/cmd_vel");
        assert_eq!(got["type"], "geometry_msgs/msg/Twist");
        assert_eq!(got["typedefs"], td);
    }

    #[test]
    fn entry_with_typedefs_omits_field_when_absent() {
        let got = entry_with_typedefs("/svc", "pkg/Srv", None);
        assert!(got.get("typedefs").is_none());
        assert_eq!(got.as_object().unwrap().len(), 2);
    }

    #[test]
    fn nonempty_typedefs_filters_missing_and_empty() {
        assert!(nonempty_typedefs(&json!({})).is_none());
        assert!(nonempty_typedefs(&json!({ "typedefs": [] })).is_none());
        assert!(nonempty_typedefs(&json!({ "typedefs": "nope" })).is_none());
        let v = json!({ "typedefs": [ { "type": "pkg/T" } ] });
        assert_eq!(nonempty_typedefs(&v), Some(json!([ { "type": "pkg/T" } ])));
    }

    #[test]
    fn nonempty_typedefs_slims_to_four_fields() {
        // rosapi's extra fields (examples/constnames/constvalues — the latter
        // carrying non-deterministic Python object reprs) are dropped; only the
        // four mapper fields survive, yielding the bundled snapshot shape.
        let v = json!({ "typedefs": [ {
            "type": "turtlesim/Pose",
            "fieldnames": ["x"], "fieldtypes": ["float"], "fieldarraylen": [-1],
            "examples": ["0.0"],
            "constnames": ["SLOT_TYPES"],
            "constvalues": ["<rosidl_parser.definition.BasicType object at 0xffff7bf11960>"]
        } ] });
        assert_eq!(
            nonempty_typedefs(&v),
            Some(json!([ {
                "type": "turtlesim/Pose",
                "fieldnames": ["x"], "fieldtypes": ["float"], "fieldarraylen": [-1]
            } ]))
        );
    }

    #[test]
    fn merge_typedefs_concats_and_dedupes_by_type() {
        let a = Some(json!([ { "type": "pkg/A" }, { "type": "pkg/B" } ]));
        let b = Some(json!([ { "type": "pkg/B" }, { "type": "pkg/C" } ]));
        // First occurrence of each `type` wins; order preserved.
        assert_eq!(
            merge_typedefs([a, b, None]),
            Some(json!([ { "type": "pkg/A" }, { "type": "pkg/B" }, { "type": "pkg/C" } ]))
        );
        // All-None / all-empty → None, so no empty array is ever attached.
        assert!(merge_typedefs::<3>([None, None, None]).is_none());
    }

    #[test]
    fn infra_types_are_skipped() {
        // The rosapi-crashing framework-infra types are skipped; the robot's own
        // topics/services are introspected.
        assert!(is_infra_type("rcl_interfaces/srv/DescribeParameters"));
        assert!(is_infra_type("rcl_interfaces/msg/ParameterEvent"));
        assert!(is_infra_type("type_description_interfaces/srv/GetTypeDescription"));
        assert!(is_infra_type("rosbridge_msgs/msg/ConnectedClients"));
        assert!(!is_infra_type("turtlesim/srv/TeleportAbsolute"));
        assert!(!is_infra_type("turtlesim/Pose"));
        assert!(!is_infra_type("geometry_msgs/msg/Twist"));
        assert!(!is_infra_type("std_srvs/srv/Empty"));
    }
}
