# ADR-10: Secure Secret Management for Effect Handlers

**Status:** Accepted (amended with Vault response wrapping)
**Date:** 2026-02-02
**Amended:** 2026-02-08
**Related:** 04-execution-rules.md

## Context

Effect handlers often require sensitive credentials (API tokens, SSH keys, database passwords) to interact with external systems.

Currently, secrets are:
1.  **Loaded from Environment Variables**: At engine startup (e.g., `NOMAD_TOKEN`, `SLURM_SSH_KEY`).
2.  **Embedded in Static Config**: Passed to the handler's struct during initialization.

This approach has significant limitations:
*   **Lack of Flexibility**: Secrets are static and process-wide. A single engine cannot easily manage multiple Nomad clusters with different tokens or support per-tenant credentials.
*   **Leakage Risk**: If a user attempts to pass a secret dynamically via a token or transition configuration, that secret becomes part of the **Event Log** (e.g., `TokenCreated`, `NetInitialized`). Since the event log is immutable and often widely readable (see global stream), this constitutes a permanent security breach.

## Decision

We will introduce a **Secret Resolution** phase in the execution of Effect Transitions. Secrets will never be stored in the Petri net state or event log; they will be resolved "just in time" by the engine and passed ephemerally to the handler.

### 1. The `SecretStore` Abstraction

A trait for retrieving secrets with pluggable backends (env vars, HashiCorp Vault, AWS Secrets Manager, Kubernetes Secrets).

```rust
#[async_trait]
pub trait SecretStore: Send + Sync {
    /// Resolve a single secret key to its plaintext value.
    async fn get(&self, key: &str) -> Result<String, SecretError>;
    /// Human-readable store name for diagnostics.
    fn name(&self) -> &str;
}
```

**Implemented backends** (in `aithericon-secrets`):

| Backend | Feature | Description |
|---------|---------|-------------|
| `EnvVarSecretStore` | (default) | Resolves from process environment variables |
| `VaultSecretStore` | `vault` | HashiCorp Vault KV v2 with field extraction and caching |
| `InMemorySecretStore` | (default) | HashMap-backed, used for unwrapped secrets |
| `ChainedSecretStore` | (default) | Tries multiple backends in order |

### 2. Secret References in Configuration

In the Petri net definition (Transition Config), users provide **Secret References** using `{{secret:KEY}}` syntax instead of plaintext values.

```rust
// Scenario Definition
ctx.transition("submit_job")
    .effect("nomad_submit")
    .config(json!({
        "endpoint": "https://nomad.lab-a.internal",
        "region": "us-west",
        "auth_token": "{{secret:nomad-lab-a-token}}"
    }));
```

For Vault, keys use `path#field` syntax: `{{secret:demo/api#key}}` reads the `key` field from Vault path `secret/data/demo/api`.

### 3. Execution Flow (Just-In-Time Resolution)

The `fire_effect_transition` logic in `crates/application/src/firing.rs` (line ~296):

1.  **Load Config**: Retrieve the transition's static `effect_config` (JSON).
2.  **Scan & Resolve**: Deep-scan via `aithericon_secrets::resolve_secrets()` for `{{secret:...}}` patterns.
3.  **Fetch**: Call `store.get("nomad-lab-a-token")` for each referenced key.
4.  **Inject**: Replace the reference with the actual secret value in a *transient* copy of the config.
5.  **Execute**: Pass this transient config to the `EffectHandler` via `EffectInput.config`.
6.  **Discard**: The transient config goes out of scope. The `EffectCompleted` event contains the *original* config (with the safe `{{secret:...}}` reference), NOT the resolved secret.

### 4. Handler Interface

The `EffectHandler` trait receives a `serde_json::Value` config with secrets already resolved. No handler code changes needed — resolution is transparent.

### 5. Preventing Leaks

*   **Event Log**: The `TransitionFired` and `EffectCompleted` events log the *original* configuration (with the reference), never the resolved value.
*   **Logging**: The `SecretStore` and resolution logic use `tracing` with `skip` fields to avoid printing secrets to stdout/logs.

## Amendment: Vault Response Wrapping (2026-02-08)

### Problem

The original design covers secret resolution within the engine process (effect handler configs). However, when the engine dispatches jobs to a **remote executor** via NATS JetStream, a new problem arises:

The job spec may contain `{{secret:KEY}}` refs that need resolving on the executor side. If the engine resolves secrets and sends plaintext values on NATS, any subscriber on the stream can read them. NATS JetStream messages are also persisted to disk.

### Solution: Cubbyhole Response Wrapping

We extend the pipeline with Vault's [response wrapping](https://developer.hashicorp.com/vault/docs/concepts/response-wrapping) (cubbyhole pattern). The engine wraps resolved secrets into a **single-use Vault wrapping token** before publishing to NATS. Only the opaque token travels on the wire — never plaintext secrets.

### New Traits and Functions

**`SecretWrapper` trait** (in `aithericon-secrets`, feature `vault`):

```rust
#[async_trait]
pub trait SecretWrapper: Send + Sync {
    /// Wrap a map of resolved secret key→value pairs into a single-use token.
    async fn wrap(
        &self,
        secrets: HashMap<String, String>,
        ttl_secs: u64,
    ) -> Result<String, SecretError>;
}
```

`VaultSecretStore` implements both `SecretStore` and `SecretWrapper`, so one instance serves both roles.

**`vault_unwrap_secrets()` standalone function**:

```rust
/// Unwrap a Vault wrapping token. Does NOT need VAULT_TOKEN — the wrapping
/// token itself is used as X-Vault-Token. Single-use: Vault invalidates
/// the token after this call.
pub async fn vault_unwrap_secrets(
    vault_addr: &str,
    wrapping_token: &str,
) -> Result<HashMap<String, String>, SecretError>;
```

### Execution Flow (Executor Integration)

```
Engine (petri-lab)                    NATS                     Executor
──────────────────                    ────                     ────────
1. Effect fires "executor_submit"
2. ExecutorNatsClient scans spec
   for {{secret:KEY}} refs
3. Resolves keys via SecretStore
   (Vault KV read)
4. Wraps resolved values via
   SecretWrapper::wrap() → "hvs.xxx"
5. Publishes to NATS:                 ──────────────────►
   - spec with UNRESOLVED refs                                6. InjectSecretsHook checks
   - wrapped_secrets: "hvs.xxx"                                  job.wrapped_secrets
   - NO plaintext secrets                                     7. vault_unwrap_secrets()
                                                                 → HashMap (token consumed)
                                                              8. Builds InMemorySecretStore
                                                              9. Resolves {{secret:KEY}} in
                                                                 spec.config and env
                                                             10. Process runs with secrets
                                                                 as environment variables
```

### Security Properties

| Property | Mechanism |
|----------|-----------|
| **Single-use** | Vault invalidates wrapping token after first unwrap (HTTP 400 on replay) |
| **TTL-bound** | Token expires after configurable TTL (default: 600s). HTTP 403 on expiry |
| **Minimal executor privilege** | Executor needs only `VAULT_ADDR`, not `VAULT_TOKEN` |
| **Wire safety** | NATS payload contains opaque `hvs.xxx` token, never plaintext secrets |
| **Event log safety** | `EffectCompleted` stores original unresolved `{{secret:KEY}}` refs |

### Implementation Locations

| Component | File | What Happens |
|-----------|------|--------------|
| Secret traits + resolution | `aithericon-secrets/src/` | `SecretStore`, `SecretWrapper`, `resolve_secrets()`, `vault_unwrap_secrets()` |
| Effect config resolution | `petri-application/src/firing.rs:296` | Just-in-time `resolve_secrets()` before passing config to `EffectHandler` |
| Job wrapping before NATS | `petri-executor/src/client.rs:300` | `extract_secret_keys()` → `store.get()` → `wrapper.wrap()` → `job.wrapped_secrets` |
| Engine config wiring | `core-engine/src/config.rs:253` | `VaultSecretStore::from_env()` → `set_secret_wrapping()` |
| Executor unwrapping | `executor-worker/src/staging.rs:322` | `InjectSecretsHook`: unwrap token → `InMemorySecretStore` → `resolve_secrets()` |
| Job field | `executor-domain/src/job.rs:50` | `ExecutionJob.wrapped_secrets: Option<String>` |

### Configuration

**Engine (petri-lab)**:

| Variable | Purpose |
|----------|---------|
| `VAULT_ADDR` | Vault server address |
| `VAULT_TOKEN` | Service token (needs `secret/data/*` read + `sys/wrapping/wrap` update) |

Feature: `cargo build -p core-engine --features executor,executor-vault-secrets`

**Executor (aithericon-executor)**:

| Variable | Purpose |
|----------|---------|
| `VAULT_ADDR` | Same Vault server. No `VAULT_TOKEN` needed. |

Feature: `cargo build -p aithericon-executor-service --features vault`

### Two-Layer Resolution

Secrets are resolved at two independent layers:

1. **Engine layer** (`firing.rs`): Resolves `{{secret:KEY}}` in `effect_config` before passing to effect handlers. This handles secrets needed by the engine itself (e.g., Nomad auth tokens).

2. **Executor layer** (`staging.rs`): Resolves `{{secret:KEY}}` in `spec.config` and `env` after unwrapping the Vault token. This handles secrets needed by the executed process.

Both layers use the same `resolve_secrets()` function and `{{secret:KEY}}` syntax. They operate independently — a job can use both (effect config has engine-side secrets, spec env has process-side secrets).

## Consequences

### Positive
*   **Security**: Secrets are never persisted in the event log or on NATS.
*   **Flexibility**: Secrets can be rotated without redeploying the engine or updating the scenario (just update the store).
*   **Multi-Tenancy**: The engine can resolve `{{secret:tenant-a-key}}` and `{{secret:tenant-b-key}}` dynamically based on the context.
*   **Minimal Executor Trust**: The executor never holds broad secret access — it only receives pre-scoped, single-use wrapping tokens.

### Negative
*   **Complexity**: Adds a resolution step to every effect firing, and a wrap/unwrap step for executor jobs.
*   **Latency**: Fetching secrets (if remote, e.g., Vault) adds latency to transition execution. Caching in `VaultSecretStore` mitigates this (default 5-minute TTL).
*   **Vault Dependency**: Response wrapping requires a running Vault instance. The feature is opt-in (`executor-vault-secrets` feature flag) and falls back gracefully: without Vault, refs pass through unresolved and the executor uses its local `SecretStore`.
*   **Syntax**: The `{{secret:...}}` pattern is a "magic string" that must be documented and parsed correctly.

## Migration

Existing hardcoded env-var configs in `NomadWatcher` etc. can remain for *infrastructure* (watching events), but *effect* secrets (submitting jobs) should use this model. The Vault wrapping feature is opt-in via feature flags and has no effect when `VAULT_ADDR`/`VAULT_TOKEN` are not set.
