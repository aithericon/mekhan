//! Lab Runner Fleet — DB row structs, wire DTOs, and the mekhan-native
//! runner-token helpers (Phase 1).
//!
//! Two token families, each `<prefix>_{uuid}.{secret}`:
//!   - `rnr_{runner_id}.{secret}`   — runner control-plane credential.
//!   - `rt_{regtoken_id}.{secret}`  — GitLab-style registration token.
//!
//! The uuid prefix gives an O(1) row lookup; the secret half is verified by a
//! constant-time compare of `sha256(secret)` against the stored `token_hash`.
//! Only the hash is ever persisted — the plaintext is returned once at mint.
//! This is NOT a Zitadel PAT: it works fully offline in `dev_noop`.
//!
//! These structs mirror the migration column order (see
//! `service/migrations/20240134000000_runners.sql`) so a `SELECT *` reads back
//! via `sqlx::FromRow` without surprises.

use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

// ── Token prefixes ─────────────────────────────────────────────────────────

/// Prefix of the runner control-plane credential.
pub const RUNNER_TOKEN_PREFIX: &str = "rnr_";
/// Prefix of the registration token.
pub const REG_TOKEN_PREFIX: &str = "rt_";

// ── DB rows ────────────────────────────────────────────────────────────────

/// One row from the `runners` table. Column order matches the migration.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RunnerRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    #[sqlx(rename = "runner_group")]
    pub group: Option<String>,
    /// SHA-256 (hex) of the secret half of `rnr_{id}.{secret}`. Never leaves
    /// the server — DTOs deliberately omit it.
    pub token_hash: String,
    pub nats_public_key: Option<String>,
    pub capabilities: serde_json::Value,
    pub status: String,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub enrolled_by: Uuid,
    pub enrolled_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// One row from the `runner_registration_tokens` table. Column order matches
/// the migration.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RunnerRegistrationTokenRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    #[sqlx(rename = "runner_group")]
    pub group: Option<String>,
    /// SHA-256 (hex) of the secret half of `rt_{id}.{secret}`.
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

/// Compact list-row shape. Returned by `GET /api/v1/runners` — MUST NOT carry
/// `token_hash`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RunnerSummary {
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "group", skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub status: String,
    /// Advertised capabilities (the same `capabilities` JSON object the runner
    /// enrolled with). Included on the list row so the fleet UI can show a caps
    /// summary inline without an extra per-runner round-trip. `{}` when none.
    pub capabilities: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<DateTime<Utc>>,
    pub enrolled_at: DateTime<Utc>,
}

impl From<RunnerRow> for RunnerSummary {
    fn from(r: RunnerRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            group: r.group,
            status: r.status,
            capabilities: r.capabilities,
            last_seen_at: r.last_seen_at,
            enrolled_at: r.enrolled_at,
        }
    }
}

/// Admin view returned by `GET /api/v1/runners/{id}`. MUST NOT carry
/// `token_hash`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RunnerDetail {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    #[serde(rename = "group", skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nats_public_key: Option<String>,
    pub capabilities: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<DateTime<Utc>>,
    pub enrolled_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl From<RunnerRow> for RunnerDetail {
    fn from(r: RunnerRow) -> Self {
        Self {
            id: r.id,
            workspace_id: r.workspace_id,
            name: r.name,
            group: r.group,
            status: r.status,
            nats_public_key: r.nats_public_key,
            capabilities: r.capabilities,
            last_seen_at: r.last_seen_at,
            enrolled_at: r.enrolled_at,
            revoked_at: r.revoked_at,
        }
    }
}

/// Request body for `POST /api/v1/runners/enroll`. Authenticated by the
/// `registration_token` in the body, not by the auth gate.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct EnrollRequest {
    /// `rt_{id}.{secret}` registration token.
    pub registration_token: String,
    /// Operator-facing runner name; must be unique within the workspace.
    pub name: String,
    /// Optional NATS account public key the runner will use.
    #[serde(default)]
    pub nats_public_key: Option<String>,
    /// Arbitrary self-reported capability blob. Defaults to `{}`.
    #[serde(default = "empty_object")]
    pub capabilities: serde_json::Value,
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Response body for a successful enrollment. `runner_token` is the full
/// `rnr_{id}.{secret}` credential, returned ONCE and never stored in plaintext.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EnrolledRunner {
    pub id: Uuid,
    pub runner_token: String,
    pub workspace_id: Uuid,
    #[serde(rename = "group")]
    pub group: Option<String>,
    /// Phase 2 — a freshly-signed scoped NATS *user* JWT, minted from the
    /// `nats_public_key` the runner sent at enrollment. `null` when no public
    /// key was supplied OR signing was unavailable; the runner can fetch it
    /// later via `POST /api/v1/runners/{id}/nats-creds`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nats_jwt: Option<String>,
}

/// Response for `POST /api/v1/runners/{id}/nats-creds` — a freshly-minted
/// scoped NATS user JWT plus the issuing account signing key's public key. The
/// runner assembles its own `.creds` file from this JWT and its locally-held
/// user nkey seed.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RunnerNatsCreds {
    /// Freshly signed scoped user JWT, bound to the runner's stored
    /// `nats_public_key`.
    pub nats_jwt: String,
    /// The runners-account signing key's PUBLIC key (`A…`) — the JWT issuer and
    /// the value the NATS server's account resolver must trust.
    pub account_public_key: String,
}

/// Request body for `POST /api/v1/runners/registration-tokens`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateRegistrationTokenRequest {
    #[serde(rename = "group", default)]
    pub group: Option<String>,
    /// Defaults to `true` (reusable) when omitted.
    #[serde(default)]
    pub reusable: Option<bool>,
    #[serde(default)]
    pub max_uses: Option<i32>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

/// Response for a freshly-minted registration token. `token` is the full
/// `rt_{id}.{secret}` credential, returned ONCE.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CreatedRegistrationToken {
    pub id: Uuid,
    pub token: String,
    #[serde(rename = "group")]
    pub group: Option<String>,
    pub reusable: bool,
    pub max_uses: Option<i32>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Compact list-row for registration tokens. MUST NOT carry `token_hash`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RegistrationTokenSummary {
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

/// Phase 5 — one row of the live in-memory presence snapshot returned by
/// `GET /api/v1/runners/presence`. This reflects the presence-controller's
/// in-memory `PresenceMap` (the actual pool-capacity signal), NOT the
/// `runners.last_seen_at` column on [`RunnerSummary`] (a best-effort UI bump).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RunnerPresenceSnapshot {
    pub runner_id: Uuid,
    /// Whether mekhan currently considers the runner PRESENT (one pool unit
    /// admitted and not yet reaped).
    pub present: bool,
    /// Milliseconds since the last presence heartbeat from this runner.
    pub last_seen_ms_ago: u64,
    /// The runner's self-reported executor `backends` (wire-names, e.g.
    /// `["python"]`) — the set-membership dimension it advertises in its
    /// presence heartbeat, ORTHOGONAL to its typed `capabilities`. Surfaced for
    /// fleet visibility so an operator can see which execution backends a live
    /// runner actually serves (a presence-pool step on an uncovered backend will
    /// queue until a covering runner checks in).
    #[serde(default)]
    pub backends: Vec<String>,
}

impl From<RunnerRegistrationTokenRow> for RegistrationTokenSummary {
    fn from(r: RunnerRegistrationTokenRow) -> Self {
        Self {
            id: r.id,
            group: r.group,
            reusable: r.reusable,
            uses: r.uses,
            max_uses: r.max_uses,
            expires_at: r.expires_at,
            created_at: r.created_at,
        }
    }
}

// ── Token mint / parse / verify ────────────────────────────────────────────

/// A freshly minted token: the row id, the full plaintext credential
/// (`<prefix>_{id}.{secret}`, surfaced ONCE), and the SHA-256 hex of the
/// secret half (what gets stored in `token_hash`).
pub struct MintedToken {
    pub id: Uuid,
    pub full_token: String,
    pub token_hash: String,
}

/// SHA-256 → lowercase hex.
pub fn sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Generate a 256-bit secret, base64url (no padding) encoded.
fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Mint a token for a freshly-allocated row id: returns the full plaintext
/// credential and the hash to persist.
pub fn mint_token(prefix: &str, id: Uuid) -> MintedToken {
    let secret = random_secret();
    let full_token = format!("{prefix}{id}.{secret}");
    let token_hash = sha256_hex(&secret);
    MintedToken {
        id,
        full_token,
        token_hash,
    }
}

/// Parse a `<prefix>_{uuid}.{secret}` token into its `(uuid, secret)` parts.
/// Returns `None` on any structural mismatch (wrong prefix, missing dot,
/// unparseable uuid).
pub fn parse_token(prefix: &str, token: &str) -> Option<(Uuid, String)> {
    let rest = token.strip_prefix(prefix)?;
    let (id_part, secret) = rest.split_once('.')?;
    if secret.is_empty() {
        return None;
    }
    let id = Uuid::parse_str(id_part).ok()?;
    Some((id, secret.to_string()))
}

/// Constant-time equality over two byte slices. Returns `false` immediately
/// for length mismatch (the hashes we compare are fixed-length hex of the same
/// algorithm, so a length difference only ever means "different input"), then
/// compares every remaining byte without short-circuiting.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Verify a presented `secret` against a stored `token_hash` in constant time
/// over the (fixed-length) hash outputs.
pub fn verify_secret(secret: &str, token_hash: &str) -> bool {
    let computed = sha256_hex(secret);
    constant_time_eq(computed.as_bytes(), token_hash.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_parse_verify_round_trip() {
        let id = Uuid::new_v4();
        let minted = mint_token(RUNNER_TOKEN_PREFIX, id);
        assert_eq!(minted.id, id);
        assert!(minted.full_token.starts_with("rnr_"));

        let (parsed_id, secret) =
            parse_token(RUNNER_TOKEN_PREFIX, &minted.full_token).expect("parse should succeed");
        assert_eq!(parsed_id, id);
        assert!(verify_secret(&secret, &minted.token_hash));
    }

    #[test]
    fn reg_token_round_trip() {
        let id = Uuid::new_v4();
        let minted = mint_token(REG_TOKEN_PREFIX, id);
        assert!(minted.full_token.starts_with("rt_"));
        let (parsed_id, secret) =
            parse_token(REG_TOKEN_PREFIX, &minted.full_token).expect("parse should succeed");
        assert_eq!(parsed_id, id);
        assert!(verify_secret(&secret, &minted.token_hash));
    }

    #[test]
    fn verify_rejects_tampered_secret() {
        let id = Uuid::new_v4();
        let minted = mint_token(RUNNER_TOKEN_PREFIX, id);
        let (_, secret) = parse_token(RUNNER_TOKEN_PREFIX, &minted.full_token).unwrap();
        let tampered = format!("{secret}x");
        assert!(!verify_secret(&tampered, &minted.token_hash));
    }

    #[test]
    fn verify_rejects_wrong_id_token_hash() {
        // Two independently minted tokens: secret of one must never verify
        // against the hash of the other (cross-row replay guard).
        let a = mint_token(RUNNER_TOKEN_PREFIX, Uuid::new_v4());
        let b = mint_token(RUNNER_TOKEN_PREFIX, Uuid::new_v4());
        let (_, secret_a) = parse_token(RUNNER_TOKEN_PREFIX, &a.full_token).unwrap();
        assert!(!verify_secret(&secret_a, &b.token_hash));
    }

    #[test]
    fn parse_rejects_malformed() {
        assert!(parse_token(RUNNER_TOKEN_PREFIX, "garbage").is_none());
        assert!(parse_token(RUNNER_TOKEN_PREFIX, "rnr_not-a-uuid.secret").is_none());
        assert!(parse_token(RUNNER_TOKEN_PREFIX, "rt_wrong-prefix.secret").is_none());
        let id = Uuid::new_v4();
        // Missing secret half.
        assert!(parse_token(RUNNER_TOKEN_PREFIX, &format!("rnr_{id}.")).is_none());
        assert!(parse_token(RUNNER_TOKEN_PREFIX, &format!("rnr_{id}")).is_none());
    }
}
