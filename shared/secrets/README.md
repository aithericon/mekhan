# aithericon-secrets

Pluggable secret resolution for the Aithericon platform. Provides a `SecretStore` trait, a `{{secret:KEY}}` reference pattern, and Vault response wrapping for secure secret delivery over untrusted channels.

## Quick Start

```toml
# Cargo.toml
[dependencies]
aithericon-secrets = { git = "https://github.com/aithericon/aithericon-secrets" }

# Enable Vault backend (KV store + response wrapping)
aithericon-secrets = { git = "https://github.com/aithericon/aithericon-secrets", features = ["vault"] }
```

```rust
use aithericon_secrets::{resolve_secrets, EnvVarSecretStore};

let config = serde_json::json!({
    "endpoint": "https://api.example.com",
    "auth_token": "{{secret:API_TOKEN}}"
});

let store = EnvVarSecretStore;
let resolved = resolve_secrets(&config, &store).await?;
// auth_token is now the value of $API_TOKEN
```

## Secret Reference Syntax

Secret references use the pattern `{{secret:KEY}}` inside JSON string values. The resolver deep-scans any JSON structure (objects, arrays, nested values) and replaces all matches.

| Pattern | Behavior |
|---------|----------|
| `"{{secret:API_KEY}}"` | Entire value replaced with resolved secret |
| `"Bearer {{secret:TOKEN}}"` | Inline replacement within a string |
| `"{{secret:BASE_URL}}/v1?key={{secret:API_KEY}}"` | Multiple refs in one string |
| `"plain text"` | Passed through unchanged |

For Vault keys, the `path#field` syntax addresses specific fields within a Vault KV secret:

| Key | Vault Path | Field |
|-----|-----------|-------|
| `"demo/api#key"` | `secret/data/demo/api` | `key` |
| `"demo/db#password"` | `secret/data/demo/db` | `password` |
| `"demo/token"` | `secret/data/demo/token` | `value` (default) |

## Backends

### `EnvVarSecretStore`

Resolves keys from process environment variables. No configuration needed.

```rust
use aithericon_secrets::EnvVarSecretStore;

let store = EnvVarSecretStore;
// {{secret:API_TOKEN}} resolves from $API_TOKEN
```

### `VaultSecretStore` (feature: `vault`)

Reads secrets from HashiCorp Vault KV v2 engine. Supports field extraction, key prefixes, and secret caching.

```rust
use aithericon_secrets::VaultSecretStore;

let store = VaultSecretStore::new("https://vault:8200", "hvs.service-token")
    .mount("secret")           // KV v2 mount point (default: "secret")
    .key_prefix("myapp/")      // Prepended to all key paths
    .cache_ttl(Duration::from_secs(60));  // Default: 5 min. ZERO disables.

// {{secret:db#password}} → GET /v1/secret/data/myapp/db → field "password"
```

**From environment:**

```rust
// Reads VAULT_ADDR and VAULT_TOKEN from env
let store = VaultSecretStore::from_env();
```

### `InMemorySecretStore`

Holds secrets in a `HashMap`. Used internally to hold secrets unwrapped from Vault wrapping tokens, and for testing.

```rust
use aithericon_secrets::InMemorySecretStore;
use std::collections::HashMap;

let store = InMemorySecretStore::new(HashMap::from([
    ("API_KEY".into(), "sk-abc123".into()),
]));
```

### `ChainedSecretStore`

Tries multiple backends in order. Returns the first success. Stops on non-`NotFound` errors (e.g., `AccessDenied` is not swallowed).

```rust
use aithericon_secrets::{ChainedSecretStore, EnvVarSecretStore, VaultSecretStore};

let store = ChainedSecretStore::new(vec![
    Box::new(EnvVarSecretStore),
    Box::new(VaultSecretStore::from_env().unwrap()),
]);
// Tries env var first, falls back to Vault on NotFound
```

### Custom Backends

Implement the `SecretStore` trait:

```rust
use aithericon_secrets::{SecretStore, SecretError};
use async_trait::async_trait;

struct MyStore { /* ... */ }

#[async_trait]
impl SecretStore for MyStore {
    async fn get(&self, key: &str) -> Result<String, SecretError> {
        // Fetch from AWS Secrets Manager, K8s secrets, database, etc.
        todo!()
    }

    fn name(&self) -> &str {
        "my-store"
    }
}
```

## Vault Response Wrapping

The crate supports Vault's [cubbyhole response wrapping](https://developer.hashicorp.com/vault/docs/concepts/response-wrapping) pattern for secure secret delivery over untrusted channels like NATS message queues.

### The Problem

When the engine dispatches a job to a remote executor via NATS, the job spec may contain `{{secret:KEY}}` references that need resolving. Sending plaintext secrets on NATS is a security risk — any subscriber on the stream can read them.

### The Solution

```
  Engine (petri-lab)                  NATS                    Executor
  ─────────────────                  ─────                   ─────────
  1. Scan spec for {{secret:KEY}}
  2. Resolve keys via SecretStore
  3. Wrap resolved values into       ───────────────────►
     single-use Vault wrapping       Job payload contains:
     token (hvs.xxx)                 - spec with unresolved refs
                                     - wrapped_secrets: "hvs.xxx"
                                     - NO plaintext secrets
                                                              4. Unwrap token → HashMap
                                                              5. Build InMemorySecretStore
                                                              6. Resolve {{secret:KEY}} refs
                                                              7. Run process with secrets
                                                                 as env vars
                                                              8. Token invalidated (single-use)
```

### Wrapping (Engine Side)

`VaultSecretStore` implements the `SecretWrapper` trait:

```rust
use aithericon_secrets::SecretWrapper;

let store = VaultSecretStore::new("https://vault:8200", "hvs.service-token");
let secrets = HashMap::from([
    ("demo/api#key".into(), "sk-live-12345".into()),
    ("demo/db#password".into(), "super-secret".into()),
]);

// Creates a single-use Vault wrapping token (valid for 600 seconds)
let wrapping_token = store.wrap(secrets, 600).await?;
// wrapping_token = "hvs.CAESIJnL..." (opaque, single-use)
```

### Unwrapping (Executor Side)

The standalone `vault_unwrap_secrets()` function unwraps without needing a Vault service token — the wrapping token itself is used as authentication:

```rust
use aithericon_secrets::vault_unwrap_secrets;

// Only needs VAULT_ADDR, not VAULT_TOKEN
let secrets = vault_unwrap_secrets("https://vault:8200", &wrapping_token).await?;
// secrets = {"demo/api#key": "sk-live-12345", "demo/db#password": "super-secret"}
```

### Security Properties

- **Single-use**: Vault invalidates the wrapping token after the first unwrap. A second attempt returns HTTP 400.
- **TTL-bound**: Wrapping tokens expire after a configurable TTL (default: 600 seconds). Expired tokens return HTTP 403.
- **No service token needed**: The executor only needs `VAULT_ADDR` to unwrap. It never receives `VAULT_TOKEN` — the wrapping token itself acts as a scoped, one-time credential.
- **Wire safety**: The NATS message contains only the opaque wrapping token, never plaintext secret values.

## Reference Integration: The Aithericon Platform

This crate is the shared secret-handling layer for the Aithericon platform. A typical deployment pairs it with two other components to deliver secrets over NATS without exposing plaintext on the wire:

| Component | Role | Status |
|-----------|------|--------|
| `aithericon-secrets` | Secret store traits, resolution, Vault wrapping/unwrapping | **This crate (open source)** |
| [`aithericon-executor`](https://github.com/aithericon/aithericon-executor) | Worker that unwraps secrets and resolves refs in job spec (`crates/executor-worker/src/staging.rs`) | Open source |
| Workflow engine ("petri-lab") | Engine that wraps secrets before NATS publish | Aithericon internal |

The engine side is documented here as a reference for anyone building their own wrapping producer; see the wrap/unwrap flow below.

### End-to-End Flow

**1. Scenario defines secret references**

In an SDK scenario, a job spec contains `{{secret:KEY}}` refs instead of plaintext:

```rust
ctx.seed(&exec_queue, vec![Job {
    spec: serde_json::json!({
        "type": "process",
        "config": {
            "command": "python3",
            "args": ["train.py"],
            "env": {
                "API_KEY": "{{secret:demo/api#key}}",
                "DB_PASS": "{{secret:demo/db#password}}"
            }
        }
    }),
    // ...
}]);
```

**2. Engine resolves and wraps (petri-lab)**

When the `executor_submit` effect fires, the `ExecutorNatsClient` in `petri-executor`:

1. Scans `spec.config` for `{{secret:KEY}}` patterns via `extract_secret_keys()`
2. Resolves each key against the configured `SecretStore` (Vault KV)
3. Wraps all resolved values into a single-use Vault wrapping token via `SecretWrapper::wrap()`
4. Attaches the token to `ExecutionJob.wrapped_secrets`
5. Publishes the job to NATS — the spec retains unresolved `{{secret:KEY}}` refs

Separately, in `firing.rs`, effect transition configs also get just-in-time resolution: the engine resolves `{{secret:KEY}}` refs in `effect_config` before passing to handlers, but the event log always stores the original unresolved config.

**3. Executor unwraps and resolves (aithericon-executor)**

The `InjectSecretsHook` in the executor's staging pipeline:

1. Checks `job.wrapped_secrets` for a Vault wrapping token
2. Calls `vault_unwrap_secrets(vault_addr, wrapping_token)` — Vault invalidates the token
3. Builds an `InMemorySecretStore` from the unwrapped key-value pairs
4. Resolves `{{secret:KEY}}` patterns in `RunContext.env` and `spec.config`
5. The process runs with secrets available as environment variables

If no wrapping token is present, the hook falls back to the executor's configured `SecretStore` (typically `EnvVarSecretStore`).

### Configuration

**Engine side (the producer that wraps secrets):**

| Variable | Required | Description |
|----------|----------|-------------|
| `VAULT_ADDR` | Yes | Vault server address (e.g., `http://localhost:8200`) |
| `VAULT_TOKEN` | Yes | Vault service token with read access to KV secrets and `sys/wrapping/wrap` |

**Executor (aithericon-executor)**:

| Variable | Required | Description |
|----------|----------|-------------|
| `VAULT_ADDR` | Yes | Vault server address (same Vault instance). No `VAULT_TOKEN` needed. |
| `EXECUTOR_NATS_URL` | Yes | NATS server address |
| `EXECUTOR_NAMESPACE` | Yes | Must match the engine's namespace |

Feature flags: `cargo build -p aithericon-executor-service --features vault`

**Vault (HashiCorp Vault)**:

The engine's `VAULT_TOKEN` needs these Vault policies:

```hcl
# Read secrets from KV v2
path "secret/data/*" {
  capabilities = ["read"]
}

# Create wrapping tokens
path "sys/wrapping/wrap" {
  capabilities = ["update"]
}
```

The executor needs no Vault policy — it authenticates with the single-use wrapping token itself.

### End-to-End Demo

A reference end-to-end demo (Vault dev container + engine + executor + a job with `{{secret:KEY}}` refs) lives inside Aithericon's internal workflow engine. The integration tests in this crate (`tests/vault_integration.rs`) exercise the same wrap/unwrap primitives against a real Vault via [testcontainers](https://crates.io/crates/testcontainers).

## Error Handling

| Error | Cause | Resolution |
|-------|-------|------------|
| `SecretError::NotFound(key)` | Key not found in any configured store | Check the secret exists in Vault/env; verify key path and field name |
| `SecretError::AccessDenied(msg)` | Vault returned 403 (bad token or policy) | Check `VAULT_TOKEN` has read access to the secret path |
| `SecretError::StoreUnavailable(msg)` | Vault unreachable, wrapping token expired/used, or malformed response | Check `VAULT_ADDR` is reachable; wrapping tokens are single-use and TTL-bound |

The executor logs these errors as `ExecutorError::SecretResolutionFailed` and reports a `failed` status back to the engine. The engine's retry logic (if configured) will re-wrap and re-submit.

## Testing

```bash
# Unit tests (no external dependencies)
cargo test

# Integration tests against real Vault (requires Docker)
cargo test --features vault -- --test-threads=1
```

Integration tests in `tests/vault_integration.rs` use [testcontainers](https://crates.io/crates/testcontainers) to spin up a real Vault instance and test:
- KV v2 read/write with field extraction
- Wrap and unwrap round-trip
- Single-use enforcement (second unwrap fails)
- Unwrap without `VAULT_TOKEN` (only wrapping token needed)
- TTL expiry
- Full resolution pipeline (Vault KV + resolve + wrap + unwrap + resolve)

## Contributing

Issues and pull requests are welcome. Please open an issue to discuss substantial changes before starting work.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work shall be licensed as Apache-2.0, without any additional terms or conditions.
