use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct InstanceItem {
    id: String,
    #[allow(dead_code)]
    template_id: String,
    template_name: String,
    status: String,
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct PaginatedResponse {
    items: Vec<InstanceItem>,
    total: i64,
}

pub async fn run(server: &str, template_id: Option<&str>) -> Result<()> {
    let mut url = format!("{}/api/v1/instances?page=1&per_page=50", server);
    if let Some(tid) = template_id {
        url.push_str(&format!("&template_id={}", tid));
    }

    let resp: PaginatedResponse = crate::http::auth(reqwest::Client::new().get(&url))
        .send()
        .await
        .context("failed to connect to server")?
        .json()
        .await
        .context("invalid response from server")?;

    if resp.items.is_empty() {
        println!("No instances found.");
        return Ok(());
    }

    println!(
        "{:<38}  {:<25}  {:<12}  CREATED",
        "ID", "TEMPLATE", "STATUS"
    );
    println!("{}", "-".repeat(90));

    for inst in &resp.items {
        println!(
            "{:<38}  {:<25}  {:<12}  {}",
            inst.id,
            truncate(&inst.template_name, 25),
            inst.status,
            truncate(&inst.created_at, 20),
        );
    }

    println!("\n{} instance(s) total", resp.total);
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}
