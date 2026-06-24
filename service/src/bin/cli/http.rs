//! Shared HTTP auth for the CLI.
//!
//! The CLI is a non-interactive client. Against an auth-enabled server every
//! request needs a mekhan-native user PAT (`uat_...`), supplied as
//! `MEKHAN_CLI_TOKEN` and validated server-side against the local `user_pats`
//! table (the dual-use `AuthUser` extractor accepts it exactly like a browser
//! session cookie). No-op when the env var is unset — local `dev_noop` servers
//! need no token.

use anyhow::{Context, Result};
use reqwest::RequestBuilder;
use serde::Deserialize;

/// Attach `Authorization: Bearer $MEKHAN_CLI_TOKEN` when it is set.
pub fn auth(rb: RequestBuilder) -> RequestBuilder {
    match std::env::var("MEKHAN_CLI_TOKEN") {
        Ok(t) if !t.is_empty() => rb.bearer_auth(t),
        _ => rb,
    }
}

/// Latest row in a template's version chain. Subset of `WorkflowTemplate`
/// — only the fields the CLI actually consumes after resolving the chain
/// head (`id` for version-id-scoped downstream calls, `base_template_id`
/// for lock-file writes, `version` for display).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestVersion {
    pub id: String,
    /// Chain root. Server populates this on every row (v1 sets it to its
    /// own id via `INSERT (... $1) VALUES ($1,...,$1,...)`), so `None` only
    /// surfaces for pre-existing legacy rows — practically never in this
    /// codebase.
    pub base_template_id: Option<String>,
    pub version: i32,
}

impl LatestVersion {
    /// Resolve the chain root id with the same fallback the server uses.
    pub fn base_id(&self) -> &str {
        self.base_template_id.as_deref().unwrap_or(&self.id)
    }
}

/// Resolve any id in a template chain (the lock file's `baseTemplateId`, or
/// any historical version id) to the row currently flagged `is_latest`.
/// Hits `GET /api/v1/templates/{id}/latest`, which is the single
/// chain-following endpoint — every other CLI-facing route is strictly
/// version-id-scoped, so this is the only place that follows the chain.
pub async fn resolve_latest(server: &str, any_chain_id: &str) -> Result<LatestVersion> {
    let url = format!(
        "{}/api/v1/templates/{}/latest",
        server.trim_end_matches('/'),
        any_chain_id,
    );
    let client = reqwest::Client::new();
    let resp = auth(client.get(&url))
        .send()
        .await
        .with_context(|| format!("failed to resolve latest version of {any_chain_id}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("server rejected /latest ({status}): {body}");
    }
    resp.json::<LatestVersion>()
        .await
        .context("invalid /latest response shape")
}
