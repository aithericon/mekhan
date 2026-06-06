//! Register-as-pool client for aithericon-executor-surya.
//!
//! Mirrors `aithericon_executor_llm::register` shape (sub-phase 2.2 C7
//! pattern). Substitutes Surya-specific details:
//!
//! - Pool name defaults to `${hostname}-executor-surya` (parallel to
//!   executor-llm's `${hostname}-executor`); distinct stable identity so
//!   cap-routing's `ON CONFLICT (tenant_id, pool_name)` semantics don't
//!   conflate the kreuzberg-pool and Surya-pool rows.
//! - `engines` advertisement uses `kind: "Surya"` (vs Ollama) so the
//!   cluster-status surface distinguishes pool flavours at a glance.
//! - `services.surya = { healthy: ... }` block — recognised by
//!   cap-routing's resolver (Item 5's cloud-layer-side change) to grant
//!   `Capability::Ocr`.
//! - NO `services.ollama` / `services.kreuzberg` — the Surya executor's
//!   only advertised capability surface is Surya OCR. Coexists with
//!   executor-llm's kreuzberg-flavored pool as a separate row;
//!   cap-routing's pick logic chooses between the two `Capability::Ocr`
//!   advertisements per-request.

use anyhow::Context;
use chrono::Utc;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::hardware_probe::HardwareAdvertisement;

/// Stable per-host pool name. Operator-overridable via
/// `AITHERICON_EXECUTOR_SURYA_POOL_NAME`; default
/// `${hostname}-executor-surya` per A4 § 3.1 convention.
pub fn default_pool_name() -> String {
    if let Ok(explicit) = std::env::var("AITHERICON_EXECUTOR_SURYA_POOL_NAME") {
        return explicit;
    }
    let host = gethostname::gethostname().to_string_lossy().to_string();
    format!("{host}-executor-surya")
}

/// Default tenant identity for the platform_admin JWT claim.
pub fn default_pool_tenant_id() -> String {
    std::env::var("AITHERICON_EXECUTOR_SURYA_POOL_TENANT_ID")
        .unwrap_or_else(|_| "00000000-0000-0000-0000-000000000001".to_string())
}

/// Requester role embedded in the JWT claims.
pub fn default_requester_role() -> String {
    std::env::var("AITHERICON_EXECUTOR_SURYA_POOL_REQUESTER_ROLE")
        .unwrap_or_else(|_| "platform_admin".to_string())
}

/// Build the engine-advertisement Vec used in BOTH register + heartbeat.
/// Surya is the only engine surface this pool exposes; the "version"
/// placeholder is replaced at heartbeat time by a probe of the wrapper's
/// `/health` endpoint's `device` field (rough proxy until a `/version`
/// surface is added).
pub fn build_engines_advertisement(engine_capabilities: &[String]) -> serde_json::Value {
    serde_json::json!([{
        "kind": "Surya",
        "version": "0.x",
        "capabilities": engine_capabilities,
    }])
}

/// HS256 service-account JWT claims — byte-for-byte mirror of
/// `cloud_layer_common::auth::ServiceAccountClaims`.
#[derive(Debug, Serialize)]
struct PoolJwtClaims {
    tenant_id: Uuid,
    requester_role: String,
    exp: i64,
    iat: i64,
}

/// Mint a fresh HS256 JWT for a single register call. Mint-on-demand —
/// the 5-min TTL otherwise silently fails after the first 5 minutes.
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
/// `cloud_layer_capability_routing::types::RegisterPoolRequest`.
#[derive(Debug, Serialize)]
pub struct RegisterRequest {
    pub pool_name: String,
    pub pool_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_url: Option<String>,
    pub hardware: serde_json::Value,
    pub gpus: Vec<serde_json::Value>,
    pub engines: serde_json::Value,
    pub loaded_models: Vec<String>,
    pub services: serde_json::Value,
    /// Workstream #122: dispatch backend kind this pool advertises
    /// ("http" / "python" / "surya" / "file_ops" / …). For the Surya OCR
    /// pool this is always `"surya"` — cap-routing persists it on the
    /// `compute_pools` row, threads it through `PickRouteResponse` →
    /// `cloud-layer-workflow::merge_enrichment` → enriched effect_config →
    /// mekhan executor token's `ExecutionSpec.backend`, where this crate's
    /// `SuryaBackend::supports` matches on `spec.backend == "surya"`.
    /// `#[serde(skip_serializing_if = "Option::is_none")]` preserves wire
    /// compat with pre-#122 cap-routing instances which `#[serde(default)]`
    /// the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_backend: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RegisterResponse {
    pub pool_id: Uuid,
    pub heartbeat_token: String,
}

/// POST `/v1/pools/register`; fail-closed on any non-2xx / parse error.
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

/// Derive engine-capability strings to advertise. Surya's strengths are
/// layout / table / formula detection on scanned documents; the strings
/// are operator-facing tags that capability-routing's pick algorithm can
/// weight against (analogous to executor-llm's
/// `GgufQuantization` / `VisionMultimodal` / `Streaming`).
pub fn engine_caps_for_hardware(hw: &HardwareAdvertisement) -> Vec<String> {
    let mut caps = vec![
        "OcrLayout".to_string(),
        "OcrTableDetection".to_string(),
        "OcrMultiLanguage".to_string(),
    ];
    match hw {
        HardwareAdvertisement::Metal { .. } => caps.push("MetalAcceleration".to_string()),
        HardwareAdvertisement::Cuda { .. } => caps.push("CudaAcceleration".to_string()),
        HardwareAdvertisement::Rocm { .. } => caps.push("RocmAcceleration".to_string()),
        HardwareAdvertisement::Cpu { .. } => {}
    }
    caps
}

/// Build the `RegisterRequest` for a freshly-booted Surya executor pool.
///
/// Wire-shape contract:
///
/// - `pool_name`, `pool_url`, `hardware`, `engines` always emitted.
/// - `services.surya = { healthy: surya_healthy }` always emitted —
///   `Capability::Ocr` advertisement depends on the resolver seeing this
///   block.
/// - `loaded_models` empty (Surya doesn't use the LLM model-catalog
///   pattern; OCR has no "loaded" concept at the cap-routing surface).
/// - `control_url = Some(pool_url)` per workstream #74 — pool_listener
///   serves `/v1/healthz` natively on `pool_url`, so the two URLs
///   coincide (matches executor-llm's executor-pool pattern; differs
///   from cloud-layer-compute-agent where pool_url points at an upstream
///   service and control_url points at a separate listener).
pub fn build_register_request(
    pool_name: String,
    pool_url: String,
    hardware: &HardwareAdvertisement,
    engine_capabilities: &[String],
    surya_healthy: bool,
) -> RegisterRequest {
    let services = serde_json::json!({
        "surya": { "healthy": surya_healthy },
    });
    let control_url = Some(pool_url.clone());
    RegisterRequest {
        pool_name,
        pool_url,
        control_url,
        hardware: serde_json::to_value(hardware).expect("hardware serialisation"),
        gpus: vec![],
        engines: build_engines_advertisement(engine_capabilities),
        loaded_models: vec![],
        services,
        // Workstream #122: the Surya pool dispatches OCR via its Surya
        // subprocess; mekhan's SuryaBackend matches `spec.backend == "surya"`.
        pool_backend: Some("surya".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// pool_name defaults to `${hostname}-executor-surya` and honors
    /// `AITHERICON_EXECUTOR_SURYA_POOL_NAME` override. Single-test
    /// sequential probe to avoid env-mutation race.
    #[test]
    fn default_pool_name_default_and_override_paths() {
        let prior = std::env::var("AITHERICON_EXECUTOR_SURYA_POOL_NAME").ok();
        std::env::remove_var("AITHERICON_EXECUTOR_SURYA_POOL_NAME");
        let name = default_pool_name();
        assert!(
            name.ends_with("-executor-surya"),
            "default pool_name ends with -executor-surya, got {name}"
        );
        // Honest-absence: pool name must NOT collide with executor-llm's
        // ${hostname}-executor (which ends with -executor, not -executor-surya).
        assert!(
            !name.ends_with("-executor") || name.ends_with("-executor-surya"),
            "Surya pool name MUST NOT collide with executor-llm's -executor suffix"
        );

        std::env::set_var(
            "AITHERICON_EXECUTOR_SURYA_POOL_NAME",
            "test-override-surya-pool",
        );
        let overridden = default_pool_name();
        assert_eq!(overridden, "test-override-surya-pool");

        match prior {
            Some(p) => std::env::set_var("AITHERICON_EXECUTOR_SURYA_POOL_NAME", p),
            None => std::env::remove_var("AITHERICON_EXECUTOR_SURYA_POOL_NAME"),
        }
    }

    #[test]
    fn mint_register_jwt_round_trips() {
        let secret = "test-secret";
        let tenant_id = Uuid::new_v4();
        let jwt = mint_register_jwt(secret, tenant_id, "platform_admin")
            .expect("jwt minting succeeds with valid secret");
        assert_eq!(jwt.split('.').count(), 3, "JWT is 3-segment, got {jwt}");
    }

    #[test]
    fn engine_caps_for_metal_includes_metal_acceleration() {
        let hw = HardwareAdvertisement::Metal {
            unified_memory_gb: 128,
        };
        let caps = engine_caps_for_hardware(&hw);
        assert!(caps.contains(&"MetalAcceleration".to_string()));
        assert!(caps.contains(&"OcrLayout".to_string()));
        assert!(caps.contains(&"OcrTableDetection".to_string()));
        assert!(caps.contains(&"OcrMultiLanguage".to_string()));
    }

    #[test]
    fn build_register_request_emits_surya_services_block() {
        let hw = HardwareAdvertisement::Metal {
            unified_memory_gb: 128,
        };
        let req = build_register_request(
            "test-host-executor-surya".to_string(),
            "http://127.0.0.1:3302".to_string(),
            &hw,
            &["OcrLayout".to_string()],
            true, // surya_healthy
        );
        assert_eq!(
            req.services["surya"]["healthy"], true,
            "services.surya.healthy MUST be true when subprocess is healthy"
        );
        // Honest-absence: Surya executor must NOT advertise ollama /
        // kreuzberg / ocr_sidecar — those belong to other pool flavours.
        assert!(
            req.services.get("ollama").is_none(),
            "Surya executor MUST NOT advertise services.ollama"
        );
        assert!(
            req.services.get("kreuzberg").is_none(),
            "Surya executor MUST NOT advertise services.kreuzberg (separate pool)"
        );
        assert!(
            req.services.get("ocr_sidecar").is_none(),
            "Surya executor MUST NOT advertise legacy services.ocr_sidecar"
        );
        // control_url contract per workstream #74.
        assert_eq!(
            req.control_url.as_deref(),
            Some("http://127.0.0.1:3302"),
            "executor-surya's control_url defaults to pool_url"
        );
        // Workstream #122: pool_backend advertisement must be "surya" so
        // cap-routing threads it into ExecutionSpec.backend, where mekhan's
        // SuryaBackend::supports matches `spec.backend == "surya"`.
        assert_eq!(
            req.pool_backend.as_deref(),
            Some("surya"),
            "Surya pool MUST advertise pool_backend=\"surya\""
        );
    }

    #[test]
    fn build_register_request_emits_surya_healthy_false_when_subprocess_down() {
        let hw = HardwareAdvertisement::Cpu { cores: 4 };
        let req = build_register_request(
            "test-host-executor-surya".to_string(),
            "http://127.0.0.1:3302".to_string(),
            &hw,
            &["OcrLayout".to_string()],
            false, // subprocess down
        );
        assert_eq!(
            req.services["surya"]["healthy"], false,
            "services.surya.healthy MUST reflect probed state — false when subprocess down"
        );
    }

    #[tokio::test]
    async fn register_on_boot_network_failure_surfaces_as_err() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        let req = build_register_request(
            "test-host-executor-surya".to_string(),
            format!("http://{addr}"),
            &HardwareAdvertisement::Cpu { cores: 4 },
            &["OcrLayout".to_string()],
            true,
        );
        let err = register_on_boot(&format!("http://{addr}"), "fake-jwt", &req)
            .await
            .expect_err("register against unbound port fails");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("network") || msg.contains("connect") || msg.contains("Connect"),
            "error mentions a network failure, got: {msg}"
        );
    }
}
