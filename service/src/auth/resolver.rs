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
//!
//! ## What the resolver auto-provisions
//!
//! On every resolve the principal is granted membership **only** in
//! workspaces they demonstrably belong to:
//!   - each Zitadel org claim is mapped to its bound workspace
//!     (`workspaces.zitadel_org_id`) and the user is upserted there as
//!     `editor` — a principal can hold several org-workspaces at once;
//!   - optionally (flag-gated, see below) a `viewer` row in every system
//!     workspace.
//!
//! The resolver does **not** auto-join the shared `default` tenant and does
//! **not** bulk-import an org's other members. A principal with no resolvable
//! org binding and no pre-existing grant ends up with `workspace_id = None`
//! and handlers reject — workspaces stay isolated by default.
//!
//! `multi_org = true` only changes the *active-workspace pick* (choose among
//! the full membership set, honouring the `active_workspace` cookie); the
//! single-org default returns the one resolvable org-workspace directly.
//! Neither mode touches `dev_noop`: the dev-user's seeded `default`-as-owner
//! row pre-dates resolution and is honoured regardless.
//!
//! ## Flag-gated system-workspace auto-join (`auth.auto_join_system_workspaces`)
//!
//! Default `false`: principals are NOT enrolled into system workspaces
//! (`demos`). Seeded demos stay reachable via their `visibility = 'public'`
//! read path, so no membership row is required. Set the flag `true` to restore
//! the legacy behaviour where every login mints a `viewer` row in `demos` (and
//! it appears in the picker as a real membership). Off by default because
//! those rows defeat isolation — every user surfaces as a `viewer` anywhere
//! `demos` content appears (e.g. the cross-workspace ShareDialog floor).

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

/// Extract the full **set** of Zitadel org ids referenced anywhere in the
/// roles claim. A principal granted roles in several orgs (the multi-org
/// case) shows up here as multiple ids; the single-org case yields one (or
/// zero). Order is deterministic-ish (BTreeMap iteration) but callers should
/// not depend on it for tenant selection — that's the membership-preference
/// query's job.
fn org_ids_from_claims(claims: &VerifiedClaims) -> Vec<String> {
    let Some(Value::Object(roles_obj)) = claims.extra.get(ZITADEL_ROLES_CLAIM) else {
        return Vec::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for orgs in roles_obj.values().filter_map(|v| v.as_object()) {
        for org_id in orgs.keys() {
            if seen.insert(org_id.clone()) {
                out.push(org_id.clone());
            }
        }
    }
    out
}

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
        let display_name =
            string_claim(&claims, "name").or_else(|| string_claim(&claims, "preferred_username"));
        // Standard OIDC `picture` claim — a URL to the user's profile photo.
        // Mirrored into `user_profiles.avatar_url` by the extractor upsert.
        let avatar_url = string_claim(&claims, "picture");

        let roles: Vec<String> = match claims.extra.get(ZITADEL_ROLES_CLAIM) {
            Some(Value::Object(roles_obj)) => roles_obj.keys().cloned().collect(),
            _ => Vec::new(),
        };
        // `org_id` is metadata only (the authoritative tenant is
        // `workspace_id`). Keep the legacy "first org id" semantics here; the
        // multi-org path in `DbPrincipalResolver` reads the full set via
        // `org_ids_from_claims`.
        let org_id = org_ids_from_claims(&claims).into_iter().next();

        Ok(AuthUser {
            subject: claims.subject,
            email,
            display_name,
            roles,
            org_id,
            workspace_id: None,
            workspace_role: None,
            avatar_url,
        })
    }
}

fn string_claim(claims: &VerifiedClaims, key: &str) -> Option<String> {
    claims
        .extra
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
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
    /// Mirrors `AuthConfig.multi_org`. `false` (default) returns the single
    /// resolvable org-workspace directly; `true` picks among the full
    /// membership set — see [`Self::resolve`].
    multi_org: bool,
    /// Mirrors `AuthConfig.auto_join_system_workspaces`. `false` (default)
    /// leaves system workspaces (`demos`) un-joined; `true` mints a `viewer`
    /// row in each on every resolve.
    auto_join_system_workspaces: bool,
}

impl DbPrincipalResolver {
    /// Construct with the isolation-preserving defaults (single-org, no
    /// system-workspace auto-join). Zero-config constructor for call sites /
    /// tests that don't exercise tenancy flags.
    pub fn new(db: PgPool) -> Self {
        Self::with_policy(db, false, false)
    }

    /// Construct with both tenancy flags wired from `AuthConfig`. The
    /// composition root (`main.rs`) uses this; pass `config.auth.multi_org`
    /// and `config.auth.auto_join_system_workspaces`.
    pub fn with_policy(db: PgPool, multi_org: bool, auto_join_system_workspaces: bool) -> Self {
        Self {
            inner: StaticPrincipalResolver,
            db,
            multi_org,
            auto_join_system_workspaces,
        }
    }
}

#[async_trait]
impl PrincipalResolver for DbPrincipalResolver {
    async fn resolve(&self, claims: VerifiedClaims) -> Result<AuthUser, AuthError> {
        // Extract the full org-id set BEFORE delegating (the inner resolver
        // consumes `claims` and only keeps the first org id as metadata).
        let org_ids = org_ids_from_claims(&claims);

        let mut user = self.inner.resolve(claims).await?;
        let user_id = user.subject_as_uuid();

        // Auto-membership in every system workspace (currently just `demos`),
        // gated behind `auth.auto_join_system_workspaces` (default OFF). When
        // ON, every authenticated principal gets a read-only `viewer` row so
        // demos appears in their picker as a real membership. OFF by default:
        // seeded demos stay reachable via `visibility = 'public'` without a
        // membership row, keeping workspaces isolated (no user surfaces as a
        // `viewer` everywhere demos content appears).
        if self.auto_join_system_workspaces {
            ensure_system_workspace_membership(&self.db, user_id).await?;
        }

        // Resolve every org claim to its bound workspace and auto-provision
        // membership. A multi-org principal lands in several workspaces here;
        // a single-org one in (at most) one. `primary_org_workspace` keeps the
        // first resolvable one for the single-org fast path below.
        let mut primary_org_workspace: Option<Uuid> = None;
        for zitadel_org_id in &org_ids {
            if let Some(ws_id) = lookup_workspace_by_zitadel_org(&self.db, zitadel_org_id).await? {
                upsert_member(&self.db, ws_id, user_id, DEFAULT_AUTOPROVISION_ROLE).await?;
                // First resolvable org id seeds the "primary" pick — mirrors
                // the legacy `user.org_id` (which the static resolver set to
                // the first org). The membership-preference query below makes
                // the real choice in multi-org mode; this is only the
                // single-org fast path.
                if primary_org_workspace.is_none() {
                    primary_org_workspace = Some(ws_id);
                }
            }
        }

        // NOTE: the resolver deliberately does NOT auto-join the shared
        // `default` workspace. Earlier builds enrolled every authenticated
        // principal there as `editor`, which broke isolation — every Zitadel
        // user became a member of the shared tenant on first login. A
        // principal now holds membership only where they genuinely belong (an
        // org-bound workspace above, an explicit grant, or a seeded row like
        // dev_noop's `default`-as-owner). Users with neither get
        // `workspace_id = None` and handlers reject.

        // Single-org fast path (multi_org OFF): if exactly the legacy behaviour
        // applies — one resolvable org — return that workspace directly,
        // preserving the prior `Path 1` semantics and its role re-read.
        if !self.multi_org {
            if let Some(ws_id) = primary_org_workspace {
                user.workspace_id = Some(ws_id);
                // Re-read rather than trust DEFAULT_AUTOPROVISION_ROLE: the
                // upsert is `DO NOTHING`, so an existing member keeps whatever
                // role admins assigned, not `editor`.
                user.workspace_role = lookup_role(&self.db, ws_id, user_id).await?;
                return Ok(user);
            }
        }

        // Active-workspace pick (covers BOTH modes' fall-through):
        //   - multi_org ON: choose among ALL memberships (org-workspaces +
        //     any explicit grants); the per-session cookie override applied
        //     downstream in `active_workspace::apply_override` can swap to any
        //     other membership.
        //   - multi_org OFF with no resolvable org: the legacy `Path 2` —
        //     prefer `default`, then non-system, then by age.
        // In every case `membership_workspace` returns `None` when the user
        // holds no membership at all (the multi_org "no org binding, no grant"
        // case), leaving `workspace_id = None` — handlers reject rather than
        // grant ambient access.
        user.workspace_id = membership_workspace(&self.db, user_id).await?;
        if let Some(ws_id) = user.workspace_id {
            user.workspace_role = lookup_role(&self.db, ws_id, user_id).await?;
        }
        Ok(user)
    }
}

/// Idempotently upsert the caller as a viewer in every `is_system = TRUE`
/// workspace. Today that's just `demos`, but the loop is correct for any
/// future system workspace (e.g. a `samples` or `tutorial` namespace). Only
/// invoked when `auth.auto_join_system_workspaces` is set.
async fn ensure_system_workspace_membership(db: &PgPool, user_id: Uuid) -> Result<(), AuthError> {
    let rows: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM workspaces WHERE is_system = TRUE")
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
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM workspaces WHERE zitadel_org_id = $1")
            .bind(zitadel_org_id)
            .fetch_optional(db)
            .await
            .map_err(|e| AuthError::Internal(format!("workspace lookup: {e}")))?;
    Ok(row.map(|(id,)| id))
}

async fn upsert_member(
    db: &PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
    role: &str,
) -> Result<(), AuthError> {
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

/// Fetch the caller's `role` in a specific workspace, if any. Drives
/// `AuthUser.workspace_role` so the SPA can gate admin-only affordances
/// (server still enforces via `require_role`).
async fn lookup_role(
    db: &PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
) -> Result<Option<String>, AuthError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT role FROM workspace_members WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(workspace_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .map_err(|e| AuthError::Internal(format!("workspace role lookup: {e}")))?;
    Ok(row.map(|(r,)| r))
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
          WHERE m.user_id = $1 AND w.archived_at IS NULL \
          ORDER BY (w.slug = 'default') DESC, w.is_system ASC, w.created_at ASC \
          LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await
    .map_err(|e| AuthError::Internal(format!("workspace membership lookup: {e}")))?;
    Ok(row.map(|(id,)| id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn claims_with(extra: BTreeMap<String, Value>) -> VerifiedClaims {
        VerifiedClaims {
            subject: "alice".into(),
            issuer: "https://idp".into(),
            audience: vec!["mekhan".into()],
            expires_at: 0,
            extra,
        }
    }

    #[tokio::test]
    async fn extracts_picture_claim_into_avatar_url() {
        let mut extra = BTreeMap::new();
        extra.insert("picture".into(), Value::String("https://idp/a.png".into()));
        let user = StaticPrincipalResolver
            .resolve(claims_with(extra))
            .await
            .unwrap();
        assert_eq!(user.avatar_url.as_deref(), Some("https://idp/a.png"));
    }

    #[tokio::test]
    async fn no_picture_claim_yields_none_avatar() {
        let user = StaticPrincipalResolver
            .resolve(claims_with(BTreeMap::new()))
            .await
            .unwrap();
        assert_eq!(user.avatar_url, None);
    }

    /// Build a Zitadel roles claim: `{ role: { org_id: "domain" } }`, with
    /// every `(role, org_id)` pair from the input mapped in.
    fn roles_claim(pairs: &[(&str, &str)]) -> Value {
        let mut roles = serde_json::Map::new();
        for (role, org) in pairs {
            let entry = roles
                .entry((*role).to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Value::Object(orgs) = entry {
                orgs.insert((*org).to_string(), Value::String(format!("{org}.example")));
            }
        }
        Value::Object(roles)
    }

    #[test]
    fn org_ids_empty_when_no_roles_claim() {
        assert!(org_ids_from_claims(&claims_with(BTreeMap::new())).is_empty());
    }

    #[test]
    fn org_ids_single_org() {
        let mut extra = BTreeMap::new();
        extra.insert(
            ZITADEL_ROLES_CLAIM.into(),
            roles_claim(&[("editor", "org-a"), ("viewer", "org-a")]),
        );
        assert_eq!(org_ids_from_claims(&claims_with(extra)), vec!["org-a"]);
    }

    #[test]
    fn org_ids_multi_org_deduped() {
        // Roles spread across two orgs → both ids, each once, deterministic order.
        let mut extra = BTreeMap::new();
        extra.insert(
            ZITADEL_ROLES_CLAIM.into(),
            roles_claim(&[("editor", "org-a"), ("admin", "org-b"), ("viewer", "org-b")]),
        );
        let ids = org_ids_from_claims(&claims_with(extra));
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"org-a".to_string()));
        assert!(ids.contains(&"org-b".to_string()));
    }

    #[tokio::test]
    async fn static_resolver_keeps_first_org_as_metadata() {
        let mut extra = BTreeMap::new();
        extra.insert(
            ZITADEL_ROLES_CLAIM.into(),
            roles_claim(&[("editor", "org-a"), ("admin", "org-b")]),
        );
        let user = StaticPrincipalResolver
            .resolve(claims_with(extra))
            .await
            .unwrap();
        // `org_id` is the first of the sorted set — metadata only.
        assert!(user.org_id.is_some());
        assert!(user.roles.contains(&"editor".to_string()));
    }
}
