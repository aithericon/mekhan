use async_trait::async_trait;

use crate::{SecretError, SecretStore};

/// Composite store that tries multiple backends in order.
///
/// Returns the first successful result. Stops on the first non-`NotFound` error
/// (i.e., `AccessDenied` or `StoreUnavailable` are not swallowed).
///
/// # Example
///
/// ```ignore
/// use aithericon_secrets::{ChainedSecretStore, EnvVarSecretStore, VaultSecretStore};
///
/// let store = ChainedSecretStore::new(vec![
///     Box::new(EnvVarSecretStore),
///     Box::new(VaultSecretStore::from_env().unwrap()),
/// ]);
/// // Tries env var first, falls back to Vault on NotFound
/// ```
pub struct ChainedSecretStore {
    stores: Vec<Box<dyn SecretStore>>,
    name: String,
}

impl ChainedSecretStore {
    pub fn new(stores: Vec<Box<dyn SecretStore>>) -> Self {
        let name = stores
            .iter()
            .map(|s| s.name())
            .collect::<Vec<_>>()
            .join(" -> ");
        Self { stores, name }
    }
}

#[async_trait]
impl SecretStore for ChainedSecretStore {
    async fn get(&self, key: &str) -> Result<String, SecretError> {
        let mut last_err = SecretError::NotFound(key.to_string());
        for store in &self.stores {
            match store.get(key).await {
                Ok(value) => return Ok(value),
                Err(SecretError::NotFound(_)) => {
                    last_err = SecretError::NotFound(key.to_string());
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err)
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EnvVarSecretStore;

    struct FixedStore(&'static str);

    #[async_trait]
    impl SecretStore for FixedStore {
        async fn get(&self, _key: &str) -> Result<String, SecretError> {
            Ok(self.0.to_string())
        }
        fn name(&self) -> &str {
            "fixed"
        }
    }

    struct FailStore;

    #[async_trait]
    impl SecretStore for FailStore {
        async fn get(&self, key: &str) -> Result<String, SecretError> {
            Err(SecretError::NotFound(key.to_string()))
        }
        fn name(&self) -> &str {
            "fail"
        }
    }

    struct DeniedStore;

    #[async_trait]
    impl SecretStore for DeniedStore {
        async fn get(&self, key: &str) -> Result<String, SecretError> {
            Err(SecretError::AccessDenied(key.to_string()))
        }
        fn name(&self) -> &str {
            "denied"
        }
    }

    #[tokio::test]
    async fn returns_first_success() {
        let chain = ChainedSecretStore::new(vec![
            Box::new(FailStore),
            Box::new(FixedStore("found_it")),
        ]);
        assert_eq!(chain.get("any").await.unwrap(), "found_it");
    }

    #[tokio::test]
    async fn all_not_found_returns_not_found() {
        let chain = ChainedSecretStore::new(vec![
            Box::new(FailStore),
            Box::new(FailStore),
        ]);
        assert!(matches!(
            chain.get("missing").await,
            Err(SecretError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn stops_on_non_not_found_error() {
        let chain = ChainedSecretStore::new(vec![
            Box::new(DeniedStore),
            Box::new(FixedStore("should_not_reach")),
        ]);
        assert!(matches!(
            chain.get("x").await,
            Err(SecretError::AccessDenied(_))
        ));
    }

    #[tokio::test]
    async fn name_shows_chain() {
        let chain = ChainedSecretStore::new(vec![
            Box::new(EnvVarSecretStore),
            Box::new(FailStore),
        ]);
        assert_eq!(chain.name(), "env_var -> fail");
    }
}
