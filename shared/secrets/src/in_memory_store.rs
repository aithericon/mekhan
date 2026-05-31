use std::collections::HashMap;

use async_trait::async_trait;

use crate::{SecretError, SecretStore};

/// In-memory secret store backed by a `HashMap`.
///
/// Used to hold secrets that were unwrapped from a Vault wrapping token,
/// or for testing purposes.
pub struct InMemorySecretStore {
    secrets: HashMap<String, String>,
}

impl InMemorySecretStore {
    pub fn new(secrets: HashMap<String, String>) -> Self {
        Self { secrets }
    }
}

#[async_trait]
impl SecretStore for InMemorySecretStore {
    async fn get(&self, key: &str) -> Result<String, SecretError> {
        self.secrets
            .get(key)
            .cloned()
            .ok_or_else(|| SecretError::NotFound(key.to_string()))
    }

    fn name(&self) -> &str {
        "in-memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn get_existing_key() {
        let store = InMemorySecretStore::new(HashMap::from([("KEY".into(), "value".into())]));
        assert_eq!(store.get("KEY").await.unwrap(), "value");
    }

    #[tokio::test]
    async fn get_missing_key() {
        let store = InMemorySecretStore::new(HashMap::new());
        assert!(matches!(
            store.get("NOPE").await,
            Err(SecretError::NotFound(_))
        ));
    }
}
