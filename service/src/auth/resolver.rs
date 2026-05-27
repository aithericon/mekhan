//! `StaticPrincipalResolver` — maps verified JWT claims onto Mekhan's
//! `AuthUser`. Reads the Zitadel-specific roles claim layout, so this is the
//! one place provider-specific identifiers live outside the adapter itself.
//!
//! `DbPrincipalResolver` wraps the static one with a database lookup that
//! resolves `workspace_id` from the upstream `org_id` claim (matching
//! `workspaces.zitadel_org_id`) or falls back to the seeded default
//! workspace when the user is a member there. Membership is auto-provisioned
//! on the matching-org path so first login from a known Zitadel org grants
//! workspace access without an explicit admin step.

use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::model::{AuthError, AuthUser, VerifiedClaims};
use super::port::PrincipalResolver;

/// Zitadel emits roles under this claim. Value is a nested object:
/// `{ "<role>": { "<org_id>": "<org_domain>" } }`. We flatten to the set of
/// role names and adopt the first org_id we encounter as the user's org.
const ZITADEL_ROLES_CLAIM: &str = "urn:zitadel:iam:org:project:roles";

/// Default `workspace_members` role when auto-provisioning a Zitadel-bound
/// user. Conservative on first contact; admins can promote later via the
/// workspace members admin endpoint.
const DEFAULT_AUTOPROVISION_ROLE: &str = "editor";

#[derive(Debug, Clone, Default)]
pub struct StaticPrincipalResolver;

#[async_trait]
impl PrincipalResolver for StaticPrincipalResolver {
    async fn resolve(&self, claims: VerifiedClaims) -> Result<AuthUser, AuthError> {
        let email = string_claim(&claims, "email");
        let display_name = string_claim(&claims, "name").or_else(|| string_claim(&claims, "preferred_username"));

        let (roles, org_id) = match claims.extra.get(ZITADEL_ROLES_CLAIM) {
            Some(Value::Object(roles_obj)) => {
                let roles: Vec<String> = roles_obj.keys().cloned().collect();
                let org_id = roles_obj
                    .values()
                    .filter_map(|orgs| orgs.as_object())
                    .flat_map(|m| m.keys().cloned())
                    .next();
                (roles, org_id)
            }
            _ => (Vec::new(), None),
        };

        Ok(AuthUser {
            subject: claims.subject,
            email,
            display_name,
            roles,
            org_id,
            workspace_id: None,
        })
    }
}

fn string_claim(claims: &VerifiedClaims, key: &str) -> Option<String> {
    claims.extra.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

/// Resolver that enriches the `StaticPrincipalResolver` output with a
/// `workspace_id` looked up from the `workspaces` + `workspace_members`
/// tables. Construct via [`DbPrincipalResolver::new`] and wire into the
/// `Arc<dyn PrincipalResolver>` slot in place of the bare static resolver
/// whenever a `PgPool` is available (i.e. the production composition root).
#[derive(Debug, Clone)]
pub struct DbPrincipalResolver {
    inner: StaticPrincipalResolver,
    db: PgPool,
}

impl DbPrincipalResolver {
    pub fn new(db: PgPool) -> Self {
        Self { inner: StaticPrincipalResolver, db }
    }
}

#[async_trait]
impl PrincipalResolver for DbPrincipalResolver {
    async fn resolve(&self, claims: VerifiedClaims) -> Result<AuthUser, AuthError> {
        let mut user = self.inner.resolve(claims).await?;
        let user_id = user.subject_as_uuid();

        // Auto-membership in every system workspace (currently just `demos`).
        // The platform wants every authenticated principal to *be* a member
        // of demos — not merely to see demo templates via `visibility='public'`
        // — so the demos workspace appears in their workspace picker and
        // project listings without an admin step. Viewer role: read-only.
        ensure_system_workspace_membership(&self.db, user_id).await?;

        // Path 1: known Zitadel org → look up the bound workspace, auto-
        // provision membership idempotently, return that workspace.
        if let Some(ref zitadel_org_id) = user.org_id {
            if let Some(ws_id) = lookup_workspace_by_zitadel_org(&self.db, zitadel_org_id).await? {
                upsert_member(&self.db, ws_id, user_id, DEFAULT_AUTOPROVISION_ROLE).await?;
                user.workspace_id = Some(ws_id);
                return Ok(user);
            }
        }

        // Path 2: no org claim or no binding — fall back to the default
        // workspace, but only if the user is already a member there
        // (dev-noop user is seeded as such; arbitrary Zitadel principals
        // are not). Prefer a non-system workspace so the picker doesn't
        // land on `demos` for users who *also* belong to a real tenant.
        user.workspace_id = membership_workspace(&self.db, user_id).await?;
        Ok(user)
    }
}

/// Idempotently upsert the caller as a viewer in every `is_system = TRUE`
/// workspace. Today that's just `demos`, but the loop is correct for any
/// future system workspace (e.g. a `samples` or `tutorial` namespace).
async fn ensure_system_workspace_membership(db: &PgPool, user_id: Uuid) -> Result<(), AuthError> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM workspaces WHERE is_system = TRUE",
    )
    .fetch_all(db)
    .await
    .map_err(|e| AuthError::Internal(format!("system workspace lookup: {e}")))?;
    for (ws_id,) in rows {
        upsert_member(db, ws_id, user_id, "viewer").await?;
    }
    Ok(())
}

async fn lookup_workspace_by_zitadel_org(
    db: &PgPool,
    zitadel_org_id: &str,
) -> Result<Option<Uuid>, AuthError> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM workspaces WHERE zitadel_org_id = $1")
        .bind(zitadel_org_id)
        .fetch_optional(db)
        .await
        .map_err(|e| AuthError::Internal(format!("workspace lookup: {e}")))?;
    Ok(row.map(|(id,)| id))
}

async fn upsert_member(db: &PgPool, workspace_id: Uuid, user_id: Uuid, role: &str) -> Result<(), AuthError> {
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (workspace_id, user_id) DO NOTHING",
    )
    .bind(workspace_id)
    .bind(user_id)
    .bind(role)
    .execute(db)
    .await
    .map_err(|e| AuthError::Internal(format!("workspace membership upsert: {e}")))?;
    Ok(())
}

/// Returns the user's default "active" workspace. Preference order:
///   1. `slug='default'` (the seeded tenant for dev-noop + unbound principals)
///   2. any non-system workspace (real tenants outrank `demos`)
///   3. the oldest system workspace (worst case: user is only in `demos`)
///
/// The picker in Phase B exposes the full membership list and lets the
/// user override this default per session via a cookie.
async fn membership_workspace(db: &PgPool, user_id: Uuid) -> Result<Option<Uuid>, AuthError> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT w.id \
           FROM workspaces w \
           JOIN workspace_members m ON m.workspace_id = w.id \
          WHERE m.user_id = $1 \
          ORDER BY (w.slug = 'default') DESC, w.is_system ASC, w.created_at ASC \
          LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await
    .map_err(|e| AuthError::Internal(format!("workspace membership lookup: {e}")))?;
    Ok(row.map(|(id,)| id))
}
