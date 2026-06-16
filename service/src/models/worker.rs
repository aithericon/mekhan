//! Grouped + Enrolled Workers — DB row structs, wire DTOs, and the worker-token
//! prefixes (Phase A; docs/23 + docs/24).
//!
//! The exact parallel of [`crate::models::runner`] for the *worker* pool. A
//! worker is the long-running executor daemon that PULLS jobs off the per-backend
//! `executor-<wire>` work queues; this module gives it the same enrolled /
//! scoped-credential / revocable identity runners have, WITHOUT changing the pull
//! dispatch discipline.
//!
//! Two token families, each `<prefix>_{uuid}.{secret}`:
//!   - `wkr_{worker_id}.{secret}`  — worker control-plane credential.
//!   - `wt_{regtoken_id}.{secret}` — reusable enrollment / launch-template token.
//!
//! The token mint / parse / verify helpers are NOT reimplemented here — they are
//! the prefix-parameterized [`crate::models::runner::mint_token`] /
//! [`crate::models::runner::parse_token`] / [`crate::models::runner::verify_secret`]
//! called with the worker prefixes below, so the two families share one hashing
//! discipline (only the hash is persisted; plaintext is surfaced once at mint).
//!
//! These structs mirror the migration column order (see
//! `service/migrations/20240142000000_workers.sql`) so a `SELECT *` reads back
//! via `sqlx::FromRow` without surprises.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

// ── Token prefixes ─────────────────────────────────────────────────────────

/// Prefix of the worker control-plane credential.
pub const WORKER_TOKEN_PREFIX: &str = "wkr_";
/// Prefix of the worker registration token.
pub const WORKER_REG_TOKEN_PREFIX: &str = "wt_";

// ── DB rows ────────────────────────────────────────────────────────────────

/// One row from the `workers` table. Column order matches the migration.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WorkerRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    #[sqlx(rename = "worker_group")]
    pub group: Option<String>,
    /// The worker's routing PARTITION — the `capacity`-resource UUID of the
    /// worker group it competes in (the step's group, resolved alias→UUID, or the
    /// workspace's seeded `default` group). This is the token the executor binds
    /// its grouped consumer to (`executor-<wire>-grp.<prio>.<routing_partition>.>`);
    /// `group` above is the human alias kept for display. Workspace-safe by
    /// construction (UUID), unlike the alias.
    pub routing_partition: Uuid,
    /// SHA-256 (hex) of the secret half of `wkr_{id}.{secret}`. Never leaves the
    /// server — DTOs deliberately omit it.
    pub token_hash: String,
    pub nats_public_key: Option<String>,
    /// Self-reported executor backends (wire-names, e.g. `["python"]`). The set
    /// the scoped JWT's SUBSCRIBE grant is built from.
    pub backends: serde_json::Value,
    pub status: String,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub enrolled_by: Uuid,
    pub enrolled_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// One row from the `worker_registration_tokens` table. Column order matches the
/// migration.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WorkerRegistrationTokenRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    #[sqlx(rename = "worker_group")]
    pub group: Option<String>,
    /// SHA-256 (hex) of the secret half of `wt_{id}.{secret}`.
    pub token_hash: String,
    pub reusable: bool,
    pub uses: i32,
    pub max_uses: Option<i32>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

// ── Wire DTOs ──────────────────────────────────────────────────────────────

/// Compact list-row shape. Returned by `GET /api/v1/workers` — MUST NOT carry
/// `token_hash`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WorkerSummary {
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "group", skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The worker-group `capacity`-resource UUID this worker's `PartitionedPool`
    /// binds to (`executor-<wire>-grp.<prio>.<routing_partition>.>`). Same value
    /// the enroll response returns — surfaced on the list row so the Queue detail
    /// UI can show each worker's live partition without re-enrolling.
    pub routing_partition: Uuid,
    pub status: String,
    /// Advertised executor backends (the same `backends` JSON array the worker
    /// enrolled with). Included on the list row so the fleet UI can show the
    /// served-backend summary inline without an extra per-worker round-trip. `[]`
    /// when none.
    pub backends: serde_json::Value,
    /// Whether this worker is currently LIVE — i.e. an entry for its id is present
    /// in mekhan's in-memory [`crate::fleet::FleetLiveness`] snapshot (refreshed by
    /// the `worker.{id}.presence` NATS heartbeat, TTL-swept). This is the
    /// authoritative "is it up right now?" signal: it derives from the same
    /// presence stream the executor actually emits, and — unlike a persisted flag
    /// — can't go stale across a mekhan restart (an empty snapshot simply
    /// repopulates within one presence interval). `status` remains the lifecycle
    /// marker (`enrolled`/`revoked`), NOT liveness.
    pub online: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<DateTime<Utc>>,
    pub enrolled_at: DateTime<Utc>,
}

impl From<WorkerRow> for WorkerSummary {
    /// DB-only projection: `online` defaults to `false` and `last_seen_at` is the
    /// persisted column. The read handlers overlay the live
    /// [`crate::fleet::FleetLiveness`] snapshot to set `online` (and a fresher
    /// `last_seen_at`) for currently-connected workers.
    fn from(w: WorkerRow) -> Self {
        Self {
            id: w.id,
            name: w.name,
            group: w.group,
            routing_partition: w.routing_partition,
            status: w.status,
            backends: w.backends,
            online: false,
            last_seen_at: w.last_seen_at,
            enrolled_at: w.enrolled_at,
        }
    }
}

/// Admin view returned by `GET /api/v1/workers/{id}`. MUST NOT carry
/// `token_hash`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WorkerDetail {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    #[serde(rename = "group", skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The worker-group `capacity`-resource UUID this worker's `PartitionedPool`
    /// binds to (`executor-<wire>-grp.<prio>.<routing_partition>.>`).
    pub routing_partition: Uuid,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nats_public_key: Option<String>,
    pub backends: serde_json::Value,
    /// Live presence (see [`WorkerSummary::online`]): `true` when an entry for this
    /// worker is in the in-memory [`crate::fleet::FleetLiveness`] snapshot. The
    /// `get_worker` handler overlays the snapshot; `From<WorkerRow>` defaults it to
    /// `false`.
    pub online: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<DateTime<Utc>>,
    pub enrolled_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl From<WorkerRow> for WorkerDetail {
    /// DB-only projection (`online = false`, persisted `last_seen_at`); the read
    /// handler overlays the live [`crate::fleet::FleetLiveness`] snapshot.
    fn from(w: WorkerRow) -> Self {
        Self {
            id: w.id,
            workspace_id: w.workspace_id,
            name: w.name,
            group: w.group,
            routing_partition: w.routing_partition,
            status: w.status,
            nats_public_key: w.nats_public_key,
            backends: w.backends,
            online: false,
            last_seen_at: w.last_seen_at,
            enrolled_at: w.enrolled_at,
            revoked_at: w.revoked_at,
        }
    }
}

/// Request body for `POST /api/v1/workers/enroll`. Authenticated by the
/// `registration_token` in the body, not by the auth gate.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct EnrollWorkerRequest {
    /// `wt_{id}.{secret}` registration token.
    pub registration_token: String,
    /// Operator-facing worker name; must be unique within the workspace.
    pub name: String,
    /// Optional NATS account public key the worker will use.
    #[serde(default)]
    pub nats_public_key: Option<String>,
    /// Executor backends this worker serves (wire-names). The scoped JWT's
    /// SUBSCRIBE grant is built per advertised backend. Defaults to `[]`.
    #[serde(default)]
    pub backends: Vec<String>,
}

/// Response body for a successful enrollment. `worker_token` is the full
/// `wkr_{id}.{secret}` credential, returned ONCE and never stored in plaintext.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EnrolledWorker {
    pub id: Uuid,
    pub worker_token: String,
    pub workspace_id: Uuid,
    /// Human group alias, inherited from the registration token. Display only —
    /// `None` is rendered as the implicit `default` group.
    #[serde(rename = "group")]
    pub group: Option<String>,
    /// The ROUTING PARTITION the executor binds its grouped consumer to: the
    /// worker group's `capacity`-resource UUID (the alias resolved, or the
    /// workspace's seeded `default` group when the token names none). This — NOT
    /// `group` — is the token the executor partitions on
    /// (`executor-<wire>-grp.<prio>.<routing_partition>.>`).
    pub routing_partition: Uuid,
    /// A freshly-signed scoped NATS *user* JWT, minted from the
    /// `nats_public_key` the worker sent at enrollment. `null` when no public key
    /// was supplied OR signing was unavailable; the worker can fetch it later via
    /// `POST /api/v1/workers/{id}/nats-creds`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nats_jwt: Option<String>,
}

/// Response for `POST /api/v1/workers/{id}/nats-creds` — a freshly-minted scoped
/// NATS user JWT plus the issuing account signing key's public key. The worker
/// assembles its own `.creds` file from this JWT and its locally-held user nkey
/// seed.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WorkerNatsCreds {
    /// Freshly signed scoped user JWT, bound to the worker's stored
    /// `nats_public_key`.
    pub nats_jwt: String,
    /// The runners-account signing key's PUBLIC key (`A…`) — the JWT issuer and
    /// the value the NATS server's account resolver must trust. (Workers share
    /// the runner-account signer; see [`crate::runners_nats`].)
    pub account_public_key: String,
}

/// Request body for `POST /api/v1/workers/registration-tokens`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateWorkerRegistrationTokenRequest {
    #[serde(rename = "group", default)]
    pub group: Option<String>,
    /// Mint against the shared **platform** worker pool rather than the caller's
    /// workspace: the token's `workspace_id` is forced to `PLATFORM_SCOPE_ID` and
    /// it enrols workers into the global competing-consumer `default` group.
    /// Requires `is_platform_admin` (curation is a platform-admin capability).
    /// Defaults to `false` (a normal workspace-scoped token).
    #[serde(default)]
    pub platform: bool,
    /// Defaults to `true` (reusable) when omitted.
    #[serde(default)]
    pub reusable: Option<bool>,
    #[serde(default)]
    pub max_uses: Option<i32>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

/// Response for a freshly-minted worker registration token. `token` is the full
/// `wt_{id}.{secret}` credential, returned ONCE.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CreatedWorkerRegistrationToken {
    pub id: Uuid,
    pub token: String,
    #[serde(rename = "group")]
    pub group: Option<String>,
    pub reusable: bool,
    pub max_uses: Option<i32>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Compact list-row for worker registration tokens. MUST NOT carry `token_hash`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WorkerRegistrationTokenSummary {
    pub id: Uuid,
    #[serde(rename = "group", skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub reusable: bool,
    pub uses: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl From<WorkerRegistrationTokenRow> for WorkerRegistrationTokenSummary {
    fn from(w: WorkerRegistrationTokenRow) -> Self {
        Self {
            id: w.id,
            group: w.group,
            reusable: w.reusable,
            uses: w.uses,
            max_uses: w.max_uses,
            expires_at: w.expires_at,
            created_at: w.created_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::runner::{mint_token, parse_token, verify_secret};

    #[test]
    fn worker_token_round_trip() {
        let id = Uuid::new_v4();
        let minted = mint_token(WORKER_TOKEN_PREFIX, id);
        assert_eq!(minted.id, id);
        assert!(minted.full_token.starts_with("wkr_"));

        let (parsed_id, secret) =
            parse_token(WORKER_TOKEN_PREFIX, &minted.full_token).expect("parse should succeed");
        assert_eq!(parsed_id, id);
        assert!(verify_secret(&secret, &minted.token_hash));
    }

    #[test]
    fn worker_reg_token_round_trip() {
        let id = Uuid::new_v4();
        let minted = mint_token(WORKER_REG_TOKEN_PREFIX, id);
        assert!(minted.full_token.starts_with("wt_"));
        let (parsed_id, secret) =
            parse_token(WORKER_REG_TOKEN_PREFIX, &minted.full_token).expect("parse should succeed");
        assert_eq!(parsed_id, id);
        assert!(verify_secret(&secret, &minted.token_hash));
    }

    #[test]
    fn worker_parse_rejects_wrong_prefix() {
        // A runner credential must never parse as a worker token (and vice
        // versa): the prefixes disambiguate the two families.
        let id = Uuid::new_v4();
        let runner = mint_token(crate::models::runner::RUNNER_TOKEN_PREFIX, id);
        assert!(parse_token(WORKER_TOKEN_PREFIX, &runner.full_token).is_none());
        let worker = mint_token(WORKER_TOKEN_PREFIX, id);
        assert!(parse_token(
            crate::models::runner::RUNNER_TOKEN_PREFIX,
            &worker.full_token
        )
        .is_none());
    }
}
