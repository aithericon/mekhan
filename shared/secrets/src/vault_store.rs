use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;

use crate::{SecretError, SecretStore};

/// Secret store backed by HashiCorp Vault KV v2 engine.
///
/// Keys use the format `"path#field"` to address a specific field within a
/// Vault secret, or just `"path"` which defaults to the `value` field.
///
/// An optional [`key_prefix`](Self::key_prefix) prepends to all paths so short
/// keys like `"API_TOKEN"` can resolve to `myapp/API_TOKEN` in Vault.
///
/// # Example
///
/// ```ignore
/// use aithericon_secrets::VaultSecretStore;
///
/// let store = VaultSecretStore::new("https://vault:8200", "hvs.token123")
///     .mount("secret")
///     .key_prefix("myapp/")
///     .cache_ttl(std::time::Duration::from_secs(60));
///
/// // Resolves: GET /v1/secret/data/myapp/db#password → field "password"
/// let pw = store.get("db#password").await?;
/// ```
pub struct VaultSecretStore {
    client: Client,
    addr: String,
    token: String,
    mount: String,
    key_prefix: String,
    cache: RwLock<HashMap<String, CachedEntry>>,
    cache_ttl: Duration,
}

struct CachedEntry {
    value: String,
    fetched_at: Instant,
}

/// Pluggable trait for wrapping resolved secrets into a single-use token.
///
/// Implementations produce an opaque string token that can be unwrapped exactly
/// once to recover the original key→value pairs. This enables secure secret
/// delivery over untrusted channels (e.g., NATS work queues).
#[async_trait]
pub trait SecretWrapper: Send + Sync {
    /// Wrap a map of resolved secret key→value pairs.
    ///
    /// Returns a single-use wrapping token string. The token is valid for
    /// `ttl_secs` seconds and can be unwrapped exactly once.
    async fn wrap(
        &self,
        secrets: HashMap<String, String>,
        ttl_secs: u64,
    ) -> Result<String, SecretError>;
}

#[derive(Deserialize)]
struct VaultResponse {
    data: Option<VaultData>,
    errors: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct VaultData {
    data: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct WrapResponse {
    wrap_info: Option<WrapInfo>,
    errors: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct WrapInfo {
    token: String,
}

#[derive(Deserialize)]
struct UnwrapResponse {
    data: Option<serde_json::Value>,
    errors: Option<Vec<String>>,
}

impl VaultSecretStore {
    /// Create a new Vault secret store.
    ///
    /// # Arguments
    /// - `addr` — Vault server address (e.g., `https://vault.example.com:8200`)
    /// - `token` — Vault authentication token
    pub fn new(addr: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            addr: addr.into().trim_end_matches('/').to_string(),
            token: token.into(),
            mount: "secret".to_string(),
            key_prefix: String::new(),
            cache: RwLock::new(HashMap::new()),
            cache_ttl: Duration::from_secs(300),
        }
    }

    /// Create from environment variables `VAULT_ADDR` and `VAULT_TOKEN`.
    ///
    /// Returns `None` if either variable is missing or empty.
    pub fn from_env() -> Option<Self> {
        let addr = std::env::var("VAULT_ADDR").ok().filter(|s| !s.is_empty())?;
        let token = std::env::var("VAULT_TOKEN").ok().filter(|s| !s.is_empty())?;
        Some(Self::new(addr, token))
    }

    /// Set the KV v2 mount point (default: `"secret"`).
    pub fn mount(mut self, mount: impl Into<String>) -> Self {
        self.mount = mount.into();
        self
    }

    /// Set a prefix prepended to all key paths before Vault lookup.
    ///
    /// With prefix `"myapp/"`, `get("DB_PASSWORD")` resolves to
    /// Vault path `myapp/DB_PASSWORD`.
    pub fn key_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.key_prefix = prefix.into();
        self
    }

    /// Set the cache TTL (default: 5 minutes). Set to `Duration::ZERO` to disable caching.
    pub fn cache_ttl(mut self, ttl: Duration) -> Self {
        self.cache_ttl = ttl;
        self
    }

    /// Parse key into `(vault_path, field_name)`.
    ///
    /// `"foo/bar#baz"` → `("foo/bar", "baz")`
    /// `"foo/bar"` → `("foo/bar", "value")`
    fn parse_key(&self, key: &str) -> (String, String) {
        let full_key = format!("{}{}", self.key_prefix, key);
        match full_key.split_once('#') {
            Some((path, field)) => (path.to_string(), field.to_string()),
            None => (full_key, "value".to_string()),
        }
    }

    fn check_cache(&self, key: &str) -> Option<String> {
        if self.cache_ttl.is_zero() {
            return None;
        }
        let cache = self.cache.read().ok()?;
        let entry = cache.get(key)?;
        if entry.fetched_at.elapsed() < self.cache_ttl {
            Some(entry.value.clone())
        } else {
            None
        }
    }

    fn insert_cache(&self, key: String, value: String) {
        if self.cache_ttl.is_zero() {
            return;
        }
        if let Ok(mut cache) = self.cache.write() {
            cache.insert(
                key,
                CachedEntry {
                    value,
                    fetched_at: Instant::now(),
                },
            );
        }
    }
}

#[async_trait]
impl SecretStore for VaultSecretStore {
    async fn get(&self, key: &str) -> Result<String, SecretError> {
        if let Some(cached) = self.check_cache(key) {
            return Ok(cached);
        }

        let (path, field) = self.parse_key(key);
        let url = format!("{}/v1/{}/data/{}", self.addr, self.mount, path);

        let response = self
            .client
            .get(&url)
            .header("X-Vault-Token", &self.token)
            .send()
            .await
            .map_err(|e| {
                SecretError::StoreUnavailable(format!("vault request failed: {e}"))
            })?;

        let status = response.status();

        if status == reqwest::StatusCode::FORBIDDEN
            || status == reqwest::StatusCode::UNAUTHORIZED
        {
            return Err(SecretError::AccessDenied(format!(
                "vault denied access to {path} (HTTP {status})"
            )));
        }

        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(SecretError::NotFound(key.to_string()));
        }

        if !status.is_success() {
            return Err(SecretError::StoreUnavailable(format!(
                "vault returned HTTP {status} for {path}"
            )));
        }

        let body: VaultResponse = response.json().await.map_err(|e| {
            SecretError::StoreUnavailable(format!("invalid vault response: {e}"))
        })?;

        if let Some(errors) = body.errors {
            if !errors.is_empty() {
                return Err(SecretError::StoreUnavailable(errors.join(", ")));
            }
        }

        let data = body
            .data
            .and_then(|d| d.data)
            .ok_or_else(|| SecretError::NotFound(key.to_string()))?;

        let value = match &data {
            serde_json::Value::Object(map) => map.get(&field).ok_or_else(|| {
                SecretError::NotFound(format!("{key} (field '{field}' not found)"))
            })?,
            _ => {
                return Err(SecretError::StoreUnavailable(
                    "unexpected vault data format".to_string(),
                ))
            }
        };

        let resolved = match value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };

        self.insert_cache(key.to_string(), resolved.clone());
        Ok(resolved)
    }

    fn name(&self) -> &str {
        "vault"
    }
}

#[async_trait]
impl SecretWrapper for VaultSecretStore {
    async fn wrap(
        &self,
        secrets: HashMap<String, String>,
        ttl_secs: u64,
    ) -> Result<String, SecretError> {
        let url = format!("{}/v1/sys/wrapping/wrap", self.addr);

        let response = self
            .client
            .post(&url)
            .header("X-Vault-Token", &self.token)
            .header("X-Vault-Wrap-TTL", ttl_secs.to_string())
            .json(&secrets)
            .send()
            .await
            .map_err(|e| {
                SecretError::StoreUnavailable(format!("vault wrap request failed: {e}"))
            })?;

        let status = response.status();
        if status == reqwest::StatusCode::FORBIDDEN
            || status == reqwest::StatusCode::UNAUTHORIZED
        {
            return Err(SecretError::AccessDenied(
                "vault denied access to sys/wrapping/wrap".to_string(),
            ));
        }
        if !status.is_success() {
            return Err(SecretError::StoreUnavailable(format!(
                "vault wrap returned HTTP {status}"
            )));
        }

        let body: WrapResponse = response.json().await.map_err(|e| {
            SecretError::StoreUnavailable(format!("invalid vault wrap response: {e}"))
        })?;

        if let Some(errors) = body.errors {
            if !errors.is_empty() {
                return Err(SecretError::StoreUnavailable(errors.join(", ")));
            }
        }

        body.wrap_info
            .map(|w| w.token)
            .ok_or_else(|| SecretError::StoreUnavailable("no wrap_info in response".to_string()))
    }
}

/// Unwrap a Vault wrapping token to recover the original secrets.
///
/// This is a standalone function because unwrapping does NOT require a Vault
/// service token — the wrapping token itself is used as `X-Vault-Token`.
/// This means the executor only needs `VAULT_ADDR`, not `VAULT_TOKEN`.
///
/// The wrapping token is single-use: Vault invalidates it after this call.
pub async fn vault_unwrap_secrets(
    vault_addr: &str,
    wrapping_token: &str,
) -> Result<HashMap<String, String>, SecretError> {
    let client = Client::new();
    let addr = vault_addr.trim_end_matches('/');
    let url = format!("{addr}/v1/sys/wrapping/unwrap");

    let response = client
        .post(&url)
        .header("X-Vault-Token", wrapping_token)
        .send()
        .await
        .map_err(|e| {
            SecretError::StoreUnavailable(format!("vault unwrap request failed: {e}"))
        })?;

    let status = response.status();
    if status == reqwest::StatusCode::BAD_REQUEST {
        return Err(SecretError::StoreUnavailable(
            "wrapping token is invalid or already used".to_string(),
        ));
    }
    if status == reqwest::StatusCode::FORBIDDEN
        || status == reqwest::StatusCode::UNAUTHORIZED
    {
        return Err(SecretError::AccessDenied(
            "wrapping token expired or revoked".to_string(),
        ));
    }
    if !status.is_success() {
        return Err(SecretError::StoreUnavailable(format!(
            "vault unwrap returned HTTP {status}"
        )));
    }

    let body: UnwrapResponse = response.json().await.map_err(|e| {
        SecretError::StoreUnavailable(format!("invalid vault unwrap response: {e}"))
    })?;

    if let Some(errors) = body.errors {
        if !errors.is_empty() {
            return Err(SecretError::StoreUnavailable(errors.join(", ")));
        }
    }

    let data = body
        .data
        .ok_or_else(|| SecretError::StoreUnavailable("no data in unwrap response".to_string()))?;

    serde_json::from_value::<HashMap<String, String>>(data).map_err(|e| {
        SecretError::StoreUnavailable(format!("unwrapped data is not a string map: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_with_field() {
        let store = VaultSecretStore::new("http://localhost:8200", "token");
        let (path, field) = store.parse_key("myapp/db#password");
        assert_eq!(path, "myapp/db");
        assert_eq!(field, "password");
    }

    #[test]
    fn parse_key_without_field_defaults_to_value() {
        let store = VaultSecretStore::new("http://localhost:8200", "token");
        let (path, field) = store.parse_key("myapp/api_token");
        assert_eq!(path, "myapp/api_token");
        assert_eq!(field, "value");
    }

    #[test]
    fn parse_key_with_prefix() {
        let store = VaultSecretStore::new("http://localhost:8200", "token")
            .key_prefix("project/");
        let (path, field) = store.parse_key("db#password");
        assert_eq!(path, "project/db");
        assert_eq!(field, "password");
    }

    #[test]
    fn cache_hit_and_miss() {
        let store = VaultSecretStore::new("http://localhost:8200", "token");
        assert!(store.check_cache("missing").is_none());

        store.insert_cache("hit".to_string(), "secret_value".to_string());
        assert_eq!(store.check_cache("hit").unwrap(), "secret_value");
    }

    #[test]
    fn cache_disabled_with_zero_ttl() {
        let store = VaultSecretStore::new("http://localhost:8200", "token")
            .cache_ttl(Duration::ZERO);
        store.insert_cache("key".to_string(), "val".to_string());
        assert!(store.check_cache("key").is_none());
    }

    #[test]
    fn from_env_returns_none_when_missing() {
        // Ensure vars are unset for this test
        std::env::remove_var("VAULT_ADDR");
        std::env::remove_var("VAULT_TOKEN");
        assert!(VaultSecretStore::from_env().is_none());
    }

    #[test]
    fn builder_pattern() {
        let store = VaultSecretStore::new("https://vault:8200", "hvs.token")
            .mount("kv")
            .key_prefix("app/")
            .cache_ttl(Duration::from_secs(60));
        assert_eq!(store.mount, "kv");
        assert_eq!(store.key_prefix, "app/");
        assert_eq!(store.cache_ttl, Duration::from_secs(60));
        assert_eq!(store.addr, "https://vault:8200");
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn get_secret_from_vault() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/secret/data/myapp/db"))
            .and(header("X-Vault-Token", "test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "data": {
                        "password": "s3cret",
                        "username": "admin"
                    },
                    "metadata": {
                        "version": 1
                    }
                }
            })))
            .mount(&server)
            .await;

        let store = VaultSecretStore::new(server.uri(), "test-token")
            .cache_ttl(Duration::ZERO);

        let pw = store.get("myapp/db#password").await.unwrap();
        assert_eq!(pw, "s3cret");

        let user = store.get("myapp/db#username").await.unwrap();
        assert_eq!(user, "admin");
    }

    #[tokio::test]
    async fn get_default_value_field() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/secret/data/myapp/api_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "data": {
                        "value": "tok_abc123"
                    }
                }
            })))
            .mount(&server)
            .await;

        let store = VaultSecretStore::new(server.uri(), "test-token")
            .cache_ttl(Duration::ZERO);

        let token = store.get("myapp/api_token").await.unwrap();
        assert_eq!(token, "tok_abc123");
    }

    #[tokio::test]
    async fn not_found_returns_error() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/secret/data/missing"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let store = VaultSecretStore::new(server.uri(), "test-token")
            .cache_ttl(Duration::ZERO);

        let err = store.get("missing").await.unwrap_err();
        assert!(matches!(err, SecretError::NotFound(_)));
    }

    #[tokio::test]
    async fn forbidden_returns_access_denied() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/secret/data/restricted"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let store = VaultSecretStore::new(server.uri(), "bad-token")
            .cache_ttl(Duration::ZERO);

        let err = store.get("restricted").await.unwrap_err();
        assert!(matches!(err, SecretError::AccessDenied(_)));
    }

    #[tokio::test]
    async fn missing_field_returns_not_found() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/secret/data/myapp/db"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "data": {
                        "username": "admin"
                    }
                }
            })))
            .mount(&server)
            .await;

        let store = VaultSecretStore::new(server.uri(), "test-token")
            .cache_ttl(Duration::ZERO);

        let err = store.get("myapp/db#password").await.unwrap_err();
        assert!(matches!(err, SecretError::NotFound(_)));
        assert!(err.to_string().contains("password"));
    }

    #[tokio::test]
    async fn key_prefix_prepends_to_path() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/secret/data/project/api_key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "data": { "value": "key123" }
                }
            })))
            .mount(&server)
            .await;

        let store = VaultSecretStore::new(server.uri(), "test-token")
            .key_prefix("project/")
            .cache_ttl(Duration::ZERO);

        let val = store.get("api_key").await.unwrap();
        assert_eq!(val, "key123");
    }

    #[tokio::test]
    async fn custom_mount_point() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/kv/data/myapp/secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "data": { "value": "mounted" }
                }
            })))
            .mount(&server)
            .await;

        let store = VaultSecretStore::new(server.uri(), "test-token")
            .mount("kv")
            .cache_ttl(Duration::ZERO);

        let val = store.get("myapp/secret").await.unwrap();
        assert_eq!(val, "mounted");
    }

    #[tokio::test]
    async fn cache_prevents_second_request() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/secret/data/cached"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "data": { "value": "cached_val" }
                }
            })))
            .expect(1) // wiremock asserts exactly 1 call
            .mount(&server)
            .await;

        let store = VaultSecretStore::new(server.uri(), "test-token")
            .cache_ttl(Duration::from_secs(300));

        let v1 = store.get("cached").await.unwrap();
        let v2 = store.get("cached").await.unwrap();
        assert_eq!(v1, "cached_val");
        assert_eq!(v2, "cached_val");
        // wiremock will panic on drop if called more than once
    }

    #[tokio::test]
    async fn numeric_value_returned_as_string() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/secret/data/config"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "data": { "port": 5432 }
                }
            })))
            .mount(&server)
            .await;

        let store = VaultSecretStore::new(server.uri(), "test-token")
            .cache_ttl(Duration::ZERO);

        let val = store.get("config#port").await.unwrap();
        assert_eq!(val, "5432");
    }

    // ── Wrapping / Unwrapping tests ────────────────────────────────────

    #[tokio::test]
    async fn wrap_secrets() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/sys/wrapping/wrap"))
            .and(header("X-Vault-Token", "service-token"))
            .and(header("X-Vault-Wrap-TTL", "600"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "wrap_info": {
                    "token": "hvs.wrapping123",
                    "ttl": 600,
                    "creation_time": "2024-01-01T00:00:00Z"
                }
            })))
            .mount(&server)
            .await;

        let store = VaultSecretStore::new(server.uri(), "service-token");
        let secrets = HashMap::from([
            ("API_KEY".to_string(), "sk-abc123".to_string()),
            ("DB_PASS".to_string(), "hunter2".to_string()),
        ]);

        let token = store.wrap(secrets, 600).await.unwrap();
        assert_eq!(token, "hvs.wrapping123");
    }

    #[tokio::test]
    async fn wrap_forbidden_returns_access_denied() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/sys/wrapping/wrap"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let store = VaultSecretStore::new(server.uri(), "bad-token");
        let err = store.wrap(HashMap::new(), 300).await.unwrap_err();
        assert!(matches!(err, SecretError::AccessDenied(_)));
    }

    #[tokio::test]
    async fn unwrap_secrets() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/sys/wrapping/unwrap"))
            .and(header("X-Vault-Token", "hvs.wrapping123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "API_KEY": "sk-abc123",
                    "DB_PASS": "hunter2"
                }
            })))
            .mount(&server)
            .await;

        let result = vault_unwrap_secrets(&server.uri(), "hvs.wrapping123")
            .await
            .unwrap();
        assert_eq!(result["API_KEY"], "sk-abc123");
        assert_eq!(result["DB_PASS"], "hunter2");
    }

    #[tokio::test]
    async fn unwrap_expired_token_returns_error() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/sys/wrapping/unwrap"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "errors": ["wrapping token is not valid or does not exist"]
            })))
            .mount(&server)
            .await;

        let err = vault_unwrap_secrets(&server.uri(), "hvs.expired")
            .await
            .unwrap_err();
        assert!(matches!(err, SecretError::StoreUnavailable(_)));
        assert!(err.to_string().contains("invalid or already used"));
    }

    #[tokio::test]
    async fn wrap_then_unwrap_roundtrip() {
        let server = MockServer::start().await;

        let secrets = HashMap::from([
            ("KEY_A".to_string(), "val_a".to_string()),
            ("KEY_B".to_string(), "val_b".to_string()),
        ]);

        // Mock wrap
        Mock::given(method("POST"))
            .and(path("/v1/sys/wrapping/wrap"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "wrap_info": { "token": "hvs.roundtrip" }
            })))
            .mount(&server)
            .await;

        // Mock unwrap — returns the same secrets
        Mock::given(method("POST"))
            .and(path("/v1/sys/wrapping/unwrap"))
            .and(header("X-Vault-Token", "hvs.roundtrip"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "KEY_A": "val_a",
                    "KEY_B": "val_b"
                }
            })))
            .mount(&server)
            .await;

        let store = VaultSecretStore::new(server.uri(), "service-token");
        let wrapping_token = store.wrap(secrets.clone(), 300).await.unwrap();
        assert_eq!(wrapping_token, "hvs.roundtrip");

        let unwrapped = vault_unwrap_secrets(&server.uri(), &wrapping_token)
            .await
            .unwrap();
        assert_eq!(unwrapped, secrets);
    }
}
