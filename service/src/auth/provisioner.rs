//! User provisioning seam for invite-accept (Phase 4).
//!
//! Accepting an invite must map the invited email to a stable OIDC subject so
//! that a later real login lands on the same `workspace_members` /
//! `object_grants` rows. The trait abstracts "give me the sub for this email,
//! creating the identity if needed" so the accept handler is identical across
//! a real IdP (Zitadel) and offline dev.
//!
//! **Fail-closed selection (plan §4 / review M4):** [`build_user_provisioner`]
//! returns the real Zitadel provisioner for ANY non-`dev_noop` auth mode and the
//! deterministic [`NoopUserProvisioner`] ONLY under `dev_noop`. The
//! [`assert_provisioner_invariant`] boot check panics if a non-dev_noop process
//! somehow ended up with the Noop — a synthetic sub in production would silently
//! detach invited users from their real login.

use std::sync::Arc;

use async_trait::async_trait;

use super::mgmt::{MgmtError, ZitadelMgmt};
use crate::config::{AppConfig, AuthMode};

/// Resolve (or create) the OIDC subject for an invited email.
#[async_trait]
pub trait UserProvisioner: Send + Sync {
    /// Returns `(subject, newly_created)`. Idempotent on retry: an email that
    /// already has an identity reuses its existing subject (`newly_created =
    /// false`).
    async fn provision_or_resolve(
        &self,
        email: &str,
        display_name: Option<&str>,
    ) -> Result<(String, bool), MgmtError>;

    /// `true` for the offline Noop (synthetic subjects). The boot invariant
    /// rejects a synthetic provisioner under any non-`dev_noop` auth mode.
    fn is_synthetic(&self) -> bool {
        false
    }
}

/// Real provisioner: resolve-by-email first (re-invite reuse), else create a
/// Zitadel human user.
#[async_trait]
impl UserProvisioner for ZitadelMgmt {
    async fn provision_or_resolve(
        &self,
        email: &str,
        display_name: Option<&str>,
    ) -> Result<(String, bool), MgmtError> {
        if let Some(sub) = self.resolve_subject_by_email(email).await? {
            return Ok((sub, false));
        }
        let sub = self.create_human_user(email, display_name).await?;
        Ok((sub, true))
    }
}

/// Offline dev provisioner: a deterministic synthetic subject derived from the
/// email so `subject_as_uuid` is stable and the accept writes real DB rows. The
/// synthetic sub never matches a real login — fine for `dev_noop`, where every
/// request is the fixed dev user anyway and tests assert DB rows, not sessions.
#[derive(Default)]
pub struct NoopUserProvisioner;

#[async_trait]
impl UserProvisioner for NoopUserProvisioner {
    async fn provision_or_resolve(
        &self,
        email: &str,
        _display_name: Option<&str>,
    ) -> Result<(String, bool), MgmtError> {
        let slug: String = email
            .trim()
            .to_ascii_lowercase()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .take(64)
            .collect();
        Ok((format!("dev-invite-{slug}"), true))
    }

    fn is_synthetic(&self) -> bool {
        true
    }
}

/// Select the provisioner from auth config. `dev_noop` → deterministic Noop;
/// any other mode → real Zitadel broker (requires `auth.broker_pat` +
/// `issuer_url`). `None` when a real mode lacks broker credentials — the accept
/// handler then 503s rather than minting a synthetic sub in production.
pub fn build_user_provisioner(config: &AppConfig) -> Option<Arc<dyn UserProvisioner>> {
    match config.auth.mode {
        AuthMode::DevNoop => Some(Arc::new(NoopUserProvisioner)),
        _ => {
            let issuer = config.auth.issuer_url.as_deref()?;
            let pat = config.auth.broker_pat.clone()?;
            match ZitadelMgmt::new(issuer, pat) {
                Ok(m) => Some(Arc::new(m)),
                Err(e) => {
                    tracing::error!("invite provisioner: zitadel mgmt build failed: {e}");
                    None
                }
            }
        }
    }
}

/// Boot-time invariant (plan §4 / review M4): a non-`dev_noop` process must
/// never run with a synthetic provisioner, which would hand invited users a
/// subject no real login can match. Panics on violation. `None` is allowed (the
/// accept handler 503s).
pub fn assert_provisioner_invariant(
    mode: AuthMode,
    provisioner: &Option<Arc<dyn UserProvisioner>>,
) {
    if mode != AuthMode::DevNoop {
        if let Some(p) = provisioner {
            assert!(
                !p.is_synthetic(),
                "boot invariant violated: synthetic (Noop) invite provisioner under auth.mode={mode:?} — \
                 invited users would get a subject no real login matches"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_subject_is_deterministic_and_safe() {
        let n = NoopUserProvisioner;
        assert!(n.is_synthetic());
        let s1 = n
            .provision_or_resolve("Alice+Tag@Example.com", None)
            .await
            .unwrap();
        let s2 = n
            .provision_or_resolve("alice+tag@example.com", None)
            .await
            .unwrap();
        assert_eq!(s1.0, s2.0, "case/charset-normalized to a stable sub");
        assert_eq!(s1.0, "dev-invite-alice_tag_example_com");
        assert!(s1.1, "noop always reports newly_created");
    }

    #[test]
    fn invariant_allows_noop_under_dev_noop_only() {
        let p: Option<Arc<dyn UserProvisioner>> = Some(Arc::new(NoopUserProvisioner));
        assert_provisioner_invariant(AuthMode::DevNoop, &p); // no panic
    }

    #[test]
    #[should_panic(expected = "boot invariant violated")]
    fn invariant_panics_on_synthetic_under_bff() {
        let p: Option<Arc<dyn UserProvisioner>> = Some(Arc::new(NoopUserProvisioner));
        assert_provisioner_invariant(AuthMode::Bff, &p);
    }
}
