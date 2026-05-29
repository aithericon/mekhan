//! `mekhan demos reset|reseed` — operator maintenance for the built-in demos.
//!
//! Thin wrappers over the admin endpoints `POST /api/v1/admin/demos/{reset,
//! reseed}`. Both are destructive (cancel running instances + purge nets);
//! `reseed` additionally re-seeds every demo from the server's on-disk demos
//! directory, overwriting any edits.

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetReport {
    families_removed: usize,
    instances_purged: usize,
    tests_removed: usize,
    seeded: usize,
}

/// Which maintenance action to run.
#[derive(Clone, Copy)]
pub enum Action {
    /// Remove seeded demos; do not re-seed.
    Reset,
    /// Remove seeded demos then re-seed from disk.
    Reseed,
}

pub async fn run(server: &str, action: Action) -> Result<()> {
    let (path, verb) = match action {
        Action::Reset => ("/api/v1/admin/demos/reset", "reset"),
        Action::Reseed => ("/api/v1/admin/demos/reseed", "reseed"),
    };
    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let resp = crate::http::auth(reqwest::Client::new().post(&url))
        .send()
        .await
        .context("failed to connect to server")?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        if status.as_u16() == 403 {
            anyhow::bail!(
                "demo {verb} forbidden — requires editor of the default workspace \
                 (set MEKHAN_CLI_TOKEN to an editor's PAT). Server said: {body}"
            );
        }
        anyhow::bail!("demo {verb} failed ({status}): {body}");
    }

    let report: ResetReport = resp.json().await.context("invalid response shape")?;
    println!(
        "Demo {verb} complete: {} family(ies) removed, {} instance(s) purged, {} test(s) removed{}.",
        report.families_removed,
        report.instances_purged,
        report.tests_removed,
        match action {
            Action::Reseed => format!(", {} demo(s) re-seeded", report.seeded),
            Action::Reset => String::new(),
        }
    );
    Ok(())
}
