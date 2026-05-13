use async_trait::async_trait;

use crate::{SecretError, SecretStore};

/// Resolves secrets from process environment variables.
///
/// This is the default backend. In production, replace with
/// Vault or AWS Secrets Manager implementations.
pub struct EnvVarSecretStore;

#[async_trait]
impl SecretStore for EnvVarSecretStore {
    async fn get(&self, key: &str) -> Result<String, SecretError> {
        std::env::var(key).map_err(|_| SecretError::NotFound(key.to_string()))
    }

    fn name(&self) -> &str {
        "env_var"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_existing_env_var() {
        unsafe { std::env::set_var("AITHERICON_TEST_SECRET_1", "test_value") };
        let store = EnvVarSecretStore;
        let result = store.get("AITHERICON_TEST_SECRET_1").await.unwrap();
        assert_eq!(result, "test_value");
        unsafe { std::env::remove_var("AITHERICON_TEST_SECRET_1") };
    }

    #[tokio::test]
    async fn returns_not_found_for_missing_var() {
        let store = EnvVarSecretStore;
        let result = store.get("AITHERICON_NONEXISTENT_SECRET_XYZ").await;
        assert!(matches!(result, Err(SecretError::NotFound(_))));
    }
}
