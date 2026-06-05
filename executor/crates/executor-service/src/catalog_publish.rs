//! Shared runner interface-catalog publish helper.
//!
//! POSTs a `RunnerInterfaceCatalog`-shaped JSON value to mekhan's runner-
//! interfaces endpoint with the runner's `rnr_` bearer. Used by BOTH the ROS
//! catalog publisher (`ros_catalog.rs`, `ros` feature) and the model-pool node
//! agent (`model_agent.rs`, `vllm` feature) — lifting it here keeps a `vllm`
//! build (a GPU host with no ROS deps) from having to compile the ROS module.
//!
//! Gated `any(feature = "ros", feature = "vllm")` so it is only compiled when at
//! least one consumer is built.

use serde_json::{json, Value};
use tracing::info;

/// POST the catalog to mekhan's runner-interfaces endpoint with the `rnr_`
/// bearer. The body is `{ "catalog": <catalog> }` against
/// `{mekhan_url}/api/v1/runners/{runner_id}/interfaces`. mekhan replies 204
/// (No Content) on success. Returns the human-readable error string on any
/// transport or non-success status.
pub async fn publish_catalog(
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
        return Err(format!(
            "mekhan rejected catalog at {url}: HTTP {status}\n{text}"
        ));
    }

    info!(%runner_id, %url, "runner interface catalog published to mekhan");
    Ok(())
}
