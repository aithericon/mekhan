//! `StaticPrincipalResolver` — maps verified JWT claims onto Mekhan's
//! `AuthUser`. Reads the Zitadel-specific roles claim layout for role names,
//! so this is the one place provider-specific identifiers live outside the
//! adapter itself.
//!
//! `DbPrincipalResolver` wraps the static one with a database lookup that
//! reconciles the principal onto the `users` identity spine and resolves a
//! `workspace_id` from their `workspace_members` rows. A fresh principal with
//! no existing non-system membership is lazily given a **personal** workspace
//! (owner) on first login — see [`DbPrincipalResolver::ensure_personal_workspace`].
//!
//! ## No org→workspace auto-provisioning
//!
//! Earlier revisions read the Zitadel org-id set out of the roles claim and
//! auto-joined the principal into each org-bound workspace as `editor` (via a
//! since-dropped `workspaces` org-binding column). That coupling is gone:
//! mekhan no longer derives
//! tenancy from the upstream IdP org. Membership now comes only from explicit
//! sources — an invite the user accepts, an admin-granted `workspace_members`
//! row, a workspace they create, or the personal workspace minted below. The
//! `org_id` claim is dropped entirely (`AuthUser.org_id = None`); only role
//! names are still lifted off the roles claim.
//!
//! Earlier revisions also auto-joined every principal into the seeded `default`
//! (nil) workspace and into every system workspace (`demos`). Both are gone:
//! migration `20240189` demoted `default` to `is_system = TRUE` (internals /
//! legacy only), and demos stay discoverable through `visibility = 'public'`
//! rather than an auto-membership row. A fresh principal therefore lands in a
//! private personal workspace, not a shared catch-all tenant.
//!
//! ## What the resolver auto-provisions
//!
//! On every resolve a principal with **no** existing non-system membership is
//! provisioned a personal workspace (owner) before the active-workspace pick —
//! see [`DbPrincipalResolver::ensure_personal_workspace`] — so demoting
//! `default` never strands a user. Optionally (flag-gated, see below) a
//! `viewer` row is upserted in every system workspace.
//!
//! The resolver does **not** auto-join the shared `default` tenant. `dev_noop`
//! is untouched: the dev-user's seeded personal-workspace (`…0001`) owner
//! `workspace_members` row pre-dates resolution, so it is honoured regardless
//! (the flag governs auto-JOIN, not pre-existing membership).
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
/// role names only; the nested org ids are deliberately ignored — mekhan no
/// longer derives tenancy from the upstream IdP org.
const ZITADEL_ROLES_CLAIM: &str = "urn:zitadel:iam:org:project:roles";

/// Identity-provider key stamped on `user_identities.provider` for the
/// BFF / JWT (Zitadel) path. A future second IdP would use a different value;
/// the column is the seam that keeps `(provider, subject)` links disjoint.
///
/// Public so the member-admin path (`handlers::workspaces::add_member`) can
/// resolve a subject through the SAME identity spine the resolver writes,
/// instead of recomputing `v5(subject)` and orphaning reconciled users.
pub const ZITADEL_PROVIDER: &str = "zitadel";

/// Read the OIDC `email_verified` claim, accepting either a JSON boolean
/// `true` or the string `"true"` (some IdPs/serializers stringify it). Anything
/// else (absent, `false`, `"false"`, non-bool) is treated as unverified so a
/// spoofable email can never silently merge two principals.
fn email_verified_from_claims(claims: &VerifiedClaims) -> bool {
    match claims.extra.get("email_verified") {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

#[derive(Debug, Clone, Default)]
pub struct StaticPrincipalResolver;

#[async_trait]
impl PrincipalResolver for StaticPrincipalResolver {
    async fn resolve(&self, claims: VerifiedClaims) -> Result<AuthUser, AuthError> {
        let email = string_claim(&claims, "email");
        let display_name =
            string_claim(&claims, "name").or_else(|| string_claim(&claims, "preferred_username"));
        // Standard OIDC `picture` claim — a URL to the user's profile photo.
        // Mirrored into `users.avatar_url` by the extractor upsert.
        let avatar_url = string_claim(&claims, "picture");

        let roles: Vec<String> = match claims.extra.get(ZITADEL_ROLES_CLAIM) {
            Some(Value::Object(roles_obj)) => roles_obj.keys().cloned().collect(),
            _ => Vec::new(),
        };
        // `org_id` is no longer derived from the IdP — mekhan does not couple
        // tenancy to the upstream org. Always `None`; the authoritative tenant
        // is `workspace_id`, resolved from explicit `workspace_members` rows.
        let org_id = None;

        // Legacy v5 hash as the provisional id. `DbPrincipalResolver` overwrites
        // it with the reconciled `users.id` (`resolve_user_id`) before any
        // handler runs; the bare static resolver (no DB) keeps the legacy value
        // so already-seen subjects still map to their historical id.
        let user_id = AuthUser::legacy_subject_uuid(&claims.subject);
        Ok(AuthUser {
            subject: claims.subject,
            user_id,
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
    /// Mirrors `AuthConfig.auto_join_system_workspaces`. `false` (default)
    /// leaves system workspaces (`demos`) un-joined; `true` mints a `viewer`
    /// row in each on every resolve.
    auto_join_system_workspaces: bool,
    /// Mirrors `AuthConfig.platform_admins`: subjects or emails that resolve
    /// to `is_platform_admin = true`. Empty ⇒ no platform admins via config.
    platform_admins: Vec<String>,
}

impl DbPrincipalResolver {
    /// Construct with the isolation-preserving defaults (no system-workspace
    /// auto-join, no platform admins). Zero-config constructor for call sites /
    /// tests that don't exercise tenancy flags.
    pub fn new(db: PgPool) -> Self {
        Self::with_options(db, false, Vec::new())
    }

    /// Full constructor: system-workspace auto-join flag + platform-admin
    /// allow-list. The composition root (`main.rs`) uses this; pass
    /// `config.auth.{auto_join_system_workspaces, platform_admins}`.
    pub fn with_options(
        db: PgPool,
        auto_join_system_workspaces: bool,
        platform_admins: Vec<String>,
    ) -> Self {
        Self {
            inner: StaticPrincipalResolver,
            db,
            auto_join_system_workspaces,
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
        // Extract the email-verified flag BEFORE delegating (the inner resolver
        // consumes `claims`).
        let email_verified = email_verified_from_claims(&claims);

        let mut user = self.inner.resolve(claims).await?;

        // Reconcile the principal onto the `users` spine: an existing
        // (provider, subject) identity wins; else a verified email matches an
        // existing user; else a new user is minted keyed by the legacy v5 hash
        // (so already-seen subjects keep their historical id). MUST run before
        // any membership / personal-workspace logic, which all key off
        // `user.user_id` / `subject_as_uuid()`.
        user.user_id = resolve_user_id(
            &self.db,
            ZITADEL_PROVIDER,
            &user.subject,
            user.email.as_deref(),
            email_verified,
            user.display_name.as_deref(),
            user.avatar_url.as_deref(),
        )
        .await?;
        let user_id = user.user_id;

        // Platform-admin: match the principal's subject OR email against the
        // config allow-list. Stamped once here so every downstream gate reads
        // it off the resolved `AuthUser`.
        user.is_platform_admin = self.platform_admins.iter().any(|entry| {
            entry == &user.subject || user.email.as_deref() == Some(entry.as_str())
        });

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

        // NOTE: the resolver deliberately does NOT auto-join any workspace from
        // IdP claims. Earlier builds read the Zitadel org-id set out of the
        // roles claim and enrolled the principal into each org-bound workspace
        // as `editor` (via a since-dropped `workspaces` org-binding column), and
        // before that auto-joined the shared `default` tenant — both broke
        // isolation.
        // A principal now holds membership only where they genuinely belong (an
        // accepted invite, an explicit admin grant, a workspace they created,
        // or a seeded row like dev_noop's personal workspace); anyone left
        // without one is given a personal workspace below rather than dropped
        // into a shared or org-derived tenant.

        // Lazy personal-workspace provisioning. A principal with zero non-system
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

        // Active-workspace pick: choose among ALL memberships (any explicit
        // grants + the personal workspace just provisioned), preferring real
        // tenants over system workspaces, then by age. The per-session cookie
        // override applied downstream in `active_workspace::apply_override` can
        // swap to any other membership. `membership_workspace` returns `None`
        // only when the user holds no membership at all (which the
        // personal-workspace provisioning above prevents for any real
        // principal), leaving `workspace_id = None` so handlers reject rather
        // than grant ambient access.
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

/// Reconcile a verified principal onto the `users` identity spine and return
/// their stable mekhan `users.id`, in ONE transaction. Three-step order:
///
///   1. **Identity hit.** `(provider, subject)` already linked → that user_id.
///      Best-effort refresh of `users.email/display_name/avatar_url` from the
///      fresh claims (never demotes a set field to NULL).
///   2. **Verified-email reconciliation.** Only when `email_verified` AND an
///      email is present: an existing `users.email` (CITEXT, case-insensitive)
///      adopts this new subject — link the identity and return that id. This is
///      what lets the same human re-provisioned under a new subject keep their
///      grants/memberships.
///   3. **New user.** Mint keyed by the LEGACY v5 hash of the subject so any
///      already-stamped `created_by`/membership/grant rows for this subject
///      resolve to the same id. If the email collides with a DIFFERENT existing
///      user (CITEXT UNIQUE), fall back to inserting this user with email NULL
///      rather than violate the constraint (an unverified/duplicate email never
///      hijacks another identity). Then link the identity.
///
/// `provider` is the IdP key (`zitadel`); `subject` is the RAW OIDC subject
/// (never rekeyed). The returned id is the value `AuthUser::subject_as_uuid()`
/// exposes downstream.
async fn resolve_user_id(
    db: &PgPool,
    provider: &str,
    subject: &str,
    email: Option<&str>,
    email_verified: bool,
    display_name: Option<&str>,
    avatar_url: Option<&str>,
) -> Result<Uuid, AuthError> {
    let mut tx = db
        .begin()
        .await
        .map_err(|e| AuthError::Internal(format!("resolve_user_id begin: {e}")))?;

    // --- Step 1: existing identity link. --------------------------------
    let existing: Option<(Uuid,)> =
        sqlx::query_as("SELECT user_id FROM user_identities WHERE provider = $1 AND subject = $2")
            .bind(provider)
            .bind(subject)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| AuthError::Internal(format!("identity lookup: {e}")))?;

    if let Some((user_id,)) = existing {
        // Best-effort attribute refresh: only overwrite a column when the fresh
        // claim carries a value (COALESCE keeps the prior value on NULL). Email
        // is refreshed only when verified and only if it does not collide with
        // a different user (guarded by a NOT EXISTS so the CITEXT UNIQUE can't
        // throw and abort the tx).
        let refresh_email = if email_verified { email } else { None };
        sqlx::query(
            "UPDATE users SET \
                display_name = COALESCE($2, display_name), \
                avatar_url   = COALESCE($3, avatar_url), \
                email = CASE \
                    WHEN $4::citext IS NOT NULL \
                     AND NOT EXISTS (SELECT 1 FROM users u2 WHERE u2.email = $4::citext AND u2.id <> $1) \
                    THEN $4::citext ELSE email END, \
                updated_at = now() \
              WHERE id = $1",
        )
        .bind(user_id)
        .bind(display_name)
        .bind(avatar_url)
        .bind(refresh_email)
        .execute(&mut *tx)
        .await
        .map_err(|e| AuthError::Internal(format!("identity refresh: {e}")))?;

        tx.commit()
            .await
            .map_err(|e| AuthError::Internal(format!("resolve_user_id commit: {e}")))?;
        return Ok(user_id);
    }

    // --- Step 2: verified-email reconciliation. -------------------------
    if email_verified {
        if let Some(addr) = email {
            let by_email: Option<(Uuid,)> =
                sqlx::query_as("SELECT id FROM users WHERE email = $1::citext")
                    .bind(addr)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| AuthError::Internal(format!("user by email: {e}")))?;
            if let Some((user_id,)) = by_email {
                sqlx::query(
                    "INSERT INTO user_identities (provider, subject, user_id, email_verified) \
                     VALUES ($1, $2, $3, true) \
                     ON CONFLICT (provider, subject) DO NOTHING",
                )
                .bind(provider)
                .bind(subject)
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| AuthError::Internal(format!("link identity by email: {e}")))?;

                tx.commit()
                    .await
                    .map_err(|e| AuthError::Internal(format!("resolve_user_id commit: {e}")))?;
                return Ok(user_id);
            }
        }
    }

    // --- Step 3: mint a new user keyed by the legacy v5 hash. -----------
    let new_id = AuthUser::legacy_subject_uuid(subject);

    // Determine whether the email is free to claim. Only set it when it does
    // not collide with a DIFFERENT existing user; otherwise insert with NULL so
    // the CITEXT UNIQUE never throws. (When the id ALREADY exists — a legacy
    // subject reused — the DO UPDATE path applies the same best-effort refresh.)
    let email_for_insert: Option<&str> = match email {
        Some(addr) => {
            let collides: Option<(Uuid,)> =
                sqlx::query_as("SELECT id FROM users WHERE email = $1::citext AND id <> $2")
                    .bind(addr)
                    .bind(new_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| AuthError::Internal(format!("email collision probe: {e}")))?;
            if collides.is_some() {
                None
            } else {
                Some(addr)
            }
        }
        None => None,
    };

    sqlx::query(
        "INSERT INTO users (id, email, display_name, avatar_url, status) \
         VALUES ($1, $2::citext, $3, $4, 'active') \
         ON CONFLICT (id) DO UPDATE SET \
             email = COALESCE(EXCLUDED.email, users.email), \
             display_name = COALESCE(EXCLUDED.display_name, users.display_name), \
             avatar_url = COALESCE(EXCLUDED.avatar_url, users.avatar_url), \
             updated_at = now()",
    )
    .bind(new_id)
    .bind(email_for_insert)
    .bind(display_name)
    .bind(avatar_url)
    .execute(&mut *tx)
    .await
    .map_err(|e| AuthError::Internal(format!("user insert: {e}")))?;

    sqlx::query(
        "INSERT INTO user_identities (provider, subject, user_id, email_verified) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (provider, subject) DO NOTHING",
    )
    .bind(provider)
    .bind(subject)
    .bind(new_id)
    .bind(email_verified)
    .execute(&mut *tx)
    .await
    .map_err(|e| AuthError::Internal(format!("link new identity: {e}")))?;

    tx.commit()
        .await
        .map_err(|e| AuthError::Internal(format!("resolve_user_id commit: {e}")))?;
    Ok(new_id)
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

    #[tokio::test]
    async fn static_resolver_never_sets_org_id_and_lifts_roles() {
        // The roles claim is `{ "<role>": { "<org_id>": "<domain>" } }`. We
        // lift the role names but deliberately ignore the nested org ids —
        // `org_id` is always `None` now that tenancy is decoupled from the IdP.
        let mut roles = serde_json::Map::new();
        let mut org = serde_json::Map::new();
        org.insert("org-a".into(), Value::String("org-a.example".into()));
        roles.insert("editor".into(), Value::Object(org));

        let mut extra = BTreeMap::new();
        extra.insert(ZITADEL_ROLES_CLAIM.into(), Value::Object(roles));
        let user = StaticPrincipalResolver
            .resolve(claims_with(extra))
            .await
            .unwrap();
        assert_eq!(user.org_id, None);
        assert!(user.roles.contains(&"editor".to_string()));
    }
}
