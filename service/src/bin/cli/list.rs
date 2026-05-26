use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct TemplateItem {
    id: String,
    name: String,
    #[allow(dead_code)]
    description: String,
    version: i32,
    published: bool,
}

#[derive(Debug, Deserialize)]
struct PaginatedResponse {
    items: Vec<TemplateItem>,
    total: i64,
}

pub async fn run(server: &str) -> Result<()> {
    let url = format!("{}/api/v1/templates?page=1&per_page=50", server);
    let resp: PaginatedResponse = crate::http::auth(reqwest::Client::new().get(&url))
        .send()
        .await
        .context("failed to connect to server")?
        .json()
        .await
        .context("invalid response from server")?;

    if resp.items.is_empty() {
        println!("No templates found.");
        return Ok(());
    }

    println!(
        "{:<38}  {:<30}  {:>4}  {:>5}",
        "ID", "NAME", "VER", "PUB"
    );
    println!("{}", "-".repeat(80));

    for t in &resp.items {
        let pub_str = if t.published { "yes" } else { "no" };
        println!(
            "{:<38}  {:<30}  {:>4}  {:>5}",
            t.id,
            truncate(&t.name, 30),
            t.version,
            pub_str,
        );
    }

    println!("\n{} template(s) total", resp.total);
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}
