//! `ResourceSecretStore` — the write-side counterpart to
//! `aithericon-secrets::SecretStore`.
//!
//! `SecretStore::get` is read-only; nothing in the kernel implements
//! "write secrets to backend at this path". The B.9 CRUD flow needs that
//! capability — POST/PUT/rotate each lay down a new `resource_versions`
//! row alongside a fresh Vault write.
//!
//! This trait keeps the surface tiny (2 methods, no read methods) so the
//! resolver flow stays untouched: reads still go through
//! `SecretStore::get` via the engine's wrap path, and only the CRUD
//! handlers touch this write path.
//!
//! Two impls ship in this crate:
//! - [`InMemoryResourceStore`] (always available) — `RwLock<HashMap>`,
//!   used by tests and dev-without-Vault deployments.
//! - [`VaultResourceStore`] (feature `vault`) — delegates to
//!   `aithericon_secrets::VaultSecretStore`'s `put_kv` / `delete_kv`.
//!
//! Boot-time selection happens in the service binary (`mekhan-service`),
//! which reads `VAULT_ADDR` / `VAULT_TOKEN` and picks the right impl. Both
//! implementations preserve the per-version path layout
//! `aithericon/resources/{workspace_id}/{resource_id}/v{version}` — the
//! launcher's secret-template emitter relies on that exact shape, so the
//! choice of backend is invisible to the resolver.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map as JsonMap, Value};
use tokio::sync::RwLock;

/// Errors the store surfaces. Kept narrow — most failures are transport-
/// level (Vault unreachable / denied) or path-formatting bugs.
#[derive(Debug, thiserror::Error)]
pub enum ResourceStoreError {
    /// The path is malformed (empty, contains characters Vault would
    /// reject, etc.). Catches programmer errors before they reach Vault.
    #[error("invalid vault path: {0}")]
    InvalidPath(String),

    /// The backend rejected the write (auth failure, network failure,
    /// 500 from Vault, …). Wraps the underlying error message — the
    /// service translates this to HTTP 502.
    #[error("backend write failed: {0}")]
    Backend(String),

    /// The path doesn't exist (delete on a never-written path). Stays
    /// distinct from `Backend` so handlers can choose to swallow it for
    /// idempotent deletes.
    #[error("vault path not found: {0}")]
    NotFound(String),
}

/// Write-side companion to [`aithericon_secrets::SecretStore`].
///
/// Implementations write the secret fields for a resource version to the
/// configured backend at the launcher-deterministic path. Reads still go
/// through `SecretStore::get` — the trait is intentionally write-only.
#[async_trait]
pub trait ResourceSecretStore: Send + Sync {
    /// Write the secret fields for a resource version.
    ///
    /// `vault_path` is the per-version path (e.g.
    /// `aithericon/resources/<workspace>/<resource_id>/v3`); `secrets` is
    /// the subset of the create/rotate input whose keys are the
    /// descriptor's `secret_fields`. Empty maps are accepted (a write
    /// for a type with no secret fields is a no-op).
    async fn put_version(
        &self,
        vault_path: &str,
        secrets: &JsonMap<String, Value>,
    ) -> Result<(), ResourceStoreError>;

    /// Soft-delete the version at `vault_path`. Backends that don't
    /// support deletion (or where deletion is administratively gated)
    /// should treat this as a no-op rather than error.
    async fn delete_version(&self, vault_path: &str) -> Result<(), ResourceStoreError>;

    /// Diagnostic — which backend is this? Used in logs / health checks.
    fn backend_name(&self) -> &'static str;
}

/// In-process, in-memory store. Used by tests and dev-without-Vault
/// deployments. Thread-safe via `RwLock` so the handlers can share one
/// instance via `Arc`. Snapshotting / introspection helpers
/// ([`snapshot`](InMemoryResourceStore::snapshot), [`get_version`]) exist
/// so tests can assert the write side-effect without faking out the trait
/// or reaching into Vault.
#[derive(Default)]
pub struct InMemoryResourceStore {
    inner: Arc<RwLock<HashMap<String, JsonMap<String, Value>>>>,
}

impl InMemoryResourceStore {
    /// Construct an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read back the secrets at a path. Tests use this to assert that a
    /// create/rotate handler called `put_version` with the expected
    /// values without touching network.
    pub async fn get_version(&self, vault_path: &str) -> Option<JsonMap<String, Value>> {
        self.inner.read().await.get(vault_path).cloned()
    }

    /// Full snapshot — every `(path, fields)` pair. Order is HashMap-
    /// iteration order so tests should sort before comparing.
    pub async fn snapshot(&self) -> HashMap<String, JsonMap<String, Value>> {
        self.inner.read().await.clone()
    }

    /// How many secret records the store currently holds. Cheap way to
    /// assert "create wrote exactly one version".
    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    /// `true` when the store holds no secret records. Pairs with
    /// [`len`](Self::len) per clippy's `len_without_is_empty` convention.
    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }
}

#[async_trait]
impl ResourceSecretStore for InMemoryResourceStore {
    async fn put_version(
        &self,
        vault_path: &str,
        secrets: &JsonMap<String, Value>,
    ) -> Result<(), ResourceStoreError> {
        if vault_path.is_empty() {
            return Err(ResourceStoreError::InvalidPath(
                "empty vault_path".to_string(),
            ));
        }
        let mut inner = self.inner.write().await;
        inner.insert(vault_path.to_string(), secrets.clone());
        Ok(())
    }

    async fn delete_version(&self, vault_path: &str) -> Result<(), ResourceStoreError> {
        let mut inner = self.inner.write().await;
        // Idempotent: missing → silent success.
        inner.remove(vault_path);
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "in-memory"
    }
}

/// Vault-backed implementation. Delegates put/delete to
/// `VaultSecretStore::put_kv` / `delete_kv`. Reads still flow through the
/// existing `SecretStore::get` impl on the same `VaultSecretStore` — the
/// engine's wrap path picks them up automatically.
#[cfg(feature = "vault")]
pub struct VaultResourceStore {
    inner: aithericon_secrets::VaultSecretStore,
}

#[cfg(feature = "vault")]
impl VaultResourceStore {
    /// Wrap a configured `VaultSecretStore`. The caller controls mount /
    /// key_prefix / cache_ttl via the builder on the inner store; this
    /// wrapper carries no state of its own beyond the delegation.
    pub fn new(inner: aithericon_secrets::VaultSecretStore) -> Self {
        Self { inner }
    }

    /// Construct from `VAULT_ADDR` / `VAULT_TOKEN`. Returns `None` if
    /// either is missing — matches the env-driven boot path the service
    /// uses to fall back to `InMemoryResourceStore`.
    pub fn from_env() -> Option<Self> {
        aithericon_secrets::VaultSecretStore::from_env().map(Self::new)
    }
}

#[cfg(feature = "vault")]
#[async_trait]
impl ResourceSecretStore for VaultResourceStore {
    async fn put_version(
        &self,
        vault_path: &str,
        secrets: &JsonMap<String, Value>,
    ) -> Result<(), ResourceStoreError> {
        if vault_path.is_empty() {
            return Err(ResourceStoreError::InvalidPath(
                "empty vault_path".to_string(),
            ));
        }
        self.inner
            .put_kv(vault_path, secrets)
            .await
            .map_err(|e| ResourceStoreError::Backend(e.to_string()))
    }

    async fn delete_version(&self, vault_path: &str) -> Result<(), ResourceStoreError> {
        self.inner
            .delete_kv(vault_path)
            .await
            .map_err(|e| ResourceStoreError::Backend(e.to_string()))
    }

    fn backend_name(&self) -> &'static str {
        "vault"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_roundtrip() {
        let store = InMemoryResourceStore::new();
        let mut secrets = JsonMap::new();
        secrets.insert("password".to_string(), Value::String("hunter2".to_string()));
        store
            .put_version("aithericon/resources/ws/abc/v1", &secrets)
            .await
            .expect("put");
        let read = store
            .get_version("aithericon/resources/ws/abc/v1")
            .await
            .expect("present");
        assert_eq!(read["password"], "hunter2");
    }

    #[tokio::test]
    async fn in_memory_delete_is_idempotent() {
        let store = InMemoryResourceStore::new();
        // Deleting a never-written path must succeed.
        store
            .delete_version("aithericon/resources/none")
            .await
            .expect("idempotent delete");
        // Delete after write also clears.
        let mut secrets = JsonMap::new();
        secrets.insert("k".to_string(), Value::String("v".to_string()));
        store.put_version("p", &secrets).await.expect("put");
        store.delete_version("p").await.expect("delete");
        assert!(store.get_version("p").await.is_none());
    }

    #[tokio::test]
    async fn in_memory_rejects_empty_path() {
        let store = InMemoryResourceStore::new();
        let secrets = JsonMap::new();
        assert!(matches!(
            store.put_version("", &secrets).await,
            Err(ResourceStoreError::InvalidPath(_))
        ));
    }
}
