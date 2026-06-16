//! `StaticPrincipalResolver` — maps verified JWT claims onto Mekhan's
//! `AuthUser`. Reads the Zitadel-specific roles claim layout, so this is the
//! one place provider-specific identifiers live outside the adapter itself.
//!
//! `DbPrincipalResolver` wraps the static one with a database lookup that
//! resolves `workspace_id` from the upstream `org_id` claim (matching
//! `workspaces.zitadel_org_id`). When no org workspace resolves, the principal
//! is lazily given a **personal** workspace (owner) on first login — see
//! [`DbPrincipalResolver::ensure_personal_workspace`]. Membership is auto-
//! provisioned on the matching-org path so first login from a known Zitadel org
//! grants workspace access without an explicit admin step.
//!
//! ## No more shared-`default` enrolment
//!
//! Earlier revisions auto-joined every principal into the seeded `default`
//! (nil) workspace and into every system workspace (`demos`). Both are gone:
//! migration `20240189` demoted `default` to `is_system = TRUE` (internals /
//! legacy only), and demos stay discoverable through `visibility = 'public'`
//! rather than an auto-membership row. A fresh principal therefore lands in a
//! private personal workspace, not a shared catch-all tenant.
//!
//! ## Multi-org tenancy (flag-gated, `auth.multi_org`)
//!
//! With `multi_org = false` (the default — dev_noop and single-org Zitadel
//! deployments) the resolver, if the principal carries one resolvable org,
//! returns that org-workspace directly (the legacy single-org path).
//!
//! With `multi_org = true` the resolver instead:
//!   - maps **every** Zitadel org claim to its bound workspace and auto-
//!     provisions membership in each (a principal can belong to several
//!     org-workspaces at once);
//!   - picks the active workspace from the principal's full membership set
//!     (the `active_workspace` cookie override, applied downstream, can swap
//!     to any other membership).
//!
//! In both modes, a principal with **no** resolvable org workspace and no
//! existing non-system membership is provisioned a personal workspace before
//! the active-workspace pick, so demoting `default` never strands a user.
//!
//! The flag never touches `dev_noop`: the dev-user's seeded personal-workspace
//! (`…0001`) owner `workspace_members` row pre-dates resolution, so it is
//! honoured in either mode (the flag governs auto-JOIN, not pre-existing
//! membership).

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
            // The DB resolver wrapping this one stamps platform-admin from the
            // config allow-list; the bare static resolver never grants it.
            is_platform_admin: false,
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
    /// Mirrors `AuthConfig.multi_org`. `false` (default) keeps the legacy
    /// single-org behaviour: auto-join `default` as `editor`. `true` enables
    /// real multi-org tenancy — see the gated branches in [`Self::resolve`].
    multi_org: bool,
    /// Mirrors `AuthConfig.platform_admins`: subjects or emails that resolve
    /// to `is_platform_admin = true`. Empty ⇒ no platform admins via config.
    platform_admins: Vec<String>,
}

impl DbPrincipalResolver {
    /// Construct with the legacy single-org behaviour (multi-org OFF) and no
    /// platform admins. Kept as the zero-config constructor so existing call
    /// sites / tests that don't care about tenancy are unaffected.
    pub fn new(db: PgPool) -> Self {
        Self::with_options(db, false, Vec::new())
    }

    /// Construct with the multi-org flag wired from `AuthConfig.multi_org`.
    /// Delegates to [`Self::with_options`] with an empty platform-admin list,
    /// so existing callers/tests stay green.
    pub fn with_multi_org(db: PgPool, multi_org: bool) -> Self {
        Self::with_options(db, multi_org, Vec::new())
    }

    /// Full constructor: multi-org flag + platform-admin allow-list. The
    /// composition root (`main.rs`) uses this; pass `config.auth.multi_org`
    /// and `config.auth.platform_admins`.
    pub fn with_options(db: PgPool, multi_org: bool, platform_admins: Vec<String>) -> Self {
        Self {
            inner: StaticPrincipalResolver,
            db,
            multi_org,
            platform_admins,
        }
    }

    /// Lazily mint a personal workspace (owner) for a principal that holds NO
    /// non-system membership and resolved no org workspace. Runs on the login
    /// hot path, before any handler — so demoting `default` to a system
    /// workspace never leaves a real principal homeless.
    ///
    /// Idempotent via the leading guard: a single `EXISTS` over the principal's
    /// non-system memberships. Once they own (or are a member of) any real
    /// tenant — the personal one minted here, an org workspace, or a self-serve
    /// one — this is a no-op, so repeated logins don't spawn duplicate tenants.
    ///
    /// Slug derivation (token-safe via the shared [`slugify`]):
    ///   1. the email local-part (`alice@corp` → `alice`),
    ///   2. else the display name,
    ///   3. else `u-{first 8 hex of subject_as_uuid}`.
    ///
    /// On a `slug` UNIQUE collision it retries `-{n}` (bounded) before falling
    /// back to the always-unique subject-hex slug.
    async fn ensure_personal_workspace(
        &self,
        user_id: Uuid,
        email: Option<&str>,
        display_name: Option<&str>,
        subject: &str,
    ) -> Result<(), AuthError> {
        use crate::handlers::workspaces::slugify;

        // Guard: already homed in a real (non-system) tenant ⇒ nothing to do.
        let already: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 \
               FROM workspace_members m \
               JOIN workspaces w ON w.id = m.workspace_id \
              WHERE m.user_id = $1 AND w.is_system = FALSE AND w.archived_at IS NULL \
              LIMIT 1",
        )
        .bind(user_id)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| AuthError::Internal(format!("personal workspace guard: {e}")))?;
        if already.is_some() {
            return Ok(());
        }

        // Human-facing display name for the new tenant: name → email → subject.
        let display = display_name
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or(email)
            .filter(|s| !s.is_empty())
            .unwrap_or(subject)
            .to_string();

        // Preferred slug: email local-part, else display name. Always-unique
        // fallback uses the first 8 hex of the deterministic subject uuid.
        let subject_hex = user_id.simple().to_string();
        let hex_fallback = format!("u-{}", &subject_hex[..8]);
        let preferred = email
            .and_then(|e| e.split('@').next())
            .map(slugify)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                display_name
                    .map(slugify)
                    .filter(|s: &String| !s.is_empty())
            })
            .unwrap_or_else(|| hex_fallback.clone());

        // Try the preferred slug, then `-{n}` variants, then the subject-hex
        // slug as a guaranteed-unique last resort. Mirrors the create_workspace
        // tx logic: insert workspace + owner membership atomically.
        const MAX_SLUG_ATTEMPTS: usize = 16;
        for attempt in 0..MAX_SLUG_ATTEMPTS {
            let slug = match attempt {
                0 => preferred.clone(),
                n if n < MAX_SLUG_ATTEMPTS - 1 => format!("{preferred}-{n}"),
                // Final attempt: the subject-hex slug, unique per principal.
                _ => hex_fallback.clone(),
            };
            match self.insert_personal_workspace(&slug, &display, user_id).await {
                Ok(()) => return Ok(()),
                Err(PersonalWsInsertError::SlugTaken) => continue,
                Err(PersonalWsInsertError::Db(e)) => return Err(e),
            }
        }
        Err(AuthError::Internal(
            "personal workspace: exhausted slug attempts".into(),
        ))
    }

    /// Insert one personal workspace + its owner membership in a single tx,
    /// distinguishing a slug UNIQUE collision (retryable) from a real DB error.
    async fn insert_personal_workspace(
        &self,
        slug: &str,
        display_name: &str,
        owner_id: Uuid,
    ) -> Result<(), PersonalWsInsertError> {
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|e| PersonalWsInsertError::Db(AuthError::Internal(format!("begin: {e}"))))?;

        let row: Result<(Uuid,), sqlx::Error> = sqlx::query_as(
            "INSERT INTO workspaces (slug, display_name) VALUES ($1, $2) RETURNING id",
        )
        .bind(slug)
        .bind(display_name)
        .fetch_one(&mut *tx)
        .await;

        let ws_id = match row {
            Ok((id,)) => id,
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
                return Err(PersonalWsInsertError::SlugTaken);
            }
            Err(e) => {
                return Err(PersonalWsInsertError::Db(AuthError::Internal(format!(
                    "personal workspace insert: {e}"
                ))));
            }
        };

        sqlx::query(
            "INSERT INTO workspace_members (workspace_id, user_id, role) \
             VALUES ($1, $2, 'owner')",
        )
        .bind(ws_id)
        .bind(owner_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            PersonalWsInsertError::Db(AuthError::Internal(format!(
                "personal workspace membership: {e}"
            )))
        })?;

        tx.commit().await.map_err(|e| {
            PersonalWsInsertError::Db(AuthError::Internal(format!("commit: {e}")))
        })?;
        Ok(())
    }
}

/// Outcome of a single personal-workspace insert attempt: a slug collision is
/// retryable (caller bumps `-{n}`), any other DB failure is terminal.
enum PersonalWsInsertError {
    SlugTaken,
    Db(AuthError),
}

#[async_trait]
impl PrincipalResolver for DbPrincipalResolver {
    async fn resolve(&self, claims: VerifiedClaims) -> Result<AuthUser, AuthError> {
        // Extract the full org-id set BEFORE delegating (the inner resolver
        // consumes `claims` and only keeps the first org id as metadata).
        let org_ids = org_ids_from_claims(&claims);

        let mut user = self.inner.resolve(claims).await?;
        let user_id = user.subject_as_uuid();

        // Platform-admin: match the principal's subject OR email against the
        // config allow-list. Stamped once here so every downstream gate reads
        // it off the resolved `AuthUser`.
        user.is_platform_admin = self.platform_admins.iter().any(|entry| {
            entry == &user.subject || user.email.as_deref() == Some(entry.as_str())
        });

        // NOTE: demos are NOT auto-enrolled. They stay discoverable through
        // `visibility = 'public'` (no `workspace_members` row), so a fresh
        // principal is not silently dropped into the shared `demos` system
        // namespace.

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

        // Lazy personal-workspace provisioning. We only reach here when NO org
        // workspace resolved (the multi_org path falls through, the single-org
        // path's early return did not fire). A principal with zero non-system
        // memberships gets a private personal workspace (owner) minted now —
        // BEFORE any handler runs — so the demotion of `default` to a system
        // workspace strands no one. Idempotent: it is a no-op once the principal
        // holds any non-system membership.
        self.ensure_personal_workspace(
            user_id,
            user.email.as_deref(),
            user.display_name.as_deref(),
            &user.subject,
        )
        .await?;

        // Active-workspace pick (covers BOTH modes' fall-through):
        //   - multi_org ON: choose among ALL memberships (org-workspaces +
        //     any explicit grants + the personal workspace just provisioned);
        //     the per-session cookie override applied downstream in
        //     `active_workspace::apply_override` can swap to any other one.
        //   - multi_org OFF with no resolvable org: the personal workspace
        //     just provisioned (or a pre-existing non-system membership),
        //     preferring real tenants over system workspaces, then by age.
        // `membership_workspace` returns `None` only when the user holds no
        // membership at all (which the personal-workspace provisioning above
        // prevents for any real principal), leaving `workspace_id = None` so
        // handlers reject rather than grant ambient access.
        user.workspace_id = membership_workspace(&self.db, user_id).await?;
        if let Some(ws_id) = user.workspace_id {
            user.workspace_role = lookup_role(&self.db, ws_id, user_id).await?;
        }
        Ok(user)
    }
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
///   1. any non-system workspace (a real tenant — the personal workspace, an
///      org workspace, or a self-serve one), oldest first;
///   2. the oldest system workspace (worst case: the principal is only in a
///      system namespace such as the demoted `default`/nil tenant).
///
/// The old `slug='default'` preference is gone: `default` is now a system
/// workspace (migration `20240189`) and real tenants must outrank it. The
/// workspace picker exposes the full membership list and lets the user override
/// this default per session via the active-workspace cookie.
async fn membership_workspace(db: &PgPool, user_id: Uuid) -> Result<Option<Uuid>, AuthError> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT w.id \
           FROM workspaces w \
           JOIN workspace_members m ON m.workspace_id = w.id \
          WHERE m.user_id = $1 AND w.archived_at IS NULL \
          ORDER BY w.is_system ASC, w.created_at ASC \
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
