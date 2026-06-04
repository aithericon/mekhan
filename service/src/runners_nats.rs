//! Phase 2 — Lab Runner Fleet NATS scoped-credential minting.
//!
//! mekhan owns the `runners` NATS account signing key and mints a scoped,
//! per-runner *user* JWT for each enrolled runner. The runner generates its own
//! user nkey locally, sends only the PUBLIC key at enrollment (stored in
//! `runners.nats_public_key`), and mekhan signs a user JWT bound to that public
//! key whenever asked (at enroll, or later via `POST /runners/{id}/nats-creds`).
//! The account seed never leaves mekhan; the user seed never leaves the runner.
//!
//! ## Signing-key resolution precedence
//!
//! The account signing keypair is resolved once at startup, in this order
//! (first hit wins). A real provided seed always wins; for an offline dev box
//! we auto-generate and persist a stable seed so the key survives the ephemeral
//! dev Vault being wiped:
//!
//!   1. env `RUNNERS_NATS_SIGNING_SEED` — an account seed (`SA…`).
//!   2. local file `{data_dir}/runners_account_signing.nk` if present (the
//!      STABLE dev store; survives `just dev reset` wiping Vault).
//!   3. best-effort Vault read at
//!      `secret/data/aithericon/runners/nats_signing_seed` (KV v2) when
//!      `VAULT_ADDR` + `VAULT_TOKEN` are set. Failures are non-fatal.
//!   4. else GENERATE a fresh account keypair, persist its seed to the local
//!      file (0600) AND best-effort to Vault, and log the account public key.
//!
//! `data_dir` = env `MEKHAN_DATA_DIR`, defaulting to `~/.aithericon/mekhan`
//! (created 0700 if missing). There is no pre-existing data-dir convention in
//! the codebase, so this introduces one scoped to the runner signing key.

use std::path::PathBuf;

use nats_io_jwt::{KeyPair, Permission, ResponsePermission, StringList, Token, User};
use uuid::Uuid;

/// Vault KV-v2 logical path for the persisted account signing seed.
const VAULT_SECRET_PATH: &str = "secret/data/aithericon/runners/nats_signing_seed";
/// Local file (under `data_dir`) holding the account signing seed.
const SEED_FILE_NAME: &str = "runners_account_signing.nk";

/// Errors raised while minting a runner user JWT.
#[derive(Debug, thiserror::Error)]
pub enum MintError {
    /// The supplied user public key is not a NATS *user* nkey (must start `U`).
    #[error("invalid NATS user public key: {0}")]
    InvalidUserKey(String),

    #[error("invalid pool name (must be a single [A-Za-z0-9_-] token): {0}")]
    InvalidPool(String),
    /// The `nats-io-jwt` builder rejected the claims we assembled.
    #[error("failed to build user JWT claims: {0}")]
    BuildClaims(String),
}

/// Holds the `runners`-account signing keypair and its cached public key.
///
/// Always constructed (auto-generates in dev), so `AppState` carries it
/// unconditionally — minting can still *fail* per-call (a malformed runner
/// public key), but the signer itself is always present.
pub struct RunnerNatsSigner {
    account_kp: KeyPair,
    account_public_key: String,
}

impl std::fmt::Debug for RunnerNatsSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the keypair (it holds the seed).
        f.debug_struct("RunnerNatsSigner")
            .field("account_public_key", &self.account_public_key)
            .finish_non_exhaustive()
    }
}

impl RunnerNatsSigner {
    /// Generate a signer backed by a fresh, ephemeral account keypair. Handy
    /// for tests + any call site that just needs *a* valid signer without
    /// persistence (the seed is never written anywhere).
    pub fn generate_ephemeral() -> Self {
        Self::from_keypair(KeyPair::new_account())
    }

    /// Build a signer directly from an account [`KeyPair`]. Used by tests and
    /// by [`resolve`](Self::resolve) once a seed has been obtained.
    pub fn from_keypair(account_kp: KeyPair) -> Self {
        let account_public_key = account_kp.public_key();
        Self {
            account_kp,
            account_public_key,
        }
    }

    /// Resolve the account signing keypair using the documented precedence.
    /// Never fails: falls through to generate-and-persist on any miss.
    pub fn resolve() -> Self {
        // 1. Explicit seed from env always wins.
        if let Ok(seed) = std::env::var("RUNNERS_NATS_SIGNING_SEED") {
            let seed = seed.trim();
            if !seed.is_empty() {
                match KeyPair::from_seed(seed) {
                    Ok(kp) => {
                        tracing::info!(
                            account = %kp.public_key(),
                            "runner NATS signing account: from RUNNERS_NATS_SIGNING_SEED"
                        );
                        return Self::from_keypair(kp);
                    }
                    Err(e) => tracing::warn!(
                        error = %e,
                        "RUNNERS_NATS_SIGNING_SEED set but unparseable — falling through"
                    ),
                }
            }
        }

        let data_dir = data_dir();
        let seed_file = data_dir.join(SEED_FILE_NAME);

        // 2. Stable local dev store.
        if let Ok(contents) = std::fs::read_to_string(&seed_file) {
            let seed = contents.trim();
            match KeyPair::from_seed(seed) {
                Ok(kp) => {
                    tracing::info!(
                        account = %kp.public_key(),
                        path = %seed_file.display(),
                        "runner NATS signing account: from local seed file"
                    );
                    return Self::from_keypair(kp);
                }
                Err(e) => tracing::warn!(
                    error = %e,
                    path = %seed_file.display(),
                    "local runner-signing seed file unparseable — ignoring"
                ),
            }
        }

        // 3. Best-effort Vault read.
        if let Some(seed) = vault_read_seed() {
            match KeyPair::from_seed(seed.trim()) {
                Ok(kp) => {
                    tracing::info!(
                        account = %kp.public_key(),
                        "runner NATS signing account: from Vault"
                    );
                    // Mirror it to the local file so it survives Vault wipes.
                    persist_seed_to_file(&data_dir, &seed_file, &seed);
                    return Self::from_keypair(kp);
                }
                Err(e) => tracing::warn!(
                    error = %e,
                    "Vault runner-signing seed unparseable — falling through"
                ),
            }
        }

        // 4. Generate + persist (local file authoritative, Vault best-effort).
        let kp = KeyPair::new_account();
        let public = kp.public_key();
        match kp.seed() {
            Ok(seed) => {
                persist_seed_to_file(&data_dir, &seed_file, &seed);
                vault_write_seed(&seed);
            }
            Err(e) => tracing::warn!(
                error = %e,
                "could not export generated account seed — signer is process-local this run"
            ),
        }
        tracing::info!(
            account = %public,
            path = %seed_file.display(),
            "runner NATS signing account: generated a fresh account key (dev)"
        );
        Self::from_keypair(kp)
    }

    /// The account signing key's PUBLIC key (`A…`) — the issuer of every minted
    /// user JWT, and the value the NATS server's account-resolver must trust.
    pub fn account_public_key(&self) -> &str {
        &self.account_public_key
    }

    /// Mint a scoped user JWT for `runner_id` (in optional `pool`), bound to the
    /// runner-supplied `user_public_key`. Signed by the account keypair.
    ///
    /// Scope (subject taxonomy, docs/21 §4.5):
    ///   PUBLISH allow: `executor.status.{id}.>`, `executor.events.{id}.>`,
    ///   `runner.{id}.presence`, and `{pool}.claim` (only when pooled).
    ///   SUBSCRIBE allow: `runner.{id}.>`.
    ///   Plus a ResponsePermission so async-nats request/reply works.
    /// The JWT carries no expiry (long-lived; rotation = re-mint).
    pub fn mint_runner_jwt(
        &self,
        user_public_key: &str,
        runner_id: Uuid,
        pool: Option<&str>,
    ) -> Result<String, MintError> {
        if !user_public_key.starts_with('U') {
            return Err(MintError::InvalidUserKey(format!(
                "expected a NATS user key (starts with 'U'), got: {user_public_key}"
            )));
        }

        let mut pub_allow = vec![
            format!("executor.status.{runner_id}.>"),
            format!("executor.events.{runner_id}.>"),
            format!("runner.{runner_id}.presence"),
        ];
        if let Some(pool) = pool {
            if !pool.is_empty() {
                // Defense in depth: `pool` is validated at the registration-token
                // mint API, but it lands here as a literal NATS subject token, so
                // reject anything that isn't a single safe token before granting
                // `{pool}.claim` (no '.', '*', '>', or whitespace broadening it).
                if !pool
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    return Err(MintError::InvalidPool(pool.to_string()));
                }
                pub_allow.push(format!("{pool}.claim"));
            }
        }
        // Control/presence-adjacent subjects for this runner. Job delivery is
        // NOT a core-NATS subscribe: presence-pool grants land on the SHARED
        // `runner-jobs` JetStream stream, drained via a partition-filtered pull
        // consumer (`runner-jobs.{prio}.{runner_id}.>`). Scoping that pull
        // (JetStream `$JS.API.*` perms for the shared stream + this runner's
        // durable) is a separate prod-hardening concern; dev NATS is open.
        let sub_allow = vec![format!("runner.{runner_id}.>")];

        let pub_perm: Permission = Permission::builder()
            .allow(Some(StringList::from(pub_allow)))
            .try_into()
            .map_err(|e| MintError::BuildClaims(format!("pub permission: {e}")))?;
        let sub_perm: Permission = Permission::builder()
            .allow(Some(StringList::from(sub_allow)))
            .try_into()
            .map_err(|e| MintError::BuildClaims(format!("sub permission: {e}")))?;
        // Allow transient reply subjects (request/reply). `max(1)` matches the
        // common NATS default — one in-flight response per request.
        let resp_perm: ResponsePermission = ResponsePermission::builder()
            .max(Some(1i64))
            .try_into()
            .map_err(|e| MintError::BuildClaims(format!("resp permission: {e}")))?;

        let user: User = User::builder()
            .pub_(Some(pub_perm))
            .sub(Some(sub_perm))
            .resp(Some(resp_perm))
            .try_into()
            .map_err(|e| MintError::BuildClaims(format!("user claims: {e}")))?;

        // No `.expires(..)` → long-lived JWT (rotation is an explicit re-mint).
        let jwt = Token::new(user_public_key.to_string())
            .name(format!("runner-{runner_id}"))
            .claims(user)
            .sign(&self.account_kp);
        Ok(jwt)
    }

    /// Mint a scoped user JWT for an enrolled *worker* (Phase A, Grouped +
    /// Enrolled Workers), bound to the worker-supplied `user_public_key`. Signed
    /// by the SAME account keypair that signs runner JWTs — workers and runners
    /// share the one runners-account signer, only the scope differs.
    ///
    /// The worker is a competing *pull* consumer, NOT a presence-push grant
    /// target, so the scope is materially different from [`Self::mint_runner_jwt`]:
    ///   SUBSCRIBE allow: per advertised backend wire, the group's pull filter
    ///     `executor-<wire>.*.<group>.>` (the `*` is the priority token, the
    ///     `<group>` is the second coarse routing coordinate, the `>` spans the
    ///     exec-id tail) — plus `worker.{id}.>` for its own control/presence
    ///     inbox. When the worker is UNGROUPED (`group = None`) it gets only the
    ///     `worker.{id}.>` subscribe; the anonymous `executor-<wire>` pull path
    ///     is governed by the (dev-open) JetStream pull perms, not a core
    ///     subscribe, exactly as the runner job-delivery comment above explains.
    ///   PUBLISH allow: `worker.{id}.presence` (its heartbeat), and the shared
    ///     status/events fan-in `executor.status.*.>` / `executor.events.*.>`
    ///     (a worker reports on whatever exec-id it is currently draining, so the
    ///     `*` spans the exec-id — it does not get a `.claim` subject; that was
    ///     the presence-push grant a worker never participates in).
    /// The JWT carries no expiry (long-lived; rotation = re-mint).
    pub fn mint_worker_jwt(
        &self,
        user_public_key: &str,
        worker_id: Uuid,
        group: Option<&str>,
        backends: &[String],
    ) -> Result<String, MintError> {
        if !user_public_key.starts_with('U') {
            return Err(MintError::InvalidUserKey(format!(
                "expected a NATS user key (starts with 'U'), got: {user_public_key}"
            )));
        }

        let mut sub_allow = vec![format!("worker.{worker_id}.>")];
        if let Some(group) = group {
            if !group.is_empty() {
                // Defense in depth: `group` is validated at the registration-token
                // mint API, but it lands here as a literal NATS subject token, so
                // reject anything that isn't a single safe token before granting
                // the group pull filter (no '.', '*', '>', or whitespace
                // broadening it). Reuses the same `MintError::InvalidPool` variant
                // — it is the generic "unsafe subject token" mint error.
                if !group
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    return Err(MintError::InvalidPool(group.to_string()));
                }
                // One group pull filter per advertised backend wire. Backend
                // wire-names are `[a-z_]`, so they are valid NATS subject tokens
                // (no validation needed here). `executor_pool_namespace` is the
                // single source of truth for the `executor-<wire>` prefix the
                // compiler stamps onto a grouped step.
                for wire in backends {
                    let ns = aithericon_backends::executor_pool_namespace(wire);
                    sub_allow.push(format!("{ns}.*.{group}.>"));
                }
            }
        }

        // A worker publishes its presence heartbeat and the per-job status/events
        // it drains. No `.claim` — that is the presence-push admission grant a
        // worker never participates in (it competes on a pull queue).
        let pub_allow = vec![
            format!("worker.{worker_id}.presence"),
            "executor.status.*.>".to_string(),
            "executor.events.*.>".to_string(),
        ];

        let pub_perm: Permission = Permission::builder()
            .allow(Some(StringList::from(pub_allow)))
            .try_into()
            .map_err(|e| MintError::BuildClaims(format!("pub permission: {e}")))?;
        let sub_perm: Permission = Permission::builder()
            .allow(Some(StringList::from(sub_allow)))
            .try_into()
            .map_err(|e| MintError::BuildClaims(format!("sub permission: {e}")))?;
        // Allow transient reply subjects (request/reply). `max(1)` matches the
        // common NATS default — one in-flight response per request.
        let resp_perm: ResponsePermission = ResponsePermission::builder()
            .max(Some(1i64))
            .try_into()
            .map_err(|e| MintError::BuildClaims(format!("resp permission: {e}")))?;

        let user: User = User::builder()
            .pub_(Some(pub_perm))
            .sub(Some(sub_perm))
            .resp(Some(resp_perm))
            .try_into()
            .map_err(|e| MintError::BuildClaims(format!("user claims: {e}")))?;

        // No `.expires(..)` → long-lived JWT (rotation is an explicit re-mint).
        let jwt = Token::new(user_public_key.to_string())
            .name(format!("worker-{worker_id}"))
            .claims(user)
            .sign(&self.account_kp);
        Ok(jwt)
    }
}

/// Resolve the data dir: `MEKHAN_DATA_DIR` or `~/.aithericon/mekhan`.
fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("MEKHAN_DATA_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".aithericon").join("mekhan")
}

/// Create `data_dir` (0700) if missing and write `seed_file` (0600). Best-effort
/// — logs a warning on failure and otherwise carries on (the in-memory keypair
/// still works for this process).
fn persist_seed_to_file(data_dir: &std::path::Path, seed_file: &std::path::Path, seed: &str) {
    if let Err(e) = std::fs::create_dir_all(data_dir) {
        tracing::warn!(error = %e, dir = %data_dir.display(), "could not create data dir for runner signing seed");
        return;
    }
    set_dir_mode_0700(data_dir);

    if let Err(e) = std::fs::write(seed_file, seed) {
        tracing::warn!(error = %e, path = %seed_file.display(), "could not persist runner signing seed");
        return;
    }
    set_file_mode_0600(seed_file);
}

#[cfg(unix)]
fn set_dir_mode_0700(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)) {
        tracing::warn!(error = %e, path = %path.display(), "could not chmod 0700 data dir");
    }
}

#[cfg(unix)]
fn set_file_mode_0600(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!(error = %e, path = %path.display(), "could not chmod 0600 seed file");
    }
}

#[cfg(not(unix))]
fn set_dir_mode_0700(_path: &std::path::Path) {}
#[cfg(not(unix))]
fn set_file_mode_0600(_path: &std::path::Path) {}

/// Best-effort KV-v2 read of the account seed from Vault. Returns `None` (never
/// errors) on any failure — Vault is optional and must not block startup.
fn vault_read_seed() -> Option<String> {
    let addr = std::env::var("VAULT_ADDR").ok()?;
    let token = std::env::var("VAULT_TOKEN").ok()?;
    let addr = addr.trim_end_matches('/');
    let url = format!("{addr}/v1/{VAULT_SECRET_PATH}");

    // A short blocking call on its own runtime so this works from the sync
    // `resolve()` path without coupling to the caller's async context.
    let body: serde_json::Value = std::thread::scope(|s| {
        s.spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            rt.block_on(async move {
                let client = reqwest::Client::new();
                let resp = client
                    .get(&url)
                    .header("X-Vault-Token", token)
                    .timeout(std::time::Duration::from_secs(3))
                    .send()
                    .await
                    .ok()?;
                if !resp.status().is_success() {
                    return None;
                }
                resp.json::<serde_json::Value>().await.ok()
            })
        })
        .join()
        .ok()
        .flatten()
    })?;

    body.get("data")?
        .get("data")?
        .get("seed")?
        .as_str()
        .map(str::to_string)
}

/// Best-effort KV-v2 write of the account seed to Vault. Logs + swallows any
/// failure — the local file is authoritative.
fn vault_write_seed(seed: &str) {
    let (addr, token) = match (std::env::var("VAULT_ADDR"), std::env::var("VAULT_TOKEN")) {
        (Ok(a), Ok(t)) => (a, t),
        _ => return,
    };
    let addr = addr.trim_end_matches('/').to_string();
    let url = format!("{addr}/v1/{VAULT_SECRET_PATH}");
    let seed = seed.to_string();

    let ok = std::thread::scope(|s| {
        s.spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(_) => return false,
            };
            rt.block_on(async move {
                let client = reqwest::Client::new();
                let payload = serde_json::json!({ "data": { "seed": seed } });
                client
                    .put(&url)
                    .header("X-Vault-Token", token)
                    .timeout(std::time::Duration::from_secs(3))
                    .json(&payload)
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false)
            })
        })
        .join()
        .unwrap_or(false)
    });
    if ok {
        tracing::info!("mirrored runner NATS signing seed to Vault");
    } else {
        tracing::debug!("could not mirror runner NATS signing seed to Vault (best-effort)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use serde_json::Value;

    /// Decode the (unverified) claims segment of a NATS user JWT.
    fn decode_claims(jwt: &str) -> Value {
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT must have 3 dot-separated parts");
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[1].as_bytes())
            .expect("payload base64url-decodes");
        serde_json::from_slice(&bytes).expect("payload is JSON")
    }

    fn allow_list<'a>(claims: &'a Value, dir: &str) -> Vec<String> {
        claims["nats"][dir]["allow"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn mints_pooled_runner_jwt_with_full_scope() {
        let signer = RunnerNatsSigner::from_keypair(KeyPair::new_account());
        let user_kp = KeyPair::new_user();
        let user_pub = user_kp.public_key();
        let id = Uuid::new_v4();
        let pool = "lab_fleet";

        let jwt = signer
            .mint_runner_jwt(&user_pub, id, Some(pool))
            .expect("mint should succeed for a valid user key");

        let claims = decode_claims(&jwt);
        assert_eq!(claims["iss"], signer.account_public_key());
        assert_eq!(claims["sub"], user_pub);

        let pub_allow = allow_list(&claims, "pub");
        assert!(pub_allow.contains(&format!("executor.status.{id}.>")));
        assert!(pub_allow.contains(&format!("executor.events.{id}.>")));
        assert!(pub_allow.contains(&format!("runner.{id}.presence")));
        assert!(pub_allow.contains(&format!("{pool}.claim")));

        let sub_allow = allow_list(&claims, "sub");
        assert!(sub_allow.contains(&format!("runner.{id}.>")));

        // ResponsePermission present so request/reply works.
        assert!(claims["nats"]["resp"].is_object());
    }

    #[test]
    fn poolless_runner_omits_claim_subject() {
        let signer = RunnerNatsSigner::from_keypair(KeyPair::new_account());
        let user_pub = KeyPair::new_user().public_key();
        let id = Uuid::new_v4();

        let jwt = signer
            .mint_runner_jwt(&user_pub, id, None)
            .expect("mint should succeed");
        let claims = decode_claims(&jwt);
        let pub_allow = allow_list(&claims, "pub");

        assert!(pub_allow.contains(&format!("runner.{id}.presence")));
        assert!(
            !pub_allow.iter().any(|s| s.ends_with(".claim")),
            "no pool → no .claim publish subject, got {pub_allow:?}"
        );
    }

    #[test]
    fn rejects_non_user_public_key() {
        let signer = RunnerNatsSigner::from_keypair(KeyPair::new_account());
        // An ACCOUNT key starts with 'A', not 'U' → must be rejected.
        let account_pub = KeyPair::new_account().public_key();
        let err = signer
            .mint_runner_jwt(&account_pub, Uuid::new_v4(), None)
            .expect_err("an account ('A…') key must be rejected");
        assert!(matches!(err, MintError::InvalidUserKey(_)));
    }

    #[test]
    fn rejects_pool_with_subject_metacharacters() {
        let signer = RunnerNatsSigner::from_keypair(KeyPair::new_account());
        let user_pub = KeyPair::new_user().public_key();
        for bad in ["*", ">", "a.b", "x.>", "has space", "p*"] {
            let err = signer
                .mint_runner_jwt(&user_pub, Uuid::new_v4(), Some(bad))
                .expect_err("a pool with NATS subject metacharacters must be rejected");
            assert!(matches!(err, MintError::InvalidPool(_)), "pool {bad:?}");
        }
        // A safe single token still mints.
        assert!(signer
            .mint_runner_jwt(&user_pub, Uuid::new_v4(), Some("lab_fleet-1"))
            .is_ok());
    }

    #[test]
    fn mints_grouped_worker_jwt_with_pull_filter_and_presence() {
        let signer = RunnerNatsSigner::from_keypair(KeyPair::new_account());
        let user_pub = KeyPair::new_user().public_key();
        let id = Uuid::new_v4();
        let group = "groupG";
        let backends = vec!["python".to_string(), "docker".to_string()];

        let jwt = signer
            .mint_worker_jwt(&user_pub, id, Some(group), &backends)
            .expect("mint should succeed for a valid user key");

        let claims = decode_claims(&jwt);
        assert_eq!(claims["iss"], signer.account_public_key());
        assert_eq!(claims["sub"], user_pub);

        // SUBSCRIBE: one group pull filter per advertised backend wire + the
        // worker's own control inbox. The filter is `executor-<wire>.*.<group>.>`
        // (priority `*`, the group token, then the exec-id tail).
        let sub_allow = allow_list(&claims, "sub");
        assert!(sub_allow.contains(&format!("worker.{id}.>")));
        assert!(sub_allow.contains(&format!("executor-python.*.{group}.>")));
        assert!(sub_allow.contains(&format!("executor-docker.*.{group}.>")));

        // PUBLISH: presence heartbeat + the shared status/events fan-in. NO
        // `.claim` — a worker is a pull competitor, not a presence-push target.
        let pub_allow = allow_list(&claims, "pub");
        assert!(pub_allow.contains(&format!("worker.{id}.presence")));
        assert!(pub_allow.contains(&"executor.status.*.>".to_string()));
        assert!(pub_allow.contains(&"executor.events.*.>".to_string()));
        assert!(
            !pub_allow.iter().any(|s| s.ends_with(".claim")),
            "a worker must never get a `.claim` publish subject, got {pub_allow:?}"
        );

        // ResponsePermission present so request/reply works.
        assert!(claims["nats"]["resp"].is_object());
    }

    #[test]
    fn ungrouped_worker_omits_pull_filter() {
        let signer = RunnerNatsSigner::from_keypair(KeyPair::new_account());
        let user_pub = KeyPair::new_user().public_key();
        let id = Uuid::new_v4();

        let jwt = signer
            .mint_worker_jwt(&user_pub, id, None, &["python".to_string()])
            .expect("mint should succeed");
        let claims = decode_claims(&jwt);
        let sub_allow = allow_list(&claims, "sub");

        assert!(sub_allow.contains(&format!("worker.{id}.>")));
        assert!(
            !sub_allow.iter().any(|s| s.starts_with("executor-")),
            "an ungrouped worker gets no group pull filter, got {sub_allow:?}"
        );
    }

    #[test]
    fn rejects_worker_group_with_subject_metacharacters() {
        let signer = RunnerNatsSigner::from_keypair(KeyPair::new_account());
        let user_pub = KeyPair::new_user().public_key();
        for bad in ["*", ">", "a.b", "x.>", "has space", "g*"] {
            let err = signer
                .mint_worker_jwt(&user_pub, Uuid::new_v4(), Some(bad), &["python".to_string()])
                .expect_err("a worker group with NATS subject metacharacters must be rejected");
            assert!(matches!(err, MintError::InvalidPool(_)), "group {bad:?}");
        }
    }
}
