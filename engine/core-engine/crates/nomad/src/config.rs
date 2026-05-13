//! Nomad configuration from environment variables.

use config::{Config, Environment};
use serde::Deserialize;

/// Intermediate struct for config-rs deserialization.
///
/// Field `cacert` maps to env var `NOMAD_CACERT` (no underscore), while
/// the public `NomadConfig` exposes it as `ca_cert`.
#[derive(Deserialize)]
struct NomadConfigRaw {
    addr: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default = "defaults::region")]
    region: String,
    #[serde(default = "defaults::task_name")]
    task_name: String,
    #[serde(default)]
    cacert: Option<String>,
}

mod defaults {
    pub fn region() -> String {
        "global".to_string()
    }
    pub fn task_name() -> String {
        "petri-worker".to_string()
    }
}

/// Filter empty strings to `None` (preserves existing behaviour where
/// `NOMAD_TOKEN=""` is treated as absent).
fn filter_empty(opt: Option<String>) -> Option<String> {
    opt.filter(|s| !s.is_empty())
}

/// Nomad connection and filtering configuration.
///
/// Reads from environment variables with sensible defaults for `-dev` mode.
///
/// ## Environment Variables
///
/// - `NOMAD_ADDR` - Nomad HTTP address (default: "http://localhost:4646")
/// - `NOMAD_TOKEN` - ACL token (optional, empty for `-dev` mode)
/// - `NOMAD_REGION` - Nomad region (default: "global")
/// - `NOMAD_TASK_NAME` - Task name to filter events on (default: "petri-worker")
/// - `NOMAD_CACERT` - Path to CA cert for TLS (optional)
#[derive(Clone, Debug)]
pub struct NomadConfig {
    /// Nomad HTTP address (e.g., "http://localhost:4646").
    pub addr: String,
    /// ACL token (optional for `-dev` mode).
    pub token: Option<String>,
    /// Nomad region (default: "global").
    pub region: String,
    /// Task name to filter allocation events on (default: "petri-worker").
    pub task_name: String,
    /// Optional CA cert path for TLS.
    pub ca_cert: Option<String>,
}

impl Default for NomadConfig {
    fn default() -> Self {
        Self {
            addr: "http://localhost:4646".to_string(),
            token: None,
            region: "global".to_string(),
            task_name: "petri-worker".to_string(),
            ca_cert: None,
        }
    }
}

impl NomadConfig {
    /// Create configuration from environment variables.
    ///
    /// Returns `None` if `NOMAD_ADDR` is not set, indicating Nomad is not configured.
    /// Uses `config-rs` with the `NOMAD_` prefix.
    pub fn from_env() -> Option<Self> {
        // NOMAD_ADDR is the gate — if absent, Nomad is not configured.
        if std::env::var("NOMAD_ADDR").is_err() {
            return None;
        }

        let raw: NomadConfigRaw = Config::builder()
            .add_source(Environment::with_prefix("NOMAD").try_parsing(true))
            .build()
            .expect("failed to build Nomad configuration")
            .try_deserialize()
            .expect("failed to deserialize Nomad configuration");

        Some(NomadConfig {
            addr: raw.addr,
            token: filter_empty(raw.token),
            region: raw.region,
            task_name: raw.task_name,
            ca_cert: filter_empty(raw.cacert),
        })
    }

    /// Build a `reqwest::Client` with the appropriate TLS and timeout settings.
    pub fn build_http_client(&self) -> Result<reqwest::Client, reqwest::Error> {
        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10));

        if let Some(ref ca_path) = self.ca_cert {
            match std::fs::read(ca_path) {
                Ok(cert_bytes) => match reqwest::Certificate::from_pem(&cert_bytes) {
                    Ok(cert) => {
                        builder = builder.add_root_certificate(cert);
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            path = %ca_path,
                            "Invalid PEM in CA cert — TLS root not added"
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %ca_path,
                        "Failed to read CA cert — TLS root not added"
                    );
                }
            }
        }

        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env-var-mutating tests must run sequentially to avoid races.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Helper: clear all NOMAD_ env vars to get clean defaults.
    fn clear_nomad_env() {
        for key in [
            "NOMAD_ADDR",
            "NOMAD_TOKEN",
            "NOMAD_REGION",
            "NOMAD_TASK_NAME",
            "NOMAD_CACERT",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn test_default_config() {
        let config = NomadConfig::default();
        assert_eq!(config.addr, "http://localhost:4646");
        assert_eq!(config.region, "global");
        assert_eq!(config.task_name, "petri-worker");
        assert!(config.token.is_none());
        assert!(config.ca_cert.is_none());
    }

    #[test]
    fn test_from_env_returns_none_without_addr() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_nomad_env();

        assert!(NomadConfig::from_env().is_none());
    }

    #[test]
    fn test_from_env_with_addr() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_nomad_env();
        std::env::set_var("NOMAD_ADDR", "http://nomad.local:4646");

        let config = NomadConfig::from_env().unwrap();
        assert_eq!(config.addr, "http://nomad.local:4646");
        assert!(config.token.is_none());
        assert_eq!(config.region, "global");
        assert_eq!(config.task_name, "petri-worker");
        assert!(config.ca_cert.is_none());

        clear_nomad_env();
    }

    #[test]
    fn test_from_env_custom_values() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_nomad_env();
        std::env::set_var("NOMAD_ADDR", "https://nomad.prod:4646");
        std::env::set_var("NOMAD_TOKEN", "secret-token");
        std::env::set_var("NOMAD_REGION", "us-east-1");
        std::env::set_var("NOMAD_TASK_NAME", "custom-worker");
        std::env::set_var("NOMAD_CACERT", "/etc/ssl/nomad-ca.pem");

        let config = NomadConfig::from_env().unwrap();
        assert_eq!(config.addr, "https://nomad.prod:4646");
        assert_eq!(config.token.as_deref(), Some("secret-token"));
        assert_eq!(config.region, "us-east-1");
        assert_eq!(config.task_name, "custom-worker");
        assert_eq!(config.ca_cert.as_deref(), Some("/etc/ssl/nomad-ca.pem"));

        clear_nomad_env();
    }

    #[test]
    fn test_from_env_empty_token_is_none() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_nomad_env();
        std::env::set_var("NOMAD_ADDR", "http://localhost:4646");
        std::env::set_var("NOMAD_TOKEN", "");

        let config = NomadConfig::from_env().unwrap();
        assert!(config.token.is_none());

        clear_nomad_env();
    }

    #[test]
    fn test_from_env_empty_cacert_is_none() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_nomad_env();
        std::env::set_var("NOMAD_ADDR", "http://localhost:4646");
        std::env::set_var("NOMAD_CACERT", "");

        let config = NomadConfig::from_env().unwrap();
        assert!(config.ca_cert.is_none());

        clear_nomad_env();
    }

    #[test]
    fn test_build_http_client() {
        let config = NomadConfig::default();
        let client = config.build_http_client();
        assert!(client.is_ok());
    }
}
