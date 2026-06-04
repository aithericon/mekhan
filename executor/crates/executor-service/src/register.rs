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
    /// Phase 2: a signed NATS user JWT minted by mekhan from the
    /// `nats_public_key` we sent. `None` when no public key was sent OR mekhan's
    /// NATS signing key is unavailable — in that case the runner can fetch creds
    /// later via `aithericon-executor refresh-creds`.
    #[serde(default)]
    nats_jwt: Option<String>,
}

/// Response body of `POST /api/v1/runners/{id}/nats-creds` — shared wire
/// contract. Mints/rotates a fresh NATS user JWT from the runner row's stored
/// public key.
#[derive(Debug, Deserialize)]
struct RunnerNatsCreds {
    /// Freshly signed NATS user JWT for this runner.
    nats_jwt: String,
    /// The runners-account signing key's public key (the JWT issuer). Returned
    /// for diagnostics; the JWT itself already embeds it as `iss`.
    #[allow(dead_code)]
    account_public_key: String,
}

/// On-disk identity as read back by `refresh-creds` (owned, deserializing
/// variant of [`RunnerIdentity`]).
#[derive(Debug, Deserialize)]
struct RunnerIdentityOwned {
    runner_id: String,
}

/// Render the standard NATS `.creds` decorator from a user JWT and the runner's
/// own nkey seed. `async-nats`'s `with_credentials_file` parses exactly this
/// armored two-block layout (JWT block + IMPORTANT banner + SEED block).
///
/// Byte-for-byte matches the format proven in the Phase-0 minting spike.
fn assemble_creds(user_jwt: &str, seed: &str) -> String {
    format!(
        "-----BEGIN NATS USER JWT-----\n\
         {user_jwt}\n\
         ------END NATS USER JWT------\n\
         \n\
         ************************* IMPORTANT *************************\n\
         NKEY Seed printed below can be used to sign and prove identity.\n\
         NKEYs are sensitive and should be treated as secrets.\n\
         \n\
         -----BEGIN USER NKEY SEED-----\n\
         {seed}\n\
         ------END USER NKEY SEED------\n\
         \n\
         *************************************************************\n"
    )
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

    // Phase 2: if mekhan returned a signed user JWT, assemble + persist the
    // `.creds` file the daemon connects with. The seed is the one we just
    // generated locally (it never crossed the wire).
    let creds_path = runner_dir.join("runner.creds");
    let creds_written = if let Some(jwt) = enrolled.nats_jwt.as_deref() {
        let creds = assemble_creds(jwt, &nats_seed);
        write_secret(&creds_path, creds.as_bytes())?;
        true
    } else {
        false
    };

    info!(
        runner_id = %enrolled.id,
        workspace_id = %enrolled.workspace_id,
        pool = ?enrolled.pool,
        dir = %runner_dir.display(),
        nats_creds = creds_written,
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
    if creds_written {
        println!("  nats creds   : {} (mode 0600)", creds_path.display());
    }
    println!();
    if !creds_written {
        println!(
            "No NATS creds were issued at enroll time. Fetch them later with:"
        );
        println!("  aithericon-executor refresh-creds --url {}", args.url);
        println!();
    }
    println!("Next: start the daemon to begin draining jobs, e.g.");
    println!("  aithericon-executor");

    Ok(())
}

/// CLI flags for the `refresh-creds` subcommand.
#[derive(Debug, Parser)]
#[command(
    name = "aithericon-executor refresh-creds",
    about = "Mint/rotate this runner's scoped NATS creds from mekhan"
)]
struct RefreshCredsArgs {
    /// Mekhan base URL (e.g. `https://mekhan.example.com`). The nats-creds
    /// endpoint `/api/v1/runners/{id}/nats-creds` is appended to this.
    #[arg(long, env = "MEKHAN_URL")]
    url: String,

    /// Override the executor base directory (where `runner/` lives). Defaults
    /// to the resolved `ExecutorConfig.base_dir`.
    #[arg(long)]
    base_dir: Option<PathBuf>,
}

/// Entry point for the `refresh-creds` subcommand.
///
/// Reads the persisted runner identity + bearer token, POSTs to mekhan's
/// self-service nats-creds endpoint, and rewrites `{base_dir}/runner/runner.creds`
/// from the freshly-minted JWT + the seed already on disk. This is how a
/// Phase-1-enrolled runner (no JWT yet) or a rotating runner obtains fresh creds.
pub async fn refresh_creds() -> Result<(), BoxErr> {
    let flags = std::env::args().skip(2);
    let synthetic =
        std::iter::once("aithericon-executor refresh-creds".to_string()).chain(flags);
    let args = RefreshCredsArgs::parse_from(synthetic);

    // Resolve the runner directory: explicit --base-dir wins, else config.
    let base_dir = match args.base_dir {
        Some(dir) => dir,
        None => {
            let config = ExecutorConfig::load().map_err(|e| format!("configuration error: {e}"))?;
            PathBuf::from(config.base_dir)
        }
    };
    let runner_dir = base_dir.join("runner");

    let identity_path = runner_dir.join("identity.json");
    let token_path = runner_dir.join("runner.token");
    let nk_path = runner_dir.join("user.nk");

    let identity_bytes = std::fs::read(&identity_path).map_err(|e| {
        format!(
            "no runner identity at {} ({e}). Enroll first with `aithericon-executor register`.",
            identity_path.display()
        )
    })?;
    let identity: RunnerIdentityOwned = serde_json::from_slice(&identity_bytes)
        .map_err(|e| format!("failed to parse {}: {e}", identity_path.display()))?;

    let token = std::fs::read_to_string(&token_path).map_err(|e| {
        format!(
            "no runner token at {} ({e}). Enroll first with `aithericon-executor register`.",
            token_path.display()
        )
    })?;
    let token = token.trim();

    let seed = std::fs::read_to_string(&nk_path).map_err(|e| {
        format!(
            "no NATS nkey seed at {} ({e}). Enroll first with `aithericon-executor register`.",
            nk_path.display()
        )
    })?;
    let seed = seed.trim();

    let creds_url = format!(
        "{}/api/v1/runners/{}/nats-creds",
        args.url.trim_end_matches('/'),
        identity.runner_id
    );

    info!(url = %creds_url, runner_id = %identity.runner_id, "requesting NATS creds");

    let client = reqwest::Client::new();
    let resp = client
        .post(&creds_url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("nats-creds request to {creds_url} failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("nats-creds rejected by {creds_url}: HTTP {status}\n{text}").into());
    }

    let minted: RunnerNatsCreds = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse nats-creds response: {e}"))?;

    let creds = assemble_creds(&minted.nats_jwt, seed);
    let creds_path = runner_dir.join("runner.creds");
    write_secret(&creds_path, creds.as_bytes())?;

    info!(
        runner_id = %identity.runner_id,
        creds = %creds_path.display(),
        "NATS creds refreshed"
    );

    println!("NATS creds refreshed.");
    println!("  runner_id  : {}", identity.runner_id);
    println!("  nats creds : {} (mode 0600)", creds_path.display());
    println!();
    println!("Restart the daemon to pick up the new creds, e.g.");
    println!("  aithericon-executor");

    Ok(())
}

// ---------------------------------------------------------------------------
// Phase B — grouped + enrolled workers: boot-time self-enroll
// ---------------------------------------------------------------------------

/// Worker enroll request body — shared wire contract with mekhan's
/// `POST /api/v1/workers/enroll` (`EnrollWorkerRequest`). Do not rename fields
/// without changing both sides.
#[derive(Debug, Serialize)]
struct EnrollWorkerRequest {
    registration_token: String,
    name: String,
    /// `None` serializes as JSON `null`; mekhan treats null/omitted alike (no
    /// scoped JWT minted in that case).
    nats_public_key: Option<String>,
    /// Executor backend wire-names this daemon serves (set-membership). The
    /// scoped JWT mekhan mints is allowed to drain exactly these.
    backends: Vec<String>,
}

/// Worker enroll response body — shared wire contract (`EnrolledWorker`).
/// `id` / `workspace_id` are mekhan `Uuid`s on the wire (JSON strings); we keep
/// them as `String` here since the daemon only echoes them.
#[derive(Debug, Deserialize)]
struct EnrolledWorker {
    id: String,
    /// `wkr_<uuid>.<secret>` — returned ONCE, never re-fetchable.
    #[allow(dead_code)]
    worker_token: String,
    workspace_id: String,
    /// Human-readable routing group ALIAS (display only). The group the worker
    /// COMPETES in; resolved server-side to `routing_partition` below.
    #[serde(default)]
    group: Option<String>,
    /// The capacity-resource UUID this worker's grouped consumer binds to — the
    /// JetStream/NATS partition token (`executor-<wire>-grp.<prio>.<routing_partition>.>`).
    /// Workspace-safe by construction (two workspaces' "default" groups never
    /// collide on a queue). This is what the daemon binds, NOT `group`.
    routing_partition: String,
    /// Scoped NATS user JWT minted from the `nats_public_key` we sent. `None`
    /// when no key was sent OR mekhan's signing key is unavailable.
    #[serde(default)]
    nats_jwt: Option<String>,
}

/// On-disk worker identity persisted under `{base_dir}/worker/identity.json`.
/// Mirrors `aithericon_executor_worker::WorkerIdentity` (the daemon reads it
/// back in `ExecutorConfig::normalize()`); field names must stay in sync.
#[derive(Debug, Serialize)]
struct WorkerIdentityWire<'a> {
    worker_id: &'a str,
    /// Display-only group alias.
    group: Option<&'a str>,
    /// The capacity-resource UUID the grouped consumer binds as its partition
    /// token. This — not `group` — is the dispatch routing key.
    routing_partition: &'a str,
    workspace_id: &'a str,
}

/// Outcome of a successful boot-time worker enrollment, returned to the daemon
/// so it can wire the inherited group into the grouped consumer bind and set
/// `nats_creds`/`worker_id` without re-reading config.
pub struct EnrolledWorkerLocal {
    pub worker_id: String,
    /// The display-only routing group alias inherited from the registration
    /// token (mekhan's response). `None` when the token named no explicit group
    /// (the implicit "default" group); the routing key is `routing_partition`.
    pub group: Option<String>,
    /// The capacity-resource UUID this worker's grouped consumer binds as its
    /// partition token (`executor-<wire>-grp.<prio>.<routing_partition>.>`). This
    /// is the unified dispatch routing key — always present (mekhan resolves the
    /// implicit "default" group to its UUID).
    pub routing_partition: String,
    /// Absolute path to the assembled `.creds` file, when mekhan returned a JWT.
    pub creds_path: Option<PathBuf>,
}

/// Self-enroll this worker into a mekhan worker fleet on daemon boot.
///
/// Mirrors the runner [`register`] path (local nkey, POST enroll, assemble
/// `.creds`) but is non-interactive: it reads `mekhan_url` + the served backend
/// `wire`s from the daemon, not CLI flags. The returned `group` is inherited
/// from the registration token (we do NOT pass a group to mekhan — the token
/// carries it) and becomes the worker's routing group. Idempotent at the
/// caller: skip when `{base_dir}/worker/identity.json` already exists.
///
/// `name` is the operator-stable worker label (the daemon's `config.name`);
/// `backends` are the wire-names this binary registered.
pub async fn enroll_worker(
    mekhan_url: &str,
    registration_token: &str,
    name: &str,
    backends: Vec<String>,
    base_dir: &str,
) -> Result<EnrolledWorkerLocal, BoxErr> {
    // Generate a NATS user nkey locally; only the public key crosses the wire
    // (the seed is persisted and folded into the `.creds` file).
    let keypair = nkeys::KeyPair::new_user();
    let nats_public_key = keypair.public_key();
    let nats_seed = keypair
        .seed()
        .map_err(|e| format!("failed to extract nkey seed: {e}"))?;

    let worker_dir = PathBuf::from(base_dir).join("worker");
    let enroll_url = format!(
        "{}/api/v1/workers/enroll",
        mekhan_url.trim_end_matches('/')
    );
    let body = EnrollWorkerRequest {
        registration_token: registration_token.to_string(),
        name: name.to_string(),
        nats_public_key: Some(nats_public_key),
        backends,
    };

    info!(url = %enroll_url, %name, "self-enrolling worker");

    let client = reqwest::Client::new();
    let resp = client
        .post(&enroll_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("worker enroll request to {enroll_url} failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("worker enroll rejected by {enroll_url}: HTTP {status}\n{text}").into());
    }

    let enrolled: EnrolledWorker = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse worker enroll response: {e}"))?;

    std::fs::create_dir_all(&worker_dir)
        .map_err(|e| format!("failed to create {}: {e}", worker_dir.display()))?;

    let identity = WorkerIdentityWire {
        worker_id: &enrolled.id,
        group: enrolled.group.as_deref(),
        routing_partition: &enrolled.routing_partition,
        workspace_id: &enrolled.workspace_id,
    };
    let identity_path = worker_dir.join("identity.json");
    let identity_json = serde_json::to_vec_pretty(&identity)
        .map_err(|e| format!("failed to serialize worker identity: {e}"))?;
    std::fs::write(&identity_path, &identity_json)
        .map_err(|e| format!("failed to write {}: {e}", identity_path.display()))?;

    let token_path = worker_dir.join("worker.token");
    write_secret(&token_path, enrolled.worker_token.as_bytes())?;

    let nk_path = worker_dir.join("user.nk");
    write_secret(&nk_path, nats_seed.as_bytes())?;

    // Assemble + persist the `.creds` the daemon connects with, when mekhan
    // returned a JWT. The seed is the one generated locally above.
    let creds_path = worker_dir.join("worker.creds");
    let creds_written = if let Some(jwt) = enrolled.nats_jwt.as_deref() {
        let creds = assemble_creds(jwt, &nats_seed);
        write_secret(&creds_path, creds.as_bytes())?;
        true
    } else {
        false
    };

    info!(
        worker_id = %enrolled.id,
        workspace_id = %enrolled.workspace_id,
        group = ?enrolled.group,
        routing_partition = %enrolled.routing_partition,
        dir = %worker_dir.display(),
        nats_creds = creds_written,
        "worker enrolled"
    );

    Ok(EnrolledWorkerLocal {
        worker_id: enrolled.id,
        group: enrolled.group,
        routing_partition: enrolled.routing_partition,
        creds_path: creds_written.then_some(creds_path),
    })
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
