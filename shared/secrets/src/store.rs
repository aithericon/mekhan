use async_trait::async_trait;

use crate::SecretError;

/// Pluggable backend for secret retrieval.
///
/// Implementations resolve secret keys to plaintext values.
/// Keys are namespaced strings (e.g., `"OPENAI_API_KEY"`, `"project/db-password"`).
#[async_trait]
pub trait SecretStore: Send + Sync {
    /// Resolve a single secret key to its plaintext value.
    async fn get(&self, key: &str) -> Result<String, SecretError>;

    /// Human-readable store name for diagnostics.
    fn name(&self) -> &str;
}
