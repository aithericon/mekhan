//! Config-seeded PLATFORM registration tokens — declarative, headless machine
//! enrollment without an interactive mint.
//!
//! The platform-tier rework moved the shared `default` (worker) and
//! `model_serving` (runner) groups out of every workspace into the global
//! PLATFORM scope. Minting a registration token for those groups is a
//! platform-admin capability, which is awkward for automated provisioning (a CI
//! pipeline has no interactive session). Instead, an operator generates a token
//! value ONCE (Terraform `<prefix><uuid>.<secret>`), stores it in Vault, and
//! points BOTH this config and the machine's `*_REG_TOKEN` env at it. On
//! startup the seeder upserts a reusable, platform-scoped registration token
//! whose hash matches the secret — so the machine self-enrolls with the same
//! value, no mint, no human.
//!
//! Idempotent + rotation-safe: re-seeding the same token is a no-op (upsert by
//! the token's own id); seeding a NEW token for a class revokes the prior
//! bootstrap-seeded token of that class (so a rotated secret invalidates the
//! old one). Operator-minted tokens are untouched — only rows authored by the
//! bootstrap sentinel are revoked.

use uuid::Uuid;

use crate::models::asset::PLATFORM_SCOPE_ID;
use crate::models::runner::{parse_token, sha256_hex};
use crate::AppState;

/// Stable sentinel `created_by` for bootstrap-seeded registration tokens. Lets a
/// rotation revoke the prior bootstrap token of a class without touching
/// operator-minted ones. Not a real user (`b007` = "boot").
const BOOTSTRAP_SEEDER_AUTHOR_ID: Uuid = uuid::uuid!("00000000-0000-0000-0000-00000b007b07");

/// Seed (or rotate) the platform `default` WORKER-group bootstrap registration
/// token from `MEKHAN__BOOTSTRAP__WORKER_REGISTRATION_TOKEN`. No-op when unset.
pub async fn ensure_bootstrap_worker_token(state: &AppState) -> Result<(), String> {
    let Some(token) = state.config.bootstrap.worker_registration_token.as_deref() else {
        return Ok(());
    };
    seed(
        state,
        "worker_registration_tokens",
        "worker_group",
        crate::models::worker::WORKER_REG_TOKEN_PREFIX,
        crate::worker_groups::DEFAULT_WORKER_GROUP_PATH,
        token,
    )
    .await
}

/// Seed (or rotate) the platform `model_serving` RUNNER-group bootstrap
/// registration token from `MEKHAN__BOOTSTRAP__RUNNER_REGISTRATION_TOKEN`.
/// No-op when unset.
pub async fn ensure_bootstrap_runner_token(state: &AppState) -> Result<(), String> {
    let Some(token) = state.config.bootstrap.runner_registration_token.as_deref() else {
        return Ok(());
    };
    seed(
        state,
        "runner_registration_tokens",
        "runner_group",
        crate::models::runner::REG_TOKEN_PREFIX,
        crate::model_serving_group::MODEL_SERVING_GROUP_PATH,
        token,
    )
    .await
}

/// Shared upsert. `table` / `group_col` are compile-time literals (never user
/// input), so the formatted SQL is safe. Both registration-token tables share
/// the same column shape (only the group column name differs).
async fn seed(
    state: &AppState,
    table: &str,
    group_col: &str,
    prefix: &str,
    group_path: &str,
    full_token: &str,
) -> Result<(), String> {
    let (id, secret) = parse_token(prefix, full_token).ok_or_else(|| {
        format!("malformed bootstrap registration token (expected `{prefix}<uuid>.<secret>`)")
    })?;
    let token_hash = sha256_hex(&secret);

    let mut tx = state.db.begin().await.map_err(|e| e.to_string())?;

    // Rotation hygiene: revoke any OTHER live bootstrap-seeded token of this
    // class (same group, our sentinel author) so a rotated secret invalidates
    // the old token. Operator-minted tokens (different created_by) are untouched.
    sqlx::query(&format!(
        "UPDATE {table} SET revoked_at = NOW() \
          WHERE {group_col} = $1 AND created_by = $2 AND id <> $3 AND revoked_at IS NULL"
    ))
    .bind(group_path)
    .bind(BOOTSTRAP_SEEDER_AUTHOR_ID)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    // Upsert the bootstrap token itself (reusable, platform-scoped). Re-seeding
    // the same token refreshes the hash + un-revokes — so a value that was
    // rotated back is usable again.
    sqlx::query(&format!(
        "INSERT INTO {table} (id, workspace_id, {group_col}, token_hash, reusable, created_by) \
         VALUES ($1, $2, $3, $4, TRUE, $5) \
         ON CONFLICT (id) DO UPDATE \
            SET token_hash = EXCLUDED.token_hash, {group_col} = EXCLUDED.{group_col}, \
                reusable = TRUE, revoked_at = NULL"
    ))
    .bind(id)
    .bind(PLATFORM_SCOPE_ID)
    .bind(group_path)
    .bind(&token_hash)
    .bind(BOOTSTRAP_SEEDER_AUTHOR_ID)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    tx.commit().await.map_err(|e| e.to_string())?;
    tracing::info!(
        token_id = %id,
        group = %group_path,
        "seeded platform bootstrap registration token"
    );
    Ok(())
}
