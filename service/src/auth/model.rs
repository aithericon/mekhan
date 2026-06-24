//! Auth domain types — pure, no I/O dependencies.

use std::collections::BTreeMap;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable namespace used to derive a UUID from an OIDC `sub` claim. This is the
/// LEGACY identity scheme: before the `users`/`user_identities` spine
/// (migration `20240195`), a principal's mekhan `user_id` *was* this
/// `v5(NAMESPACE, sub)` hash. It is still used as the deterministic mint seed
/// for a brand-new user keyed by an as-yet-unseen subject, so every id already
/// stamped on `workflow_instances.created_by` / `workspace_members.user_id` /
/// grant rows for an already-seen subject keeps resolving to the same value.
pub const SUBJECT_UUID_NAMESPACE: Uuid = Uuid::from_u128(0x6d65_6b68_616e_5f73_756a_6563_745f_7635);

/// An authenticated principal. The domain core works in terms of this type,
/// never in terms of JWTs or provider-specific claims.
///
/// `Serialize` is hand-written (not derived) so the wire form always carries a
/// `user_id` field (the resolved mekhan identity). It is now a REAL struct field
/// backed by the `users` spine (resolved by `DbPrincipalResolver`), not a
/// derived v5 hash — `subject_as_uuid()` returns it. The SPA seeds its profile
/// cache by the same UUID every `created_by`/`author_id`/grant row uses.
/// `Deserialize`/`ToSchema` stay derived; the field carries `#[serde(default)]`
/// so old session JSON (which had no real `user_id`, or carried the derived one)
/// still deserializes — the resolver/authenticator overwrites it on the next
/// request.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, utoipa::ToSchema)]
pub struct AuthUser {
    /// OIDC `sub` claim. Stable per identity within an issuer.
    pub subject: String,
    /// The resolved mekhan identity id (`users.id`). Populated by
    /// `DbPrincipalResolver`/the dev roster/token resolvers BEFORE any handler
    /// runs; this is the value `subject_as_uuid()` returns and the key every
    /// membership / grant / authorship row uses. `#[serde(default)]` keeps old
    /// session JSON deserializing (the value is overwritten on the next
    /// request); a bare/test construction may leave it `Uuid::nil()`.
    #[serde(default)]
    pub user_id: Uuid,
    pub email: Option<String>,
    pub display_name: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    /// Legacy upstream identity-provider org id slot. No longer populated —
    /// mekhan does not derive tenancy from the IdP org. Always `None`; the
    /// authoritative tenant is `workspace_id`, resolved from explicit
    /// `workspace_members` rows. Kept on the struct so old session JSON keeps
    /// deserializing.
    pub org_id: Option<String>,
    /// Whether this principal is a platform administrator — granted by the
    /// `auth.platform_admins` allow-list (subject or email) or the dev-noop
    /// seed. Gates platform-global governance affordances (the platform scope).
    /// `#[serde(default)]` keeps old session JSON deserializing to `false`.
    #[serde(default)]
    pub is_platform_admin: bool,
    /// Mekhan workspace the principal is currently acting in. Populated by
    /// `DbPrincipalResolver` from the user's `workspace_members` row (an
    /// accepted invite, an admin grant, a workspace they created, or the
    /// lazily-minted personal workspace). `None` only when no DB handle is
    /// available (unit tests + legacy session rows; `#[serde(default)]` keeps
    /// deserialize of old session JSON working).
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
    /// The caller's role (`owner`|`admin`|`editor`|`viewer`) in their resolved
    /// `workspace_id`. Populated everywhere `workspace_id` is set — from the
    /// active-workspace override, the resolver's default pick, or the dev-noop
    /// seed. `None` when no membership backs the workspace (or no DB handle in
    /// unit tests). Lets the SPA gate admin-only affordances client-side
    /// without a second round-trip; the server still enforces via `require_role`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_role: Option<String>,
    /// URL to the principal's profile photo, lifted from the OIDC `picture`
    /// claim by `StaticPrincipalResolver`. `None` for dev-noop and any IdP that
    /// doesn't assert a picture → the SPA renders initials. Real struct field
    /// (so `ToSchema` + the `users` mirror pick it up); set at every
    /// construction site (mostly `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
}

impl AuthUser {
    /// The principal's resolved mekhan identity id (`users.id`). Kept under its
    /// historical name + `-> Uuid` signature so the ~130 downstream call sites
    /// (every `created_by` / membership / grant write) stay untouched; only its
    /// source-of-value changed — it now returns the resolved `user_id` field
    /// instead of recomputing the v5 hash of `subject`.
    pub fn subject_as_uuid(&self) -> Uuid {
        self.user_id
    }

    /// The LEGACY `v5(SUBJECT_UUID_NAMESPACE, subject)` value. Before the
    /// `users` spine this *was* the identity id; it is now the deterministic
    /// mint seed for a never-before-seen subject (resolver step 3) and the fixed
    /// id for the seeded dev roster, so already-stamped rows for known subjects
    /// keep resolving to the same `users.id`.
    pub fn legacy_subject_uuid(subject: &str) -> Uuid {
        Uuid::new_v5(&SUBJECT_UUID_NAMESPACE, subject.as_bytes())
    }

    /// The caller's active tenant workspace, or a 403 if none is resolved.
    ///
    /// Tenant-facing handlers MUST gate on this instead of falling back to
    /// `Uuid::nil()` — acting in the nil/default tenant on behalf of a
    /// principal with no active workspace silently leaks data across the
    /// isolation boundary. A principal reaches a handler with no
    /// `workspace_id` only when the resolver could not provision/pick one
    /// (e.g. a session predating personal-workspace lazy provisioning); the
    /// safe answer is to refuse the tenant-scoped action.
    pub fn require_workspace(&self) -> Result<Uuid, crate::models::error::ApiError> {
        self.workspace_id
            .ok_or_else(|| crate::models::error::ApiError::forbidden("no active workspace"))
    }
}

/// Hand-written so `user_id` (the resolved `subject_as_uuid()`) is always
/// emitted in a stable position, regardless of where it sits in the struct.
/// Mirrors the field-presence rules the derive would produce
/// (`workspace_role`/`avatar_url` skipped when `None`), and emits `user_id`
/// unconditionally. Keep in sync with the field list above.
impl Serialize for AuthUser {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Base 7 always-present fields (incl. is_platform_admin) + the resolved
        // user_id; +1 each for the two optionals when present (advisory len for
        // non-self-describing formats).
        let mut len = 8;
        if self.workspace_role.is_some() {
            len += 1;
        }
        if self.avatar_url.is_some() {
            len += 1;
        }
        let mut s = serializer.serialize_struct("AuthUser", len)?;
        s.serialize_field("subject", &self.subject)?;
        s.serialize_field("email", &self.email)?;
        s.serialize_field("display_name", &self.display_name)?;
        s.serialize_field("roles", &self.roles)?;
        s.serialize_field("org_id", &self.org_id)?;
        s.serialize_field("workspace_id", &self.workspace_id)?;
        // Always present — the whole reason this impl is hand-written.
        s.serialize_field("user_id", &self.subject_as_uuid())?;
        s.serialize_field("is_platform_admin", &self.is_platform_admin)?;
        match &self.workspace_role {
            Some(role) => s.serialize_field("workspace_role", role)?,
            None => s.skip_field("workspace_role")?,
        }
        match &self.avatar_url {
            Some(avatar) => s.serialize_field("avatar_url", avatar)?,
            None => s.skip_field("avatar_url")?,
        }
        s.end()
    }
}

/// JWT claims after the verifier has checked signature, issuer, audience,
/// and expiry. Not yet mapped onto our domain user — that's the resolver's
/// job (provider-specific role claim names live there, not here).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedClaims {
    pub subject: String,
    pub issuer: String,
    pub audience: Vec<String>,
    pub expires_at: i64,
    /// All remaining claims, keyed by their JWT name (e.g. `email`, `name`,
    /// `urn:zitadel:iam:org:project:roles`). Resolver picks the ones it
    /// knows about.
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(subject: &str) -> AuthUser {
        AuthUser {
            subject: subject.to_string(),
            user_id: AuthUser::legacy_subject_uuid(subject),
            email: None,
            display_name: None,
            roles: Vec::new(),
            org_id: None,
            is_platform_admin: false,
            workspace_id: None,
            workspace_role: None,
            avatar_url: None,
        }
    }

    #[test]
    fn serialize_always_emits_derived_user_id() {
        // Guards M8: `user_id` must never be absent/null on the wire, for ANY
        // construction site, so the SPA profile cache seed can't break.
        let u = bare("dev-user");
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(
            v["user_id"],
            serde_json::json!(u.subject_as_uuid().to_string())
        );
        // Optionals absent when None (matches the prior derive behaviour).
        assert!(!v.as_object().unwrap().contains_key("workspace_role"));
        assert!(!v.as_object().unwrap().contains_key("avatar_url"));
    }

    #[test]
    fn serialize_includes_avatar_when_present() {
        let mut u = bare("alice");
        u.avatar_url = Some("https://idp/a.png".into());
        u.workspace_role = Some("admin".into());
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(v["avatar_url"], serde_json::json!("https://idp/a.png"));
        assert_eq!(v["workspace_role"], serde_json::json!("admin"));
    }

    #[test]
    fn deserialize_ignores_serialized_user_id() {
        // Round-trip: the emitted `user_id` key is ignored on the way back
        // (recomputed from `subject`), so session JSON round-trips to an equal
        // value without `user_id` being a struct field.
        let u = bare("dev-user");
        let json = serde_json::to_string(&u).unwrap();
        let back: AuthUser = serde_json::from_str(&json).unwrap();
        assert_eq!(u, back);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("missing or malformed Authorization header")]
    MissingToken,
    #[error("invalid token: {0}")]
    InvalidToken(String),
    #[error("token has expired")]
    Expired,
    #[error("issuer does not match expected")]
    IssuerMismatch,
    #[error("audience does not match expected")]
    AudienceMismatch,
    #[error("unable to fetch signing keys: {0}")]
    JwksUnavailable(String),
    #[error("internal auth error: {0}")]
    Internal(String),
}
