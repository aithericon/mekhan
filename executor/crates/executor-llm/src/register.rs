//! Register-as-pool client for aithericon-executor.
//!
//! Sub-phase 2.2 C7: ports the register-on-boot flow from the deleted
//! `cloud-layer/cloud-layer-pool-ollama/src/register.rs` into the executor.
//! After C7, each `aithericon-executor` instance registers itself directly
//! with `cloud-layer-capability-routing` as a `compute_pool` row — no
//! intermediate "pool service" tier.
//!
//! Flow:
//!
//! 1. Executor mints an HS256 `platform_admin` JWT from `secret` with
//!    `tenant_id` from config (default canonical dev tenant
//!    `00000000-0000-0000-0000-000000000001`).
//! 2. Executor POSTs `RegisterRequest` (its current hardware + engines +
//!    initial loaded_models + services) to `/v1/pools/register` with the
//!    JWT in `Authorization: Bearer <jwt>`.
//! 3. Server assigns a `pool_id` (or returns the existing one via
//!    `ON CONFLICT (tenant_id, pool_name) DO UPDATE`), mints a fresh
//!    bearer token, persists its SHA-256 hash, and returns
//!    `{ pool_id, heartbeat_token }`.
//! 4. Executor persists `(pool_id, heartbeat_token)` in memory for the
//!    duration of this process and passes the token into the heartbeat
//!    loop ([`crate::heartbeat`]). Token re-rotates on next boot via
//!    `ON CONFLICT`.
//!
//! **Vanilla-capability avoidance** (per the routing-ambiguity workaround
//! agreed at the C7 dispatch): the executor registers with `services.ollama
//! .models_loaded = []` — even when Ollama is alive, no models are pulled
//! at fresh boot. Capability-routing's resolver grants `Vanilla` only when
//! `services.ollama.models_loaded.is_empty()` is false (see
//! `cloud-layer-capability-routing/src/capability_resolver.rs::resolve_
//! capabilities`), so an empty list means the executor pool is NOT a
//! candidate for `Vanilla` routes during the 2.1a cert. The synthetic
//! `vanilla-synth` pool (registered by the cert script with
//! `models_loaded: ["synth-model"]`) remains the unambiguous Vanilla
//! winner. Once an operator pre-warms a model via Ollama's `/api/pull`,
//! the heartbeat probe will surface it and the executor will start
//! advertising Vanilla — that's the production path; this empty-default
//! is the boot-time honesty.
//!
//! Fail-closed: any register failure (network, auth, non-2xx) returns Err
//! and the caller (`executor-service` `main`) exits non-zero. NOT
//! fall-through-to-heartbeat-without-token (that's the silent-failure
//! pattern from `feedback_act2_certification_is_tier_scoped`).

use anyhow::Context;
use chrono::Utc;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::hardware_probe::HardwareAdvertisement;

/// Stable per-host pool name. Operator-overridable via
/// `AITHERICON_EXECUTOR_POOL_NAME`; default is `${hostname}-executor` per
/// A4 § 3.1 (one executor per host; `${hostname}-executor` is the stable
/// identity, consistent with `cloud-layer-compute-agent`'s
/// `${hostname}-agent` pattern). Stable across restarts so re-register
/// hits the `ON CONFLICT (tenant_id, pool_name) DO UPDATE` path rather
/// than creating a new row + leaving the old one as stale fixture.
pub fn default_pool_name() -> String {
    if let Ok(explicit) = std::env::var("AITHERICON_EXECUTOR_POOL_NAME") {
        return explicit;
    }
    let host = gethostname::gethostname().to_string_lossy().to_string();
    format!("{host}-executor")
}

/// Default tenant identity used when minting the platform_admin JWT.
/// Mirrors the legacy `CLOUD_LAYER_POOL_TENANT_ID` convention; default is
/// the canonical dev tenant `00000000-0000-0000-0000-000000000001`.
pub fn default_pool_tenant_id() -> String {
    std::env::var("AITHERICON_EXECUTOR_POOL_TENANT_ID")
        .unwrap_or_else(|_| "00000000-0000-0000-0000-000000000001".to_string())
}

/// Requester role embedded in the JWT claims. `platform_admin` matches the
/// auth model documented in capability-routing's `main.rs`: pool
/// registration is an admin endpoint requiring platform_admin role.
pub fn default_requester_role() -> String {
    std::env::var("AITHERICON_EXECUTOR_POOL_REQUESTER_ROLE")
        .unwrap_or_else(|_| "platform_admin".to_string())
}

/// Build the engine-advertisement Vec used in BOTH register + heartbeat.
/// Extracted as a shared helper so the two code paths can never drift on
/// what engine versions/caps are advertised. The "0.x" version placeholder
/// is replaced at heartbeat time by `crate::heartbeat::probe_ollama_version`.
pub fn build_engines_advertisement(engine_capabilities: &[String]) -> serde_json::Value {
    serde_json::json!([{
        "kind": "Ollama",
        "version": "0.x",
        "capabilities": engine_capabilities,
    }])
}

/// HS256 service-account JWT claims. Mirrors
/// `cloud_layer_common::auth::ServiceAccountClaims` byte-for-byte (the
/// capability-routing register handler decodes via that struct).
#[derive(Debug, Serialize)]
struct PoolJwtClaims {
    tenant_id: Uuid,
    requester_role: String,
    exp: i64,
    iat: i64,
}

/// Mint a fresh HS256 JWT for a single register call. Mint-on-demand: the
/// 5-min TTL otherwise silently fails after the first 5 minutes.
pub fn mint_register_jwt(
    secret: &str,
    tenant_id: Uuid,
    requester_role: &str,
) -> anyhow::Result<String> {
    let now = Utc::now().timestamp();
    let claims = PoolJwtClaims {
        tenant_id,
        requester_role: requester_role.to_string(),
        iat: now,
        exp: now + 300,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .context("encode HS256 register JWT")
}

/// Wire shape of `POST /v1/pools/register` — kept in lock-step with
/// `cloud_layer_capability_routing::types::RegisterPoolRequest`. Maintaining
/// a local copy here (rather than adding `cloud-layer-capability-routing`
/// as a runtime dep) preserves the cross-repo isolation per A4 § 5.2 trip-
/// wire and Q6=A.
#[derive(Debug, Serialize)]
pub struct RegisterRequest {
    pub pool_name: String,
    pub pool_url: String,
    pub hardware: serde_json::Value,
    pub gpus: Vec<serde_json::Value>,
    pub engines: serde_json::Value,
    pub loaded_models: Vec<String>,
    pub services: serde_json::Value,
}

/// Wire shape of the register response. The `heartbeat_token` is plaintext
/// and is returned exactly once — executor persists it in memory and
/// sends it on every subsequent heartbeat as `Authorization: Bearer …`.
#[derive(Debug, Deserialize)]
pub struct RegisterResponse {
    pub pool_id: Uuid,
    pub heartbeat_token: String,
}

/// POST `/v1/pools/register` with the JWT in `Authorization: Bearer`,
/// parse the response, return `(pool_id, heartbeat_token)`. Fail-closed on
/// any network / non-2xx / parse error — caller exits non-zero.
pub async fn register_on_boot(
    capability_routing_url: &str,
    jwt: &str,
    request: &RegisterRequest,
) -> anyhow::Result<RegisterResponse> {
    let url = format!("{capability_routing_url}/v1/pools/register");
    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(jwt)
        .json(request)
        .send()
        .await
        .with_context(|| format!("POST {url} failed (network)"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("register returned non-success: {status} — body: {body}");
    }

    let parsed: RegisterResponse = resp
        .json()
        .await
        .context("parse RegisterResponse from /v1/pools/register")?;

    anyhow::ensure!(
        !parsed.heartbeat_token.is_empty(),
        "server returned empty heartbeat_token — register failed",
    );

    Ok(parsed)
}

/// Derive engine-capability strings to advertise from probed hardware. The
/// executor advertises what Ollama actually supports on this hardware, not
/// a hardcoded list. Matches the legacy `engine_caps_for_hardware` from
/// `cloud-layer-pool-ollama/src/main.rs:265`.
pub fn engine_caps_for_hardware(hw: &HardwareAdvertisement) -> Vec<String> {
    let mut caps = vec![
        "GgufQuantization".to_string(),
        "VisionMultimodal".to_string(),
        "Streaming".to_string(),
    ];
    match hw {
        HardwareAdvertisement::Metal { .. } => caps.push("MetalAcceleration".to_string()),
        HardwareAdvertisement::Cuda { .. } => caps.push("CudaAcceleration".to_string()),
        HardwareAdvertisement::Rocm { .. } => caps.push("RocmAcceleration".to_string()),
        HardwareAdvertisement::Cpu { .. } => {}
    }
    caps
}

/// Build the `RegisterRequest` for a freshly-booted executor. Used by the
/// service `main()` and by the integration test.
///
/// Per § module-doc: `services.ollama.models_loaded` defaults to empty so
/// the executor pool does NOT compete with synthetic `vanilla-synth` for
/// `Vanilla` routes during 2.1a cert. Once operators pre-warm a model,
/// heartbeat probes will start surfacing it.
pub fn build_register_request(
    pool_name: String,
    pool_url: String,
    hardware: &HardwareAdvertisement,
    engine_capabilities: &[String],
    ollama_url: String,
    loaded_models: Vec<String>,
) -> RegisterRequest {
    let services = serde_json::json!({
        "ollama": {
            "url": ollama_url,
            "models_loaded": loaded_models,
        }
    });
    RegisterRequest {
        pool_name,
        pool_url,
        hardware: serde_json::to_value(hardware).expect("hardware serialisation"),
        gpus: vec![],
        engines: build_engines_advertisement(engine_capabilities),
        loaded_models: vec![],
        services,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// pool_name defaults to `${hostname}-executor` when no override is
    /// set, and honors `AITHERICON_EXECUTOR_POOL_NAME` literally when
    /// set. Both branches asserted in ONE test (rather than two) to
    /// avoid the test-thread race that arises from concurrent env
    /// mutation — per `feedback_test_discipline` we'd rather refactor to
    /// take override-as-parameter, but the env-reading is intrinsic to
    /// the dev convention and a single-test sequential probe is the
    /// minimum-pain fix.
    #[test]
    fn default_pool_name_default_and_override_paths() {
        let prior = std::env::var("AITHERICON_EXECUTOR_POOL_NAME").ok();

        // 1. Default path: no override → ${hostname}-executor.
        std::env::remove_var("AITHERICON_EXECUTOR_POOL_NAME");
        let name = default_pool_name();
        assert!(
            name.ends_with("-executor"),
            "default pool_name ends with -executor, got {name}"
        );

        // 2. Override path: env literal wins.
        std::env::set_var("AITHERICON_EXECUTOR_POOL_NAME", "test-override-pool");
        let overridden = default_pool_name();
        assert_eq!(overridden, "test-override-pool");

        // Restore prior env (cross-test isolation).
        match prior {
            Some(p) => std::env::set_var("AITHERICON_EXECUTOR_POOL_NAME", p),
            None => std::env::remove_var("AITHERICON_EXECUTOR_POOL_NAME"),
        }
    }

    #[test]
    fn mint_register_jwt_round_trips() {
        let secret = "test-secret";
        let tenant_id = Uuid::new_v4();
        let jwt = mint_register_jwt(secret, tenant_id, "platform_admin")
            .expect("jwt minting succeeds with valid secret");
        // JWT is a 3-segment dot-separated string (header.payload.signature).
        assert_eq!(
            jwt.split('.').count(),
            3,
            "minted token is a 3-segment JWT, got {jwt}"
        );
    }

    #[test]
    fn engine_caps_for_metal_includes_metal_acceleration() {
        let hw = HardwareAdvertisement::Metal {
            unified_memory_gb: 128,
        };
        let caps = engine_caps_for_hardware(&hw);
        assert!(caps.contains(&"MetalAcceleration".to_string()));
        assert!(caps.contains(&"GgufQuantization".to_string()));
        assert!(caps.contains(&"VisionMultimodal".to_string()));
        assert!(caps.contains(&"Streaming".to_string()));
    }

    #[test]
    fn build_register_request_advertises_empty_models_to_avoid_vanilla_ambiguity() {
        let hw = HardwareAdvertisement::Metal {
            unified_memory_gb: 64,
        };
        let req = build_register_request(
            "test-host-executor".to_string(),
            "http://127.0.0.1:3301".to_string(),
            &hw,
            &["GgufQuantization".to_string(), "Streaming".to_string()],
            "http://127.0.0.1:11436".to_string(),
            vec![], // empty — boot honesty
        );
        // Top-level loaded_models stays empty regardless of caller intent —
        // the field is dropped from the wire body because capability-routing
        // sources Vanilla from services.ollama.models_loaded only.
        assert!(
            req.loaded_models.is_empty(),
            "loaded_models defaults empty"
        );
        // services.ollama.models_loaded explicitly empty so capability-
        // routing's resolver does NOT grant Vanilla — see module-doc § Vanilla-
        // capability avoidance.
        let svc = &req.services["ollama"];
        assert_eq!(svc["url"], "http://127.0.0.1:11436");
        let models = svc["models_loaded"]
            .as_array()
            .expect("models_loaded is an array");
        assert!(
            models.is_empty(),
            "Vanilla-ambiguity workaround: services.ollama.models_loaded MUST default empty at fresh boot"
        );
    }

    #[tokio::test]
    async fn register_on_boot_network_failure_surfaces_as_err() {
        // Unbound port; fail-closed expectation.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);

        let req = build_register_request(
            "test-host-executor".to_string(),
            format!("http://{addr}"),
            &HardwareAdvertisement::Cpu { cores: 4 },
            &["Streaming".to_string()],
            "http://127.0.0.1:11436".to_string(),
            vec![],
        );
        let err = register_on_boot(&format!("http://{addr}"), "fake-jwt", &req)
            .await
            .expect_err("register against an unbound port fails");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("network") || msg.contains("connect") || msg.contains("Connect"),
            "error mentions a network failure, got: {msg}"
        );
    }
}
