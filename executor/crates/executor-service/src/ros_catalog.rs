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
//! { "topics":   [ {"name": "...", "type": "..."}, ... ],
//!   "services": [ {"name": "...", "type": "..."}, ... ],
//!   "actions":  [ {"name": "...", "type": "..."}, ... ] }
//! ```
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

use serde_json::{json, Value};
use tracing::{info, warn};

use aithericon_executor_ros::RosbridgeClient;
use aithericon_executor_worker::ExecutorConfig;

/// Per-service-call timeout for rosapi introspection requests.
const ROSAPI_TIMEOUT: Duration = Duration::from_secs(10);

/// One `{name, type}` interface entry in the catalog.
fn entry(name: &str, ty: &str) -> Value {
    json!({ "name": name, "type": ty })
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
        // Retry with backoff: at runner boot the robot's rosbridge may still be
        // coming up (the dev container needs ~2s; a real robot can take longer),
        // so a single attempt races the bridge. Best-effort throughout — give up
        // quietly after the window so the daemon never blocks on catalog publish.
        const MAX_ATTEMPTS: u32 = 10;
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
async fn list_topics(client: &RosbridgeClient) -> Result<Vec<Value>, String> {
    let resp = call(client, "/rosapi/topics", &json!({})).await?;
    let names = str_array(&resp, "topics");
    let types = str_array(&resp, "types");
    Ok(names
        .iter()
        .enumerate()
        .map(|(i, name)| entry(name, types.get(i).map(String::as_str).unwrap_or("")))
        .collect())
}

/// `/rosapi/services` → `{ services: [name] }`, then `/rosapi/service_type` per
/// service for its type.
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
        out.push(entry(&name, &ty));
    }
    Ok(out)
}

/// `/rosapi/action_servers` → `{ action_servers: [name] }`, then
/// `/rosapi/action_type` per action for its type.
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
        out.push(entry(&name, &ty));
    }
    Ok(out)
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
        assert_eq!(
            entry("/turtle1/cmd_vel", "geometry_msgs/msg/Twist"),
            json!({ "name": "/turtle1/cmd_vel", "type": "geometry_msgs/msg/Twist" })
        );
    }
}
