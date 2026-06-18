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

/// Vault KV-v2 path (under the default `secret` mount) for the persisted
/// account signing seed — resolves to
/// `secret/data/aithericon/runners/nats_signing_seed`.
const VAULT_SEED_PATH: &str = "aithericon/runners/nats_signing_seed";
/// Field within the Vault secret that holds the seed.
const VAULT_SEED_FIELD: &str = "seed";
/// Local file (under `data_dir`) holding the account signing seed.
const SEED_FILE_NAME: &str = "runners_account_signing.nk";

/// Shared apalis stream-family key for lab-runner-fleet job delivery. Mirrors
/// `aithericon_executor_worker::config::RUNNER_JOBS_NAMESPACE` (`"runner-jobs"`);
/// re-stated here to build the `$JS.API.*` pull allow-list for a runner's
/// partition-keyed consumer without depending on the executor crate.
const RUNNER_JOBS_NAMESPACE: &str = "runner-jobs";

/// The three apalis priority lanes (apalis-nats `storage.rs`). Each maps to a
/// per-priority stream `{namespace}_{prio}` + a partition-keyed durable.
const PRIORITIES: [&str; 3] = ["high", "medium", "low"];

/// The shared JetStream streams every executor daemon (runner OR worker) ensures
/// at boot, before draining any job: the status/events fan-in + the chunked-data
/// transport stream. A scoped JWT must be allowed `STREAM.INFO/CREATE/UPDATE` on
/// these or it fails at startup before it can drain.
const SHARED_STREAMS: [&str; 3] = ["EXECUTOR_STATUS", "EXECUTOR_EVENTS", "EXECUTOR_DATASTREAM"];

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
    /// The account *identity* public key, when `account_kp` is an account
    /// **signing key** rather than the account identity itself. Set as the
    /// minted user JWT's `issuer_account` claim so the NATS resolver can map the
    /// signing-key issuer back to its account; required by NATS whenever a user
    /// JWT is signed by a signing key. `None` when `account_kp` IS the account
    /// identity (the standalone/dev auto-generate case) — then `iss` already is
    /// the account and no `issuer_account` is needed.
    issuer_account: Option<String>,
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
            issuer_account: None,
        }
    }

    /// Set the account identity public key for signing-key mode. When `account`
    /// is non-empty AND differs from this signer's own public key (i.e.
    /// `account_kp` is a signing key, not the account identity), it becomes the
    /// `issuer_account` claim on every minted user JWT. A value equal to the
    /// signer's own key — or empty — leaves `issuer_account` unset (the signer
    /// already IS the account).
    pub fn with_issuer_account(mut self, account: Option<String>) -> Self {
        self.issuer_account = account
            .map(|a| a.trim().to_owned())
            .filter(|a| !a.is_empty() && a != &self.account_public_key);
        self
    }

    /// Resolve the runner-signing signer: the keypair (by the documented seed
    /// precedence) plus, when that keypair is an account **signing key**, the
    /// account identity from `RUNNERS_NATS_ACCOUNT_ID` to stamp as
    /// `issuer_account`. Never fails — falls through to generate-and-persist on
    /// any miss.
    pub fn resolve() -> Self {
        Self::from_keypair(Self::resolve_account_kp())
            .with_issuer_account(std::env::var("RUNNERS_NATS_ACCOUNT_ID").ok())
    }

    /// Resolve just the account signing keypair using the documented precedence.
    fn resolve_account_kp() -> KeyPair {
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
                        return kp;
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
                    return kp;
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
                    return kp;
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
        kp
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
        // Job delivery is NOT a core-NATS subscribe: presence-pool grants land on
        // the SHARED `runner-jobs` JetStream stream-set, drained via a
        // partition-keyed pull consumer per priority
        // (`runner-jobs_{prio}_{runner_id}_consumer`). Grant exactly the
        // `$JS.API.*` ops that consumer needs (STREAM.INFO + CONSUMER
        // CREATE/INFO/MSG.NEXT), its `$JS.ACK.>` reply subjects, and the
        // `runner-jobs.dlq` publish — scoped to THIS runner's durable so it can
        // bind and drain its lane but touch no other partition.
        pub_allow.extend(jetstream_pull_pub_allow(
            RUNNER_JOBS_NAMESPACE,
            &runner_id.to_string(),
        ));
        // The shared status/events/datastream streams the daemon ensures at boot
        // (STREAM.INFO/CREATE/UPDATE) + the chunked-data publish subject.
        pub_allow.extend(jetstream_shared_stream_pub_allow());

        // This runner's own control subject PLUS the request/reply mux inbox.
        // async-nats subscribes ONCE to `_INBOX.>` (default prefix) and ALL
        // JetStream pull deliveries and `$JS.API.*` replies arrive there. Without
        // this the inbox subscribe is denied and draining silently dies — the
        // `resp` ResponsePermission only lets a RESPONDER publish to learned
        // reply subjects, NOT a requester subscribe to its own inbox.
        let sub_allow = vec![format!("runner.{runner_id}.>"), "_INBOX.>".to_string()];

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
            // When signing with an account signing key, the resolver needs the
            // account identity here to map `iss` (the signing key) → account.
            .issuer_account(self.issuer_account.clone())
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
    ///     `executor-<wire>-grp.*.<group>.>` (grouped jobs ride the parallel
    ///     `executor-<wire>-grp` stream; the `*` is the priority token, the
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

        // Own control subject + the request/reply mux inbox — same reason as the
        // runner mint: all JetStream pull deliveries + `$JS.API.*` replies land
        // on the default-prefix `_INBOX.>`, so without it draining silently dies.
        let mut sub_allow = vec![format!("worker.{worker_id}.>"), "_INBOX.>".to_string()];
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
                // (no validation needed here). Grouped jobs ride the PARALLEL
                // `executor-<wire>-grp` stream (the D1 isolation decision — see
                // `ExecutionBackendType::executor_namespace_for_group`), so the
                // subscribe filter is `executor-<wire>-grp.<prio>.<group>.>`. The
                // worker is scoped to its group's pull subjects only; it can
                // never see the anonymous `executor-<wire>` pool's stream.
                for wire in backends {
                    let ns = aithericon_backends::executor_pool_namespace(wire);
                    sub_allow.push(format!("{ns}-grp.*.{group}.>"));
                }
            }
        }

        // A worker publishes its presence heartbeat and the per-job status/events
        // it drains. No `.claim` — that is the presence-push admission grant a
        // worker never participates in (it competes on a pull queue).
        let mut pub_allow = vec![
            format!("worker.{worker_id}.presence"),
            "executor.status.*.>".to_string(),
            "executor.events.*.>".to_string(),
        ];

        // JetStream pull scope, one stream-family per advertised backend wire. A
        // grouped worker is a competing pull consumer on the PARALLEL
        // `executor-<wire>-grp` stream-set, keyed on its group's routing
        // partition. Only meaningful when grouped (an ungrouped worker has no
        // partition to bind a durable on, mirroring the subscribe-filter
        // omission above).
        if let Some(group) = group {
            if !group.is_empty() {
                for wire in backends {
                    let ns = format!("{}-grp", aithericon_backends::executor_pool_namespace(wire));
                    pub_allow.extend(jetstream_pull_pub_allow(&ns, group));
                }
            }
        }
        // The shared status/events/datastream streams every worker ensures at
        // boot (the StatusReporter runs regardless of grouping).
        pub_allow.extend(jetstream_shared_stream_pub_allow());

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
            // Account identity for the resolver when signing with a signing key.
            .issuer_account(self.issuer_account.clone())
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

/// Run a Vault call to completion on a fresh current-thread runtime in a
/// scoped thread, so the sync `resolve()` path can use the async
/// `aithericon_secrets` store without coupling to the caller's async context.
/// Returns `None` on runtime-build failure or when the 3s budget elapses —
/// Vault is optional and must not block startup.
fn block_on_vault<T: Send>(fut: impl std::future::Future<Output = T> + Send) -> Option<T> {
    std::thread::scope(|s| {
        s.spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            rt.block_on(async move {
                tokio::time::timeout(std::time::Duration::from_secs(3), fut)
                    .await
                    .ok()
            })
        })
        .join()
        .ok()
        .flatten()
    })
}

/// Best-effort KV-v2 read of the account seed from Vault via the shared
/// `aithericon_secrets` store. Returns `None` (never errors) on any failure.
fn vault_read_seed() -> Option<String> {
    use aithericon_secrets::SecretStore as _;
    let store = aithericon_secrets::VaultSecretStore::from_env()?;
    let key = format!("{VAULT_SEED_PATH}#{VAULT_SEED_FIELD}");
    block_on_vault(async move { store.get(&key).await })?.ok()
}

/// Best-effort KV-v2 write of the account seed to Vault via the shared
/// `aithericon_secrets` store. Logs + swallows any failure — the local file
/// is authoritative.
fn vault_write_seed(seed: &str) {
    let Some(store) = aithericon_secrets::VaultSecretStore::from_env() else {
        return;
    };
    let mut fields = serde_json::Map::new();
    fields.insert(
        VAULT_SEED_FIELD.to_string(),
        serde_json::Value::String(seed.to_string()),
    );
    match block_on_vault(async move { store.put_kv(VAULT_SEED_PATH, &fields).await }) {
        Some(Ok(())) => tracing::info!("mirrored runner NATS signing seed to Vault"),
        _ => tracing::debug!("could not mirror runner NATS signing seed to Vault (best-effort)"),
    }
}

/// Build the `$JS.API.*` + ACK + DLQ **publish** allow-list a partition-keyed
/// pull consumer needs to bind and drain one apalis stream-family.
///
/// `namespace` is the apalis stream-family key (`runner-jobs` for a runner,
/// `executor-<wire>-grp` for a worker); `partition` is the durable's partition
/// token (the runner_id UUID, or the worker group's routing-partition UUID). For
/// each priority lane apalis `NatsStorage` creates a `{namespace}_{prio}` stream
/// and a durable `{namespace}_{prio}_{partition}_consumer`; a pull consumer
/// drives `STREAM.INFO` → `CONSUMER.CREATE` (CreateOrUpdate on every reconnect)
/// → `CONSUMER.INFO` → `CONSUMER.MSG.NEXT`, and acks land on a server-assigned
/// `$JS.ACK.{stream}.{consumer}.…` reply subject. All entries are PUBLISH
/// subjects (the `$JS.API.*` reply rides the `_INBOX.>` subscribe). Both the
/// 2-token and 3-token filtered consumer-create forms are granted (async-nats
/// 0.42 sends the filtered variant for a single-filter pull consumer), plus the
/// legacy `DURABLE.CREATE`. No `STREAM.CREATE/UPDATE` (job streams are
/// pre-created in-cluster) and no `CONSUMER.DELETE` — keeping it tight.
fn jetstream_pull_pub_allow(namespace: &str, partition: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(PRIORITIES.len() * 7 + 1);
    for prio in PRIORITIES {
        let stream = format!("{namespace}_{prio}");
        let durable = format!("{stream}_{partition}_consumer");
        out.push(format!("$JS.API.STREAM.INFO.{stream}"));
        out.push(format!("$JS.API.CONSUMER.CREATE.{stream}.{durable}"));
        out.push(format!("$JS.API.CONSUMER.CREATE.{stream}.{durable}.>"));
        out.push(format!(
            "$JS.API.CONSUMER.DURABLE.CREATE.{stream}.{durable}"
        ));
        out.push(format!("$JS.API.CONSUMER.INFO.{stream}.{durable}"));
        out.push(format!("$JS.API.CONSUMER.MSG.NEXT.{stream}.{durable}"));
        // Server-assigned ack reply subject tail → wildcard, scoped per
        // (stream, durable) so an identity can only ack its own deliveries.
        out.push(format!("$JS.ACK.{stream}.{durable}.>"));
    }
    // The apalis ack path publishes to `{namespace}.dlq` on terminal failure.
    out.push(format!("{namespace}.dlq"));
    out
}

/// Build the `$JS.API.STREAM.*` management + datastream publish allow-list for
/// the [`SHARED_STREAMS`] every executor daemon ensures at boot.
///
/// Before draining, a runner/worker calls `StatusReporter::new`
/// (→ `get_or_create_stream` on `EXECUTOR_STATUS` + `EXECUTOR_EVENTS`) and
/// ensures `EXECUTOR_DATASTREAM`. `get_or_create_stream` issues
/// `$JS.API.STREAM.INFO.{name}` and, when absent, `STREAM.CREATE`/`UPDATE` — so
/// a scoped JWT lacking these fails at startup. The status/events PUBLISH
/// subjects are granted at the call site; this adds the stream-management API
/// plus the chunked-data publish subject. No `STREAM.DELETE/PURGE` — these are
/// shared streams a scoped identity must never tear down.
fn jetstream_shared_stream_pub_allow() -> Vec<String> {
    let mut out = Vec::with_capacity(SHARED_STREAMS.len() * 3 + 1);
    for stream in SHARED_STREAMS {
        out.push(format!("$JS.API.STREAM.INFO.{stream}"));
        out.push(format!("$JS.API.STREAM.CREATE.{stream}"));
        out.push(format!("$JS.API.STREAM.UPDATE.{stream}"));
    }
    out.push("executor.datastream.*.>".to_string());
    out
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

    fn allow_list(claims: &Value, dir: &str) -> Vec<String> {
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
    fn signing_key_mode_sets_issuer_account() {
        // Signer keypair is an account *signing key*; a distinct account
        // identity is supplied → every minted JWT must stamp it as
        // `issuer_account` so the resolver can map `iss` (the signing key) back
        // to the account. Without this NATS rejects with `authorization
        // violation`.
        let account_identity = KeyPair::new_account().public_key();
        let signer = RunnerNatsSigner::from_keypair(KeyPair::new_account())
            .with_issuer_account(Some(account_identity.clone()));

        let user_pub = KeyPair::new_user().public_key();
        let jwt = signer
            .mint_runner_jwt(&user_pub, Uuid::new_v4(), Some("lab"))
            .expect("mint should succeed");
        let claims = decode_claims(&jwt);

        assert_eq!(claims["iss"], signer.account_public_key());
        assert_eq!(claims["nats"]["issuer_account"], account_identity);

        // Workers ride the same signer → same issuer_account stamping.
        let wjwt = signer
            .mint_worker_jwt(&user_pub, Uuid::new_v4(), Some("grp"), &["python".into()])
            .expect("worker mint should succeed");
        assert_eq!(
            decode_claims(&wjwt)["nats"]["issuer_account"],
            account_identity
        );
    }

    #[test]
    fn runner_jwt_grants_jetstream_pull_and_inbox_scope() {
        // Regression: a runner connecting to a real (auth-enabled) NATS needs
        // `_INBOX.>` (sub) + `$JS.API.*`/`$JS.ACK.>` (pub) or it fails with
        // `Permissions Violation` at consumer-bind / stream-ensure and never
        // drains. Lock the exact subjects the apalis pull consumer drives.
        let signer = RunnerNatsSigner::from_keypair(KeyPair::new_account());
        let id = Uuid::new_v4();
        let jwt = signer
            .mint_runner_jwt(&KeyPair::new_user().public_key(), id, Some("lab"))
            .expect("mint should succeed");
        let claims = decode_claims(&jwt);

        let sub = allow_list(&claims, "sub");
        assert!(
            sub.contains(&"_INBOX.>".to_string()),
            "needs _INBOX.> subscribe"
        );

        let pub_allow = allow_list(&claims, "pub");
        let stream = format!("{RUNNER_JOBS_NAMESPACE}_high");
        let durable = format!("{stream}_{id}_consumer");
        for want in [
            format!("$JS.API.STREAM.INFO.{stream}"),
            format!("$JS.API.CONSUMER.CREATE.{stream}.{durable}.>"),
            format!("$JS.API.CONSUMER.MSG.NEXT.{stream}.{durable}"),
            format!("$JS.ACK.{stream}.{durable}.>"),
            format!("{RUNNER_JOBS_NAMESPACE}.dlq"),
            "$JS.API.STREAM.INFO.EXECUTOR_STATUS".to_string(),
            "$JS.API.STREAM.CREATE.EXECUTOR_EVENTS".to_string(),
        ] {
            assert!(pub_allow.contains(&want), "missing pub grant: {want}");
        }
    }

    #[test]
    fn identity_signer_omits_issuer_account() {
        // When the signer IS the account identity (issuer == own key, or empty),
        // no issuer_account is stamped — `iss` already is the account.
        let kp = KeyPair::new_account();
        let own = kp.public_key();
        let signer = RunnerNatsSigner::from_keypair(kp).with_issuer_account(Some(own));

        let user_pub = KeyPair::new_user().public_key();
        let jwt = signer
            .mint_runner_jwt(&user_pub, Uuid::new_v4(), None)
            .expect("mint should succeed");
        assert!(
            decode_claims(&jwt)["nats"]["issuer_account"].is_null(),
            "issuer_account must be absent when the signer is the account identity"
        );
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
        // worker's own control inbox. The filter is `executor-<wire>-grp.*.<group>.>`
        // (priority `*`, the group token, then the exec-id tail).
        let sub_allow = allow_list(&claims, "sub");
        assert!(sub_allow.contains(&format!("worker.{id}.>")));
        assert!(sub_allow.contains(&format!("executor-python-grp.*.{group}.>")));
        assert!(sub_allow.contains(&format!("executor-docker-grp.*.{group}.>")));

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
                .mint_worker_jwt(
                    &user_pub,
                    Uuid::new_v4(),
                    Some(bad),
                    &["python".to_string()],
                )
                .expect_err("a worker group with NATS subject metacharacters must be rejected");
            assert!(matches!(err, MintError::InvalidPool(_)), "group {bad:?}");
        }
    }
}
