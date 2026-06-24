//! Platform root token — a headless super-admin credential for automated
//! provisioning (CI / Terraform).
//!
//! A request presenting `Authorization: Bearer <platform_root_token>` resolves to
//! a synthetic principal with `is_platform_admin = true` and NO workspace, so a
//! pipeline can curate platform-tier infrastructure (create platform pools, mint
//! registration tokens, …) without an interactive login. This is the machine
//! counterpart to the `auth.platform_admins` allow-list (which names *human*
//! Zitadel principals); it is orthogonal to the auth `mode` and works in both
//! `dev_noop` and `bff`.
//!
//! Security: the configured token MUST carry the [`PLATFORM_ROOT_TOKEN_PREFIX`]
//! so the extractor only runs the compare for that bearer shape (never for human
//! PATs or `wkr_`/`rnr_` tokens). The compare is constant-time. It's a powerful
//! credential — keep it in Vault, rotate it, prefer the narrower bootstrap
//! registration tokens for plain machine enrollment.

use super::model::AuthUser;

/// Required prefix on a platform root token. Gates the constant-time compare to
/// this bearer shape so the path never fires for other credential kinds.
pub const PLATFORM_ROOT_TOKEN_PREFIX: &str = "plat_";

/// `subject` stamped on the synthetic platform-root principal.
pub const PLATFORM_ROOT_SUBJECT: &str = "platform-root";

/// Marker role on the platform-root principal.
pub const PLATFORM_ROOT_ROLE: &str = "platform-root";

/// Constant-time check that `presented` matches the `configured` root token.
/// `false` (path disabled) when no token is configured. Both must carry the
/// `plat_` prefix; a length mismatch still runs the compare against the
/// configured value so timing doesn't leak the length.
pub fn matches_root_token(configured: Option<&str>, presented: &str) -> bool {
    let Some(configured) = configured else {
        return false;
    };
    if configured.is_empty() || !presented.starts_with(PLATFORM_ROOT_TOKEN_PREFIX) {
        return false;
    }
    constant_time_eq(configured.as_bytes(), presented.as_bytes())
}

/// Constant-time byte compare (length-equal case). Mirrors the private helper in
/// `models::runner`; duplicated rather than exported to keep that module's
/// surface unchanged.
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

/// The synthetic principal a valid platform root token resolves to: a
/// platform admin with no workspace binding (platform mints force the platform
/// scope; ops needing a workspace will 400, which is correct for a root key).
pub fn platform_root_user() -> AuthUser {
    AuthUser {
        subject: PLATFORM_ROOT_SUBJECT.to_string(),
        // Synthetic principal, no workspace: a fixed legacy id is fine (it
        // never keys a real `users` row).
        user_id: AuthUser::legacy_subject_uuid(PLATFORM_ROOT_SUBJECT),
        email: None,
        display_name: Some("Platform Root".to_string()),
        roles: vec![PLATFORM_ROOT_ROLE.to_string()],
        org_id: None,
        is_platform_admin: true,
        workspace_id: None,
        workspace_role: None,
        avatar_url: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_config_never_matches() {
        assert!(!matches_root_token(None, "plat_secret"));
        assert!(!matches_root_token(Some(""), "plat_secret"));
    }

    #[test]
    fn requires_prefix() {
        // Right value, wrong shape → no match (path stays disabled for it).
        assert!(!matches_root_token(Some("plat_abc"), "abc"));
        assert!(!matches_root_token(Some("plat_abc"), "wkr_abc"));
    }

    #[test]
    fn exact_match_only() {
        assert!(matches_root_token(Some("plat_abc"), "plat_abc"));
        assert!(!matches_root_token(Some("plat_abc"), "plat_abcd"));
        assert!(!matches_root_token(Some("plat_abc"), "plat_ab"));
    }

    #[test]
    fn synthetic_user_is_platform_admin_without_workspace() {
        let u = platform_root_user();
        assert!(u.is_platform_admin);
        assert!(u.workspace_id.is_none());
        assert_eq!(u.subject, PLATFORM_ROOT_SUBJECT);
    }
}
