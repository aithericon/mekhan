//! `aithericon-executor register` — Phase 1, Lab Runner Fleet enrollment.
//!
//! A lab PC runs:
//!
//! ```text
//! aithericon-executor register \
//!     --url https://mekhan.example.com \
//!     --token rt_<uuid>.<secret> \
//!     --name xrd_1 \
//!     [--pool lab_fleet] \
//!     [--capabilities '{"gpu":true}']
//! ```
//!
//! The runner generates its OWN NATS user nkey locally (the seed never leaves
//! the box — only the public key is sent so mekhan can mint NATS creds for it in
//! Phase 2), POSTs the enrollment, and persists the returned control-plane
//! credential under `{base_dir}/runner/`:
//!
//! - `identity.json` — `{ "runner_id", "pool", "workspace_id" }`
//! - `runner.token`  — the `rnr_<uuid>.<secret>` bearer credential (mode 0600)
//! - `user.nk`       — the NATS user nkey **seed** (mode 0600)
//!
//! The `rnr_` token is returned exactly once by mekhan and is never stored
//! server-side in plaintext, so this file is the only copy.

use std::path::PathBuf;

use aithericon_executor_worker::ExecutorConfig;
use clap::Parser;
use serde::{Deserialize, Serialize};
use tracing::info;

type BoxErr = Box<dyn std::error::Error + Send + Sync>;

/// CLI flags for the `register` subcommand.
///
/// Parsed from the process args **after** the `register` token, so we pass
/// `register` through as the binary name (clap convention) — see [`register`].
#[derive(Debug, Parser)]
#[command(
    name = "aithericon-executor register",
    about = "Enroll this executor into a mekhan lab-runner fleet"
)]
struct RegisterArgs {
    /// Mekhan base URL (e.g. `https://mekhan.example.com`). The enroll endpoint
    /// `/api/v1/runners/enroll` is appended to this.
    #[arg(long, env = "MEKHAN_URL")]
    url: String,

    /// Registration token issued by mekhan: `rt_<uuid>.<secret>`.
    #[arg(long, env = "AITHERICON_REGISTRATION_TOKEN")]
    token: String,

    /// Human-readable name for this runner (e.g. `xrd_1`).
    #[arg(long)]
    name: String,

    /// Optional pool hint to request. Server-side policy may override.
    #[arg(long)]
    pool: Option<String>,

    /// Arbitrary capabilities JSON object advertised to mekhan. Default `{}`.
    #[arg(long, default_value = "{}")]
    capabilities: String,
}

/// Enroll request body — field names are the shared wire contract with mekhan's
/// `POST /api/v1/runners/enroll`. Do not rename without changing both sides.
#[derive(Debug, Serialize)]
struct EnrollRequest {
    registration_token: String,
    name: String,
    /// `None` is serialized as JSON `null`; mekhan treats null/omitted alike.
    nats_public_key: Option<String>,
    capabilities: serde_json::Value,
}

/// Enroll response body (`EnrolledRunner`) — shared wire contract.
#[derive(Debug, Deserialize)]
struct EnrolledRunner {
    id: String,
    /// `rnr_<uuid>.<secret>` — returned ONCE, never re-fetchable.
    runner_token: String,
    workspace_id: String,
    pool: Option<String>,
}

/// On-disk identity persisted under `{base_dir}/runner/identity.json`.
///
/// Mirrors `aithericon_executor_worker::RunnerIdentity` (the daemon reads it
/// back in `ExecutorConfig::normalize()`); field names must stay in sync.
#[derive(Debug, Serialize)]
struct RunnerIdentity<'a> {
    runner_id: &'a str,
    pool: Option<&'a str>,
    workspace_id: &'a str,
}

/// Entry point for the `register` subcommand. Reads `std::env::args()` and
/// re-shapes them so clap parses everything after the `register` token.
pub async fn register() -> Result<(), BoxErr> {
    // argv = [bin, "register", flags...]. Hand clap a synthetic argv whose
    // program name is "aithericon-executor register" and whose args are the
    // flags, so `--help` and error messages read naturally.
    let flags = std::env::args().skip(2);
    let synthetic = std::iter::once("aithericon-executor register".to_string()).chain(flags);
    let args = RegisterArgs::parse_from(synthetic);

    let capabilities: serde_json::Value = serde_json::from_str(&args.capabilities)
        .map_err(|e| format!("--capabilities is not valid JSON: {e}"))?;
    if !capabilities.is_object() {
        return Err("--capabilities must be a JSON object (e.g. '{}')".into());
    }

    // Generate a NATS user nkey locally. We persist the SEED on disk (Phase 2
    // will mint NATS creds from it); only the public key crosses the wire.
    let keypair = nkeys::KeyPair::new_user();
    let nats_public_key = keypair.public_key();
    let nats_seed = keypair
        .seed()
        .map_err(|e| format!("failed to extract nkey seed: {e}"))?;

    // Resolve the destination directory before the network call so a misconfig
    // fails fast (and so we never enroll without somewhere to persist the
    // one-time token).
    let config = ExecutorConfig::load().map_err(|e| format!("configuration error: {e}"))?;
    let runner_dir = PathBuf::from(&config.base_dir).join("runner");

    let enroll_url = format!("{}/api/v1/runners/enroll", args.url.trim_end_matches('/'));
    let body = EnrollRequest {
        registration_token: args.token.clone(),
        name: args.name.clone(),
        nats_public_key: Some(nats_public_key.clone()),
        capabilities,
    };

    info!(url = %enroll_url, name = %args.name, "enrolling runner");

    let client = reqwest::Client::new();
    let resp = client
        .post(&enroll_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("enroll request to {enroll_url} failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "enroll rejected by {enroll_url}: HTTP {status}\n{text}"
        )
        .into());
    }

    let enrolled: EnrolledRunner = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse enroll response: {e}"))?;

    // Persist identity + secrets under {base_dir}/runner/.
    std::fs::create_dir_all(&runner_dir)
        .map_err(|e| format!("failed to create {}: {e}", runner_dir.display()))?;

    let identity = RunnerIdentity {
        runner_id: &enrolled.id,
        pool: enrolled.pool.as_deref(),
        workspace_id: &enrolled.workspace_id,
    };
    let identity_path = runner_dir.join("identity.json");
    let identity_json = serde_json::to_vec_pretty(&identity)
        .map_err(|e| format!("failed to serialize identity: {e}"))?;
    std::fs::write(&identity_path, &identity_json)
        .map_err(|e| format!("failed to write {}: {e}", identity_path.display()))?;

    let token_path = runner_dir.join("runner.token");
    write_secret(&token_path, enrolled.runner_token.as_bytes())?;

    let nk_path = runner_dir.join("user.nk");
    write_secret(&nk_path, nats_seed.as_bytes())?;

    info!(
        runner_id = %enrolled.id,
        workspace_id = %enrolled.workspace_id,
        pool = ?enrolled.pool,
        dir = %runner_dir.display(),
        "runner enrolled"
    );

    println!("Runner enrolled successfully.");
    println!("  runner_id    : {}", enrolled.id);
    println!("  workspace_id : {}", enrolled.workspace_id);
    println!(
        "  pool         : {}",
        enrolled.pool.as_deref().unwrap_or("(none)")
    );
    println!("  identity     : {}", identity_path.display());
    println!("  token        : {} (mode 0600)", token_path.display());
    println!("  nats nkey    : {} (mode 0600)", nk_path.display());
    println!();
    println!("Next: start the daemon to begin draining jobs, e.g.");
    println!("  aithericon-executor");

    Ok(())
}

/// Write a secret file and lock it down to owner-only (0600) on unix.
fn write_secret(path: &std::path::Path, bytes: &[u8]) -> Result<(), BoxErr> {
    std::fs::write(path, bytes).map_err(|e| format!("failed to write {}: {e}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms)
            .map_err(|e| format!("failed to chmod 0600 {}: {e}", path.display()))?;
    }

    Ok(())
}
