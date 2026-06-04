//! Data-plane stream credential seam (docs/25 §6).
//!
//! A data-channel `open` token carries a transport DESCRIPTOR
//! `{transport, subject, content_type, credential?}`. The optional `credential`
//! is a **single-use Vault wrapping token** scoping a NATS-subject grant to the
//! producer's datastream subject — provisioned the SAME way resource secrets are
//! (engine wraps at submit via [`SecretWrapper`], executor unwraps in a staging
//! hook via [`vault_unwrap_secrets`]). It is the data-plane analog of
//! `ExecutionJob.wrapped_secrets`.
//!
//! **Dev path is credential-LESS.** Dev NATS is open / no-auth, so the engine
//! mints the descriptor with `credential: None` and the executor connects to the
//! subject directly. The functions here are no-ops on `None` and never touch
//! Vault in that case — so `just dev` never requires Vault for streaming.
//!
//! The wrap seam lives here (not the engine) so the executor's unwrap side can
//! depend on the SAME envelope key without a cross-workspace type leak; the
//! transport (`executor-worker`) reads the unwrapped grant.

#[cfg(feature = "vault")]
use std::collections::HashMap;

#[cfg(feature = "vault")]
use crate::{vault_store::SecretWrapper, SecretError};

/// The Vault wrapping-token field name inside the wrapped data-stream grant. The
/// engine wraps `{ NATS_SUBJECT_GRANT_KEY: <subject perm> }`; the executor
/// unwraps and reads the same key back.
#[cfg(feature = "vault")]
pub const NATS_SUBJECT_GRANT_KEY: &str = "nats_subject_grant";

/// Wrap a scoped NATS-subject grant into a single-use Vault wrapping token for a
/// data-channel descriptor's `credential` field (docs/25 §6).
///
/// Mirrors resource-secret wrapping in the executor submit path: the grant
/// (here just the producer's datastream subject the consumer is allowed to read)
/// is wrapped into an opaque token that travels in the `open` descriptor over
/// NATS and is unwrapped exactly once by the consumer's executor.
///
/// Returns `Ok(None)` when no `wrapper` is configured — the **dev path**: the
/// descriptor then carries `credential: None` and the consumer connects to the
/// open subject directly, no Vault involved.
#[cfg(feature = "vault")]
pub async fn wrap_subject_grant(
    wrapper: Option<&dyn SecretWrapper>,
    subject: &str,
    ttl_secs: u64,
) -> Result<Option<String>, SecretError> {
    let Some(wrapper) = wrapper else {
        // Dev / open-NATS: no credential, connect directly.
        return Ok(None);
    };
    let mut grant = HashMap::new();
    grant.insert(NATS_SUBJECT_GRANT_KEY.to_string(), subject.to_string());
    let token = wrapper.wrap(grant, ttl_secs).await?;
    Ok(Some(token))
}

/// Unwrap a data-channel descriptor's `credential` back into the scoped NATS
/// subject grant, in the consumer's executor staging hook (docs/25 §6).
///
/// `credential` is the descriptor field as delivered: `None` on the dev path
/// (returns `Ok(None)`, no Vault call — the transport connects with no creds),
/// `Some(token)` in a wrapped deployment (single-use unwrap via the wrapping
/// token itself, no Vault service token needed — only `VAULT_ADDR`).
#[cfg(feature = "vault")]
pub async fn unwrap_subject_grant(
    vault_addr: Option<&str>,
    credential: Option<&str>,
) -> Result<Option<String>, SecretError> {
    let Some(credential) = credential else {
        // Dev / open-NATS: descriptor carried no credential.
        return Ok(None);
    };
    let vault_addr = vault_addr.ok_or_else(|| {
        SecretError::StoreUnavailable(
            "data-stream credential present but VAULT_ADDR is not set".to_string(),
        )
    })?;
    let unwrapped = crate::vault_unwrap_secrets(vault_addr, credential).await?;
    Ok(unwrapped.get(NATS_SUBJECT_GRANT_KEY).cloned())
}

#[cfg(all(test, feature = "vault"))]
mod tests {
    use super::*;
    use async_trait::async_trait;

    /// A wrapper that records what it was asked to wrap and returns a fixed token.
    struct RecordingWrapper {
        last: std::sync::Mutex<Option<HashMap<String, String>>>,
    }

    #[async_trait]
    impl SecretWrapper for RecordingWrapper {
        async fn wrap(
            &self,
            secrets: HashMap<String, String>,
            _ttl_secs: u64,
        ) -> Result<String, SecretError> {
            *self.last.lock().unwrap() = Some(secrets);
            Ok("wrap-token-xyz".to_string())
        }
    }

    /// Dev path: no wrapper ⇒ no credential, no Vault contact. This is the
    /// invariant that keeps `just dev` Vault-free for streaming.
    #[tokio::test]
    async fn wrap_without_wrapper_is_none() {
        let out = wrap_subject_grant(None, "executor.datastream.exec-1.frames", 600)
            .await
            .unwrap();
        assert!(out.is_none());
    }

    /// Wrapped deployment: the subject grant is wrapped under the shared key.
    #[tokio::test]
    async fn wrap_with_wrapper_produces_token_for_subject() {
        let wrapper = RecordingWrapper {
            last: std::sync::Mutex::new(None),
        };
        let out = wrap_subject_grant(
            Some(&wrapper),
            "executor.datastream.exec-1.frames",
            600,
        )
        .await
        .unwrap();
        assert_eq!(out.as_deref(), Some("wrap-token-xyz"));
        let grant = wrapper.last.lock().unwrap().clone().unwrap();
        assert_eq!(
            grant.get(NATS_SUBJECT_GRANT_KEY).map(String::as_str),
            Some("executor.datastream.exec-1.frames")
        );
    }

    /// Dev path on the consumer side: no credential ⇒ no unwrap, no Vault.
    #[tokio::test]
    async fn unwrap_none_credential_is_none() {
        let out = unwrap_subject_grant(Some("http://vault:8200"), None)
            .await
            .unwrap();
        assert!(out.is_none());
    }

    /// A credential present without a Vault address is a configuration error,
    /// not a silent connect-without-creds.
    #[tokio::test]
    async fn unwrap_credential_without_vault_addr_errors() {
        let err = unwrap_subject_grant(None, Some("wrap-token-xyz"))
            .await
            .unwrap_err();
        assert!(matches!(err, SecretError::StoreUnavailable(_)));
    }
}
