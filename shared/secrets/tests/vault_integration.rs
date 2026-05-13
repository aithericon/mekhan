#![cfg(feature = "vault")]
//! Integration tests against a real HashiCorp Vault container (dev mode).
//!
//! Requires Docker. Run with:
//! ```bash
//! cargo test --features vault --test vault_integration -- --test-threads=1
//! ```

use std::collections::HashMap;
use std::time::Duration;

use aithericon_secrets::{
    extract_secret_keys, resolve_secrets, InMemorySecretStore, SecretStore, SecretWrapper,
    VaultSecretStore,
};
use testcontainers::core::WaitFor;
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt};
use tokio::sync::OnceCell;

// ---------------------------------------------------------------------------
// Shared Vault container (one per test binary)
// ---------------------------------------------------------------------------

struct SharedVault {
    addr: String,
    root_token: String,
    _container: testcontainers::ContainerAsync<GenericImage>,
}

static SHARED_VAULT: OnceCell<SharedVault> = OnceCell::const_new();

const ROOT_TOKEN: &str = "test-root-token";

async fn shared_vault() -> &'static SharedVault {
    SHARED_VAULT
        .get_or_init(|| async {
            let container = GenericImage::new("hashicorp/vault", "1.15")
                .with_exposed_port(8200.into())
                .with_wait_for(WaitFor::message_on_stdout("Vault server started"))
                .with_env_var("VAULT_DEV_ROOT_TOKEN_ID", ROOT_TOKEN)
                .with_env_var("VAULT_DEV_LISTEN_ADDRESS", "0.0.0.0:8200")
                .start()
                .await
                .expect("Failed to start Vault testcontainer");

            let host = container.get_host().await.expect("get_host");
            let port = container.get_host_port_ipv4(8200).await.expect("get_port");
            let addr = format!("http://{host}:{port}");

            // Poll /v1/sys/health until ready
            let client = reqwest::Client::new();
            for _ in 0..60 {
                match client.get(format!("{addr}/v1/sys/health")).send().await {
                    Ok(resp) if resp.status().is_success() => break,
                    _ => tokio::time::sleep(Duration::from_millis(500)).await,
                }
            }

            SharedVault {
                addr,
                root_token: ROOT_TOKEN.to_string(),
                _container: container,
            }
        })
        .await
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write a secret to Vault KV v2 at `secret/data/{path}`.
async fn vault_kv_put(addr: &str, token: &str, path: &str, data: &serde_json::Value) {
    let client = reqwest::Client::new();
    let url = format!("{addr}/v1/secret/data/{path}");
    let body = serde_json::json!({ "data": data });
    let resp = client
        .post(&url)
        .header("X-Vault-Token", token)
        .json(&body)
        .send()
        .await
        .expect("vault KV put failed");
    assert!(
        resp.status().is_success(),
        "vault KV put returned {}: {}",
        resp.status(),
        resp.text().await.unwrap_or_default()
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_kv_store_and_retrieve() {
    let v = shared_vault().await;

    // Write a secret via Vault HTTP API
    vault_kv_put(
        &v.addr,
        &v.root_token,
        "integration/db",
        &serde_json::json!({
            "username": "admin",
            "password": "hunter2",
            "port": 5432
        }),
    )
    .await;

    // Read back via VaultSecretStore
    let store = VaultSecretStore::new(&v.addr, &v.root_token).cache_ttl(Duration::ZERO);

    let pw = store.get("integration/db#password").await.unwrap();
    assert_eq!(pw, "hunter2");

    let user = store.get("integration/db#username").await.unwrap();
    assert_eq!(user, "admin");

    // Numeric values come back as their string representation
    let port = store.get("integration/db#port").await.unwrap();
    assert_eq!(port, "5432");
}

#[tokio::test]
async fn test_wrap_unwrap_real_vault() {
    let v = shared_vault().await;

    let store = VaultSecretStore::new(&v.addr, &v.root_token);

    let secrets = HashMap::from([
        ("API_KEY".to_string(), "sk-live-abc123".to_string()),
        ("DB_PASS".to_string(), "super-secret".to_string()),
    ]);

    // Wrap
    let wrapping_token = store.wrap(secrets.clone(), 300).await.unwrap();
    assert!(!wrapping_token.is_empty());

    // Unwrap
    let unwrapped =
        aithericon_secrets::vault_unwrap_secrets(&v.addr, &wrapping_token)
            .await
            .unwrap();
    assert_eq!(unwrapped["API_KEY"], "sk-live-abc123");
    assert_eq!(unwrapped["DB_PASS"], "super-secret");
}

#[tokio::test]
async fn test_unwrap_single_use() {
    let v = shared_vault().await;

    let store = VaultSecretStore::new(&v.addr, &v.root_token);
    let secrets = HashMap::from([("KEY".to_string(), "value".to_string())]);

    let wrapping_token = store.wrap(secrets, 300).await.unwrap();

    // First unwrap succeeds
    let result = aithericon_secrets::vault_unwrap_secrets(&v.addr, &wrapping_token).await;
    assert!(result.is_ok(), "first unwrap should succeed");

    // Second unwrap with same token fails — this is the key security property
    let result2 = aithericon_secrets::vault_unwrap_secrets(&v.addr, &wrapping_token).await;
    assert!(result2.is_err(), "second unwrap should fail (single-use)");
}

#[tokio::test]
async fn test_unwrap_without_vault_token() {
    let v = shared_vault().await;

    // Wrap with the real root token
    let store = VaultSecretStore::new(&v.addr, &v.root_token);
    let secrets = HashMap::from([("SECRET".to_string(), "data".to_string())]);
    let wrapping_token = store.wrap(secrets, 300).await.unwrap();

    // Unwrap using vault_unwrap_secrets — this function does NOT use VAULT_TOKEN,
    // it uses the wrapping token itself as X-Vault-Token. This proves the executor
    // only needs VAULT_ADDR, not VAULT_TOKEN.
    let unwrapped =
        aithericon_secrets::vault_unwrap_secrets(&v.addr, &wrapping_token)
            .await
            .unwrap();
    assert_eq!(unwrapped["SECRET"], "data");
}

#[tokio::test]
async fn test_wrap_with_ttl_expiry() {
    let v = shared_vault().await;

    let store = VaultSecretStore::new(&v.addr, &v.root_token);
    let secrets = HashMap::from([("EPHEMERAL".to_string(), "gone-soon".to_string())]);

    // Wrap with a very short TTL (2 seconds)
    let wrapping_token = store.wrap(secrets, 2).await.unwrap();

    // Wait for TTL to expire
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Unwrap should fail — token expired
    let result = aithericon_secrets::vault_unwrap_secrets(&v.addr, &wrapping_token).await;
    assert!(result.is_err(), "unwrap after TTL expiry should fail");
}

#[tokio::test]
async fn test_full_secret_resolution_pipeline() {
    let v = shared_vault().await;

    // Step 1: Write secrets to Vault KV
    vault_kv_put(
        &v.addr,
        &v.root_token,
        "pipeline/creds",
        &serde_json::json!({
            "api_key": "sk-pipeline-test",
            "db_password": "pipeline-pw"
        }),
    )
    .await;

    // Step 2: Create a config JSON with {{secret:KEY}} refs
    let config = serde_json::json!({
        "command": "python3",
        "args": ["train.py"],
        "env": {
            "API_KEY": "{{secret:pipeline/creds#api_key}}",
            "DB_PASSWORD": "{{secret:pipeline/creds#db_password}}",
            "NORMAL_VAR": "no-secret-here"
        }
    });

    // Step 3: Extract secret keys
    let keys = extract_secret_keys(&config);
    assert_eq!(keys.len(), 2);
    assert!(keys.contains(&"pipeline/creds#api_key".to_string()));
    assert!(keys.contains(&"pipeline/creds#db_password".to_string()));

    // Step 4: Resolve secrets from Vault
    let store = VaultSecretStore::new(&v.addr, &v.root_token).cache_ttl(Duration::ZERO);
    let mut resolved = HashMap::new();
    for key in &keys {
        let value = store.get(key).await.unwrap();
        resolved.insert(key.clone(), value);
    }
    assert_eq!(resolved["pipeline/creds#api_key"], "sk-pipeline-test");
    assert_eq!(resolved["pipeline/creds#db_password"], "pipeline-pw");

    // Step 5: Wrap resolved secrets
    let wrapping_token = store.wrap(resolved.clone(), 300).await.unwrap();

    // Step 6: Unwrap (simulating executor side — only needs VAULT_ADDR)
    let unwrapped =
        aithericon_secrets::vault_unwrap_secrets(&v.addr, &wrapping_token)
            .await
            .unwrap();

    // Step 7: Build InMemorySecretStore from unwrapped secrets
    let mem_store = InMemorySecretStore::new(unwrapped);

    // Step 8: Resolve refs in config using the in-memory store
    let resolved_config = resolve_secrets(&config, &mem_store).await.unwrap();

    // Verify
    assert_eq!(
        resolved_config["env"]["API_KEY"], "sk-pipeline-test",
        "secret ref should be resolved"
    );
    assert_eq!(
        resolved_config["env"]["DB_PASSWORD"], "pipeline-pw",
        "secret ref should be resolved"
    );
    assert_eq!(
        resolved_config["env"]["NORMAL_VAR"], "no-secret-here",
        "non-secret values should pass through"
    );
    assert_eq!(
        resolved_config["command"], "python3",
        "non-secret fields should pass through"
    );
}
