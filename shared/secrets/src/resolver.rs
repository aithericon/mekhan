use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::{SecretError, SecretStore};

static SECRET_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{secret:([^}]+)\}\}").unwrap());

/// Deep-scan a JSON value, replacing all `{{secret:KEY}}` patterns with resolved values.
///
/// - String values: patterns replaced inline (supports mixed text + secret refs)
/// - Objects/Arrays: recursed into
/// - Numbers/Booleans/Null: passed through unchanged
///
/// Returns a new JSON value with secrets resolved. The original is not modified.
pub async fn resolve_secrets(
    value: &Value,
    store: &dyn SecretStore,
) -> Result<Value, SecretError> {
    match value {
        Value::String(s) => resolve_string(s, store).await,
        Value::Object(map) => {
            let mut resolved = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                resolved.insert(k.clone(), Box::pin(resolve_secrets(v, store)).await?);
            }
            Ok(Value::Object(resolved))
        }
        Value::Array(arr) => {
            let mut resolved = Vec::with_capacity(arr.len());
            for v in arr {
                resolved.push(Box::pin(resolve_secrets(v, store)).await?);
            }
            Ok(Value::Array(resolved))
        }
        other => Ok(other.clone()),
    }
}

/// Scan a JSON value and return all secret keys referenced (without resolving).
pub fn extract_secret_keys(value: &Value) -> Vec<String> {
    let mut keys = Vec::new();
    extract_keys_recursive(value, &mut keys);
    keys
}

fn extract_keys_recursive(value: &Value, keys: &mut Vec<String>) {
    match value {
        Value::String(s) => {
            for cap in SECRET_PATTERN.captures_iter(s) {
                keys.push(cap[1].to_string());
            }
        }
        Value::Object(map) => {
            for v in map.values() {
                extract_keys_recursive(v, keys);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                extract_keys_recursive(v, keys);
            }
        }
        _ => {}
    }
}

async fn resolve_string(s: &str, store: &dyn SecretStore) -> Result<Value, SecretError> {
    if !SECRET_PATTERN.is_match(s) {
        return Ok(Value::String(s.to_string()));
    }

    // If the entire string is a single secret ref, return the resolved value directly
    if let Some(cap) = SECRET_PATTERN.captures(s) {
        if cap.get(0).unwrap().as_str() == s {
            let key = &cap[1];
            let resolved = store.get(key).await?;
            return Ok(Value::String(resolved));
        }
    }

    // Mixed text + secret refs: replace each match inline
    let mut result = s.to_string();
    for cap in SECRET_PATTERN.captures_iter(s) {
        let key = &cap[1];
        let resolved = store.get(key).await?;
        result = result.replace(cap.get(0).unwrap().as_str(), &resolved);
    }
    Ok(Value::String(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;

    struct MockStore(HashMap<String, String>);

    #[async_trait]
    impl SecretStore for MockStore {
        async fn get(&self, key: &str) -> Result<String, SecretError> {
            self.0
                .get(key)
                .cloned()
                .ok_or_else(|| SecretError::NotFound(key.to_string()))
        }
        fn name(&self) -> &str {
            "mock"
        }
    }

    fn mock_store() -> MockStore {
        MockStore(HashMap::from([
            ("API_TOKEN".into(), "sk-abc123".into()),
            ("DB_PASSWORD".into(), "hunter2".into()),
            ("BASE_URL".into(), "https://api.example.com".into()),
        ]))
    }

    #[tokio::test]
    async fn resolve_single_string() {
        let store = mock_store();
        let input = serde_json::json!("{{secret:API_TOKEN}}");
        let result = resolve_secrets(&input, &store).await.unwrap();
        assert_eq!(result, serde_json::json!("sk-abc123"));
    }

    #[tokio::test]
    async fn resolve_nested_json() {
        let store = mock_store();
        let input = serde_json::json!({
            "endpoint": "https://example.com",
            "auth": {
                "type": "bearer",
                "token": "{{secret:API_TOKEN}}"
            },
            "database": {
                "password": "{{secret:DB_PASSWORD}}"
            }
        });
        let result = resolve_secrets(&input, &store).await.unwrap();
        assert_eq!(result["auth"]["token"], "sk-abc123");
        assert_eq!(result["database"]["password"], "hunter2");
        assert_eq!(result["endpoint"], "https://example.com");
    }

    #[tokio::test]
    async fn resolve_mixed_text_and_secret() {
        let store = mock_store();
        let input = serde_json::json!("Bearer {{secret:API_TOKEN}}");
        let result = resolve_secrets(&input, &store).await.unwrap();
        assert_eq!(result, serde_json::json!("Bearer sk-abc123"));
    }

    #[tokio::test]
    async fn no_secrets_passthrough() {
        let store = mock_store();
        let input = serde_json::json!({
            "endpoint": "https://example.com",
            "retries": 3,
            "enabled": true,
            "tags": ["a", "b"]
        });
        let result = resolve_secrets(&input, &store).await.unwrap();
        assert_eq!(result, input);
    }

    #[tokio::test]
    async fn missing_secret_returns_error() {
        let store = mock_store();
        let input = serde_json::json!({"key": "{{secret:NONEXISTENT}}"});
        let result = resolve_secrets(&input, &store).await;
        assert!(matches!(result, Err(SecretError::NotFound(k)) if k == "NONEXISTENT"));
    }

    #[tokio::test]
    async fn resolve_in_array() {
        let store = mock_store();
        let input = serde_json::json!(["{{secret:API_TOKEN}}", "plain", "{{secret:DB_PASSWORD}}"]);
        let result = resolve_secrets(&input, &store).await.unwrap();
        assert_eq!(result[0], "sk-abc123");
        assert_eq!(result[1], "plain");
        assert_eq!(result[2], "hunter2");
    }

    #[tokio::test]
    async fn extract_keys_finds_all() {
        let input = serde_json::json!({
            "a": "{{secret:KEY_A}}",
            "b": {
                "c": "{{secret:KEY_B}}"
            },
            "d": ["{{secret:KEY_C}}", "no secret here"],
            "e": 42
        });
        let mut keys = extract_secret_keys(&input);
        keys.sort();
        assert_eq!(keys, vec!["KEY_A", "KEY_B", "KEY_C"]);
    }

    #[tokio::test]
    async fn extract_keys_empty_when_none() {
        let input = serde_json::json!({"a": "plain", "b": 42});
        let keys = extract_secret_keys(&input);
        assert!(keys.is_empty());
    }

    #[tokio::test]
    async fn resolve_multiple_refs_in_one_string() {
        let store = mock_store();
        let input = serde_json::json!("{{secret:BASE_URL}}/v1?token={{secret:API_TOKEN}}");
        let result = resolve_secrets(&input, &store).await.unwrap();
        assert_eq!(
            result,
            serde_json::json!("https://api.example.com/v1?token=sk-abc123")
        );
    }
}
