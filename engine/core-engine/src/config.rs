//! Top-level engine configuration from environment variables.

use config::{Config, Environment};
use serde::Deserialize;
use tracing::info;

/// Engine-level configuration loaded from un-prefixed environment variables.
///
/// ## Environment Variables
///
/// - `PORT` - HTTP server port (default: 3030)
/// - `NET_ID` - Engine net identity for consumer naming and cross-net bridge (optional)
/// - `SCHEDULER_BACKEND` - Scheduler backend: `mock` | `nomad` (optional, absent = disabled)
/// - `SCHEDULER_JOB_TEMPLATE` - Job template ID (default: `"default"`)
/// - `SCHEDULER_SIGNAL_PLACE` - Fallback signal place name (default: `"sig_compute"`)
/// - `SCHEDULER_SIGNAL_ROUTES` - Per-status signal routing CSV (optional)
/// - `EXECUTOR_ENABLED` - Enable executor integration: `true` (optional, absent = disabled)
/// - `EXECUTOR_SIGNAL_PLACE` - Executor fallback signal place name (default: `"sig_executor"`)
/// - `EXECUTOR_SIGNAL_ROUTES` - Per-status signal routing CSV (optional)
/// - `EXECUTOR_EVENT_ROUTES` - Per-category event routing CSV (optional)
/// - `EXECUTOR_NAMESPACE` - apalis-nats job namespace (default: `"executor_jobs"`)
#[derive(Debug, Clone, Deserialize)]
pub struct EngineConfig {
    #[serde(default = "defaults::port")]
    pub port: u16,
    #[serde(default)]
    pub net_id: Option<String>,
    #[serde(default)]
    pub scheduler_backend: Option<String>,
    #[serde(default = "defaults::scheduler_job_template")]
    pub scheduler_job_template: String,
    #[serde(default = "defaults::scheduler_signal_place")]
    #[allow(dead_code)] // used only with scheduler features (nomad, slurm)
    pub scheduler_signal_place: String,
    #[serde(default)]
    pub scheduler_signal_routes: Option<String>,
    /// Set to "true" to enable executor integration (requires `executor` feature).
    #[serde(default)]
    #[allow(dead_code)] // used only with executor feature
    pub executor_enabled: Option<String>,
    /// Fallback signal place for executor statuses not in `executor_signal_routes`.
    #[serde(default = "defaults::executor_signal_place")]
    #[allow(dead_code)] // used only with executor feature
    pub executor_signal_place: String,
    /// Per-status signal routing CSV for executor (e.g., `running:sig_running,completed:sig_completed`).
    #[serde(default)]
    #[allow(dead_code)] // used only with executor feature
    pub executor_signal_routes: Option<String>,
    /// Per-category event routing CSV for executor (e.g., `progress:sig_progress,artifact:sig_artifact`).
    #[serde(default)]
    #[allow(dead_code)] // used only with executor feature
    pub executor_event_routes: Option<String>,
    /// Executor apalis-nats namespace override.
    #[serde(default)]
    #[allow(dead_code)] // used only with executor feature
    pub executor_namespace: Option<String>,
    /// Set to "false" to disable schema validation globally.
    /// Default: enabled (true).
    #[serde(default)]
    pub petri_validate_schemas: Option<String>,
    /// Organization ID for human task routing to HPI.
    /// Fallback when token data does not include org_id.
    #[serde(default)]
    #[allow(dead_code)]
    pub human_org_id: Option<String>,
}

mod defaults {
    pub fn port() -> u16 {
        3030
    }
    pub fn scheduler_job_template() -> String {
        "default".to_string()
    }
    pub fn scheduler_signal_place() -> String {
        "sig_compute".to_string()
    }
    pub fn executor_signal_place() -> String {
        "sig_executor".to_string()
    }
}

impl EngineConfig {
    /// Load configuration from environment variables using `config-rs`.
    pub fn from_env() -> Self {
        let mut config: Self = Config::builder()
            .add_source(Environment::default())
            .build()
            .expect("failed to build engine configuration")
            .try_deserialize()
            .expect("failed to deserialize engine configuration");

        // Filter empty strings to None (preserve pre-config-rs behaviour)
        config.net_id = config.net_id.filter(|s| !s.is_empty());
        config.scheduler_backend = config.scheduler_backend.filter(|s| !s.is_empty());
        config.scheduler_signal_routes = config.scheduler_signal_routes.filter(|s| !s.is_empty());
        config.executor_enabled = config.executor_enabled.filter(|s| !s.is_empty());
        config.executor_signal_routes = config.executor_signal_routes.filter(|s| !s.is_empty());
        config.executor_event_routes = config.executor_event_routes.filter(|s| !s.is_empty());
        config.executor_namespace = config.executor_namespace.filter(|s| !s.is_empty());
        config.human_org_id = config.human_org_id.filter(|s| !s.is_empty());

        config
    }

    /// Convert the raw scheduler env vars into a `petri_api::SchedulerConfig`.
    ///
    /// Returns `None` if `SCHEDULER_BACKEND` is not set.
    pub fn build_scheduler_config(&self) -> Option<petri_api::SchedulerConfig> {
        let backend_str = self.scheduler_backend.as_deref()?;

        let backend = match backend_str.to_lowercase().as_str() {
            "mock" => petri_api::SchedulerBackend::Mock,

            #[cfg(feature = "nomad")]
            "nomad" => {
                let nomad_config = match petri_nomad::NomadConfig::from_env() {
                    Some(cfg) => cfg,
                    None => {
                        tracing::warn!(
                            "SCHEDULER_BACKEND=nomad but NOMAD_ADDR not set, scheduler disabled"
                        );
                        return None;
                    }
                };
                let signal_routes = parse_signal_routes(self.scheduler_signal_routes.as_deref());

                if !signal_routes.is_empty() {
                    info!(
                        routes = ?signal_routes,
                        fallback = %self.scheduler_signal_place,
                        "Per-status signal routing configured"
                    );
                }

                petri_api::SchedulerBackend::Nomad {
                    config: nomad_config,
                    fallback_place: self.scheduler_signal_place.clone(),
                    signal_routes,
                }
            }

            #[cfg(feature = "slurm")]
            "slurm" => {
                let slurm_config = match petri_slurm::SlurmConfig::from_env() {
                    Some(cfg) => cfg,
                    None => {
                        tracing::warn!(
                            "SCHEDULER_BACKEND=slurm but SLURM_SSH_HOST not set, scheduler disabled"
                        );
                        return None;
                    }
                };
                let signal_routes = parse_signal_routes(self.scheduler_signal_routes.as_deref());

                if !signal_routes.is_empty() {
                    info!(
                        routes = ?signal_routes,
                        fallback = %self.scheduler_signal_place,
                        "Per-status signal routing configured"
                    );
                }

                petri_api::SchedulerBackend::Slurm {
                    config: Box::new(slurm_config),
                    fallback_place: self.scheduler_signal_place.clone(),
                    signal_routes,
                }
            }

            other => {
                tracing::warn!(backend = %other, "Unknown SCHEDULER_BACKEND, scheduler disabled");
                return None;
            }
        };

        info!(
            backend = %backend_str,
            template = %self.scheduler_job_template,
            "Scheduler backend configured"
        );

        Some(petri_api::SchedulerConfig {
            backend,
            job_template_id: self.scheduler_job_template.clone(),
        })
    }

    /// Build an ExecutionConfig from environment settings.
    pub fn build_execution_config(&self) -> petri_application::ExecutionConfig {
        let disabled = self
            .petri_validate_schemas
            .as_deref()
            .map(|v| v.eq_ignore_ascii_case("false") || v == "0")
            .unwrap_or(false);

        if disabled {
            petri_application::ExecutionConfig {
                validate_output_schemas: false,
                validate_injection_schemas: false,
            }
        } else {
            petri_application::ExecutionConfig::default()
        }
    }

    /// Whether the executor integration is enabled.
    ///
    /// Requires `EXECUTOR_ENABLED=true` and the `executor` cargo feature.
    #[cfg(feature = "executor")]
    pub fn is_executor_enabled(&self) -> bool {
        self.executor_enabled
            .as_deref()
            .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
            .unwrap_or(false)
    }

    /// Build an `ExecutorIntegrationConfig` for the registry.
    ///
    /// Returns `None` if the executor is not enabled.
    /// The caller must provide NATS connection objects.
    #[cfg(feature = "executor")]
    pub fn build_executor_integration_config(
        &self,
        nats_client: async_nats::Client,
        jetstream: async_nats::jetstream::Context,
    ) -> Option<petri_api::ExecutorIntegrationConfig> {
        if !self.is_executor_enabled() {
            return None;
        }

        let signal_routes = parse_kv_csv(self.executor_signal_routes.as_deref());
        let event_routes = parse_kv_csv(self.executor_event_routes.as_deref());
        let namespace = self
            .executor_namespace
            .clone()
            .unwrap_or_else(|| "executor_jobs".to_string());

        if !signal_routes.is_empty() {
            info!(
                routes = ?signal_routes,
                fallback = %self.executor_signal_place,
                "Executor per-status signal routing configured"
            );
        }
        if !event_routes.is_empty() {
            info!(
                routes = ?event_routes,
                "Executor per-category event routing configured"
            );
        }

        info!(
            namespace = %namespace,
            fallback_place = %self.executor_signal_place,
            "Executor integration enabled"
        );

        #[allow(unused_mut)]
        let mut ecfg = petri_api::ExecutorIntegrationConfig {
            nats_client,
            jetstream,
            namespace,
            fallback_place: self.executor_signal_place.clone(),
            signal_routes,
            event_routes,
            #[cfg(feature = "executor-vault-secrets")]
            secret_store: None,
            #[cfg(feature = "executor-vault-secrets")]
            secret_wrapper: None,
        };

        #[cfg(feature = "executor-vault-secrets")]
        if let Some(vault_store) = aithericon_secrets::VaultSecretStore::from_env() {
            let store = std::sync::Arc::new(vault_store);
            ecfg.secret_store = Some(store.clone());
            ecfg.secret_wrapper = Some(store);
            info!("Vault secret wrapping enabled for executor submissions");
        }

        Some(ecfg)
    }

    /// Print the startup banner.
    pub fn print_startup_banner(&self) {
        println!("Digital Lab - Colored Petri Net Engine (Multi-Net)");
        println!("==================================================");
        println!();
        println!(
            "Engine starting empty - load scenarios via POST /api/nets/{{net_id}}/scenario"
        );
        println!();

        println!("NATS Integration: CONNECTED");
        println!("  - Events published to: petri.events.>");
        if let Some(ref net_id) = self.net_id {
            println!("  - Cross-net bridge: petri.bridge.{}.>", net_id);
        }
        println!();

        println!("API Server: http://0.0.0.0:{}", self.port);
        println!("  Net-scoped:   /api/nets/{{net_id}}/topology, ...");
        println!("  Metadata:     GET /api/nets/metadata");
        println!("Swagger UI: http://localhost:{}/swagger-ui", self.port);
        println!();
    }
}

/// Parse a `SCHEDULER_SIGNAL_ROUTES` value.
///
/// Format: `status:place,status:place,...`
/// Example: `running:sig_running,completed:sig_completed,failed:sig_failed`
#[cfg(any(feature = "nomad", feature = "slurm"))]
fn parse_signal_routes(raw: Option<&str>) -> std::collections::HashMap<String, String> {
    let raw = match raw {
        Some(v) if !v.is_empty() => v,
        _ => return std::collections::HashMap::new(),
    };

    let mut routes = std::collections::HashMap::new();
    for pair in raw.split(',') {
        let pair = pair.trim();
        if let Some((status, place)) = pair.split_once(':') {
            let status = status.trim().to_string();
            let place = place.trim().to_string();
            if !status.is_empty() && !place.is_empty() {
                if !petri_domain::JobStatus::ALL_NAMES.contains(&status.as_str()) {
                    tracing::warn!(
                        status = %status,
                        place = %place,
                        valid = ?petri_domain::JobStatus::ALL_NAMES,
                        "Unknown status in SCHEDULER_SIGNAL_ROUTES — may be a typo"
                    );
                }
                routes.insert(status, place);
            }
        } else {
            tracing::warn!(
                entry = %pair,
                "Invalid SCHEDULER_SIGNAL_ROUTES entry (expected 'status:place'), skipping"
            );
        }
    }
    routes
}

/// Parse a generic `key:value,key:value,...` CSV string into a HashMap.
///
/// Used for executor signal routes and event routes.
#[cfg(feature = "executor")]
fn parse_kv_csv(raw: Option<&str>) -> std::collections::HashMap<String, String> {
    let raw = match raw {
        Some(v) if !v.is_empty() => v,
        _ => return std::collections::HashMap::new(),
    };

    let mut routes = std::collections::HashMap::new();
    for pair in raw.split(',') {
        let pair = pair.trim();
        if let Some((key, value)) = pair.split_once(':') {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            if !key.is_empty() && !value.is_empty() {
                routes.insert(key, value);
            }
        } else {
            tracing::warn!(
                entry = %pair,
                "Invalid key:value entry in route config, skipping"
            );
        }
    }
    routes
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn clear_engine_env() {
        for key in [
            "PORT",
            "NET_ID",
            "SCHEDULER_BACKEND",
            "SCHEDULER_JOB_TEMPLATE",
            "SCHEDULER_SIGNAL_PLACE",
            "SCHEDULER_SIGNAL_ROUTES",
            "PETRI_VALIDATE_SCHEMAS",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn test_from_env_defaults() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_engine_env();

        let config = EngineConfig::from_env();
        assert_eq!(config.port, 3030);
        assert!(config.net_id.is_none());
        assert!(config.scheduler_backend.is_none());
        assert_eq!(config.scheduler_job_template, "default");
        assert_eq!(config.scheduler_signal_place, "sig_compute");
        assert!(config.scheduler_signal_routes.is_none());

        clear_engine_env();
    }

    #[test]
    fn test_from_env_custom_values() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_engine_env();

        std::env::set_var("PORT", "8080");
        std::env::set_var("NET_ID", "alpha");
        std::env::set_var("SCHEDULER_BACKEND", "mock");
        std::env::set_var("SCHEDULER_JOB_TEMPLATE", "my-template");
        std::env::set_var("SCHEDULER_SIGNAL_PLACE", "sig_custom");
        std::env::set_var("SCHEDULER_SIGNAL_ROUTES", "running:sig_run,failed:sig_fail");

        let config = EngineConfig::from_env();
        assert_eq!(config.port, 8080);
        assert_eq!(config.net_id.as_deref(), Some("alpha"));
        assert_eq!(config.scheduler_backend.as_deref(), Some("mock"));
        assert_eq!(config.scheduler_job_template, "my-template");
        assert_eq!(config.scheduler_signal_place, "sig_custom");
        assert_eq!(
            config.scheduler_signal_routes.as_deref(),
            Some("running:sig_run,failed:sig_fail")
        );

        clear_engine_env();
    }

    #[test]
    fn test_empty_net_id_is_none() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        std::env::set_var("NET_ID", "");

        let config = EngineConfig::from_env();
        assert!(config.net_id.is_none());

        clear_engine_env();
    }

    #[cfg(any(feature = "nomad", feature = "slurm"))]
    #[test]
    fn test_parse_signal_routes() {
        let routes = parse_signal_routes(Some("running:sig_running,completed:sig_completed"));
        assert_eq!(routes.len(), 2);
        assert_eq!(routes.get("running").unwrap(), "sig_running");
        assert_eq!(routes.get("completed").unwrap(), "sig_completed");
    }

    #[cfg(feature = "nomad")]
    #[test]
    fn test_parse_signal_routes_empty() {
        let routes = parse_signal_routes(None);
        assert!(routes.is_empty());

        let routes = parse_signal_routes(Some(""));
        assert!(routes.is_empty());
    }
}
