//! Router configuration — defaults → optional TOML file → `ROUTER_*` env.
//!
//! The replica inventory is static for the MVP (doc 29 Router-MVP): a list of
//! upstream vLLM/Ollama-OpenAI replicas with their served models, residency
//! zone, and per-engine concurrency `C` (= vLLM `--max-num-seqs`). The live
//! poll of mekhan's capacity/fleet APIs (`inventory.rs`) is the soft-dep
//! upgrade deferred to doc 11 P2.

use anyhow::Context;
use serde::Deserialize;

/// Top-level router config.
#[derive(Debug, Clone, Deserialize)]
pub struct RouterConfig {
    /// `host:port` the router binds. Default `0.0.0.0:13200`.
    #[serde(default = "default_bind")]
    pub bind_addr: String,
    /// Tenant auth settings.
    #[serde(default)]
    pub auth: AuthSettings,
    /// NATS url for cancel subscribe + metering publish. When absent, those
    /// features are disabled (the router still routes + admits).
    #[serde(default)]
    pub nats_url: Option<String>,
    /// mekhan base url for the (deferred) live inventory poll.
    #[serde(default)]
    pub mekhan_url: Option<String>,
    /// Static replica inventory.
    #[serde(default)]
    pub replicas: Vec<ReplicaConfig>,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            bind_addr: default_bind(),
            auth: AuthSettings::default(),
            nats_url: None,
            mekhan_url: None,
            replicas: Vec::new(),
        }
    }
}

fn default_bind() -> String {
    "0.0.0.0:13200".to_string()
}

/// Tenant auth mode + the dev-noop fixed tenant.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthSettings {
    /// `dev_noop` (fixed tenant, no token required) or `bearer` (require an
    /// `Authorization: Bearer` token; real JWT verification deferred).
    #[serde(default = "default_auth_mode")]
    pub mode: String,
    /// The tenant attributed to every request (dev-noop) or the fallback
    /// tenant in bearer mode until token-claims extraction lands.
    #[serde(default = "default_tenant")]
    pub default_tenant: String,
}

impl Default for AuthSettings {
    fn default() -> Self {
        Self {
            mode: default_auth_mode(),
            default_tenant: default_tenant(),
        }
    }
}

fn default_auth_mode() -> String {
    "dev_noop".to_string()
}

fn default_tenant() -> String {
    "dev".to_string()
}

/// One upstream model-server replica.
#[derive(Debug, Clone, Deserialize)]
pub struct ReplicaConfig {
    /// Upstream OpenAI-compatible base url (e.g. `http://localhost:11434`).
    pub base_url: String,
    /// Model ids this replica serves (routing eligibility).
    #[serde(default)]
    pub model_ids: Vec<String>,
    /// GDPR residency zone this replica lives in. `None` = unconstrained.
    #[serde(default)]
    pub residency_zone: Option<String>,
    /// Per-engine concurrent-request budget (vLLM `--max-num-seqs`). The
    /// admission semaphore is sized to this.
    #[serde(default = "default_concurrency")]
    pub concurrency_c: usize,
    /// Optional upstream API key (forwarded as `Authorization: Bearer`).
    #[serde(default)]
    pub api_key: Option<String>,
}

fn default_concurrency() -> usize {
    8
}

impl RouterConfig {
    /// Load defaults, then an optional TOML file (`ROUTER_CONFIG`), then
    /// explicit `ROUTER_*` env overrides. Replicas come from `ROUTER_REPLICAS`
    /// (a JSON array) when set — env-friendly for `just`/docker.
    pub fn load() -> anyhow::Result<Self> {
        let mut cfg = if let Ok(path) = std::env::var("ROUTER_CONFIG") {
            config::Config::builder()
                .add_source(config::File::with_name(&path))
                .build()
                .context("building router config from ROUTER_CONFIG")?
                .try_deserialize()
                .context("deserializing router config file")?
        } else {
            RouterConfig::default()
        };

        if let Ok(v) = std::env::var("ROUTER_BIND_ADDR") {
            cfg.bind_addr = v;
        }
        if let Ok(v) = std::env::var("ROUTER_AUTH_MODE") {
            cfg.auth.mode = v;
        }
        if let Ok(v) = std::env::var("ROUTER_DEFAULT_TENANT") {
            cfg.auth.default_tenant = v;
        }
        if let Ok(v) = std::env::var("ROUTER_NATS_URL") {
            cfg.nats_url = Some(v);
        }
        if let Ok(v) = std::env::var("ROUTER_MEKHAN_URL") {
            cfg.mekhan_url = Some(v);
        }
        if let Ok(v) = std::env::var("ROUTER_REPLICAS") {
            cfg.replicas = serde_json::from_str(&v).context(
                "ROUTER_REPLICAS must be a JSON array of \
                 {base_url, model_ids, residency_zone?, concurrency_c?, api_key?}",
            )?;
        }
        Ok(cfg)
    }
}
