use anyhow::{Context, Result};
use serde_json::Value;

pub async fn run(server: &str, instance_id: &str) -> Result<()> {
    let url = format!("{}/api/instances/{}", server, instance_id);
    let client = reqwest::Client::new();
    let resp = client
        .delete(&url)
        .send()
        .await
        .context("failed to connect to server")?;

    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or_default();

    match status.as_u16() {
        200 => {
            println!("Cancelled instance {}", instance_id);
        }
        404 => {
            anyhow::bail!("Instance not found: {}", instance_id);
        }
        409 => {
            let msg = body["error"].as_str().unwrap_or("conflict");
            println!("Cannot cancel: {}", msg);
        }
        _ => {
            let msg = body["error"].as_str().unwrap_or("unknown error");
            anyhow::bail!("Cancel failed ({}): {}", status, msg);
        }
    }

    Ok(())
}
