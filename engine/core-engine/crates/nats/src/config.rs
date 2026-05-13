//! NATS configuration from environment variables.

use std::time::Duration;

use config::{Config, Environment};
use serde::Deserialize;

/// Intermediate struct for config-rs deserialization.
///
/// Duration fields are represented as milliseconds (`u64`) and converted
/// to `std::time::Duration` when constructing `NatsConfig`.
#[derive(Deserialize)]
struct NatsConfigRaw {
    #[serde(default = "defaults::url")]
    url: String,
    #[serde(default)]
    creds: Option<String>,
    #[serde(default = "defaults::connection_name")]
    connection_name: String,
    #[serde(default)]
    jetstream_domain: Option<String>,
    #[serde(default = "defaults::ping_interval_secs")]
    ping_interval_secs: u16,
    #[serde(default = "defaults::connection_timeout_ms")]
    connection_timeout_ms: u64,
    #[serde(default = "defaults::request_timeout_ms")]
    request_timeout_ms: u64,
    #[serde(default = "defaults::max_retries")]
    max_retries: u32,
    #[serde(default = "defaults::retry_base_delay_ms")]
    retry_base_delay_ms: u64,
    #[serde(default = "defaults::circuit_breaker_threshold")]
    circuit_breaker_threshold: u32,
    #[serde(default = "defaults::circuit_breaker_reset_ms")]
    circuit_breaker_reset_ms: u64,
}

mod defaults {
    pub fn url() -> String {
        "nats://localhost:4333".to_string()
    }
    pub fn connection_name() -> String {
        "petri-engine".to_string()
    }
    pub fn ping_interval_secs() -> u16 {
        20
    }
    pub fn connection_timeout_ms() -> u64 {
        10_000
    }
    pub fn request_timeout_ms() -> u64 {
        10_000
    }
    pub fn max_retries() -> u32 {
        3
    }
    pub fn retry_base_delay_ms() -> u64 {
        100
    }
    pub fn circuit_breaker_threshold() -> u32 {
        5
    }
    pub fn circuit_breaker_reset_ms() -> u64 {
        30_000
    }
}

/// NATS connection configuration.
///
/// Reads from environment variables with sensible defaults.
#[derive(Clone, Debug)]
pub struct NatsConfig {
    /// NATS server URL (e.g., "nats://localhost:4333")
    pub url: String,

    /// Path to NATS credentials file (.creds) for authenticated connections.
    pub creds: Option<String>,

    /// Connection name for monitoring (shows up in NATS server logs)
    pub connection_name: String,

    /// JetStream domain (optional, for multi-tenant setups)
    pub jetstream_domain: Option<String>,

    /// PING interval for keepalive (must be shorter than LB idle timeout)
    pub ping_interval: Duration,

    /// TCP connect timeout
    pub connection_timeout: Duration,

    /// Request-reply timeout
    pub request_timeout: Duration,

    /// Maximum retry attempts for publishing
    pub max_retries: u32,

    /// Base delay for retry backoff (doubles each attempt)
    pub retry_base_delay: Duration,

    /// Circuit breaker: number of failures before opening
    pub circuit_breaker_threshold: u32,

    /// Circuit breaker: time before attempting to close again
    pub circuit_breaker_reset: Duration,

    /// This engine's net identity (for bridge publishing as source_net_id)
    pub net_id: Option<String>,
}

impl Default for NatsConfig {
    fn default() -> Self {
        Self {
            url: "nats://localhost:4333".to_string(),
            creds: None,
            connection_name: "petri-engine".to_string(),
            jetstream_domain: None,
            ping_interval: Duration::from_secs(20),
            connection_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(10),
            max_retries: 3,
            retry_base_delay: Duration::from_millis(100),
            circuit_breaker_threshold: 5,
            circuit_breaker_reset: Duration::from_secs(30),
            net_id: None,
        }
    }
}

impl NatsConfig {
    /// Create configuration from environment variables.
    ///
    /// Uses `config-rs` with the `NATS_` prefix.
    ///
    /// ## Environment Variables
    ///
    /// - `NATS_URL` - Server URL (default: "nats://localhost:4333")
    /// - `NATS_CONNECTION_NAME` - Connection name (default: "petri-engine")
    /// - `NATS_JETSTREAM_DOMAIN` - JetStream domain (optional)
    /// - `NATS_PING_INTERVAL_SECS` - Keepalive ping interval in seconds (default: 20)
    /// - `NATS_CONNECTION_TIMEOUT_MS` - TCP connect timeout in ms (default: 10000)
    /// - `NATS_REQUEST_TIMEOUT_MS` - Request-reply timeout in ms (default: 10000)
    /// - `NATS_MAX_RETRIES` - Max publish retries (default: 3)
    /// - `NATS_RETRY_BASE_DELAY_MS` - Base retry delay in ms (default: 100)
    /// - `NATS_CIRCUIT_BREAKER_THRESHOLD` - Failures before circuit opens (default: 5)
    /// - `NATS_CIRCUIT_BREAKER_RESET_MS` - Circuit reset time in ms (default: 30000)
    /// - `NET_ID` - Engine net identity for cross-net bridging (optional)
    pub fn from_env() -> Self {
        let raw: NatsConfigRaw = Config::builder()
            .add_source(Environment::with_prefix("NATS").try_parsing(true))
            .build()
            .expect("failed to build NATS configuration")
            .try_deserialize()
            .expect("failed to deserialize NATS configuration");

        NatsConfig {
            url: raw.url,
            creds: raw.creds,
            connection_name: raw.connection_name,
            jetstream_domain: raw.jetstream_domain,
            ping_interval: Duration::from_secs(raw.ping_interval_secs as u64),
            connection_timeout: Duration::from_millis(raw.connection_timeout_ms),
            request_timeout: Duration::from_millis(raw.request_timeout_ms),
            max_retries: raw.max_retries,
            retry_base_delay: Duration::from_millis(raw.retry_base_delay_ms),
            circuit_breaker_threshold: raw.circuit_breaker_threshold,
            circuit_breaker_reset: Duration::from_millis(raw.circuit_breaker_reset_ms),
            net_id: std::env::var("NET_ID").ok().filter(|s| !s.is_empty()),
        }
    }

    /// Build tuned `ConnectOptions` with credentials, keepalive, timeouts,
    /// and an event callback for reconnect visibility.
    ///
    /// This is the single source of truth for connection tuning — used by
    /// both `connect()` and the executor NATS setup in `main.rs`.
    pub async fn build_options(&self) -> Result<async_nats::ConnectOptions, async_nats::ConnectError> {
        let base = if let Some(ref creds_path) = self.creds {
            let expanded = shellexpand::tilde(creds_path);
            tracing::info!(url = %self.url, name = %self.connection_name, creds = %expanded, "Building NATS options with credentials");
            async_nats::ConnectOptions::with_credentials_file(expanded.as_ref()).await?
        } else {
            tracing::info!(url = %self.url, name = %self.connection_name, "Building NATS options");
            async_nats::ConnectOptions::new()
        };

        let conn_name = self.connection_name.clone();
        Ok(base
            .ping_interval(self.ping_interval)
            .connection_timeout(self.connection_timeout)
            .request_timeout(Some(self.request_timeout))
            .event_callback(move |event| {
                let name = conn_name.clone();
                async move {
                    use async_nats::Event;
                    match event {
                        Event::Disconnected => tracing::warn!(conn = %name, "NATS disconnected"),
                        Event::Connected => tracing::info!(conn = %name, "NATS (re)connected"),
                        Event::SlowConsumer(n) => tracing::warn!(conn = %name, n, "NATS slow consumer"),
                        other => tracing::debug!(conn = %name, ?other, "NATS event"),
                    }
                }
            })
            .name(&self.connection_name))
    }

    /// Connect to NATS server.
    ///
    /// Returns a connected client ready for use.
    pub async fn connect(&self) -> Result<async_nats::Client, async_nats::ConnectError> {
        self.build_options().await?.connect(&self.url).await
    }

    /// Connect and create a JetStream context.
    ///
    /// JetStream provides durable, persistent messaging.
    pub async fn connect_jetstream(
        &self,
    ) -> Result<async_nats::jetstream::Context, async_nats::ConnectError> {
        let client = self.connect().await?;

        let jetstream = if let Some(ref domain) = self.jetstream_domain {
            async_nats::jetstream::with_domain(client, domain)
        } else {
            async_nats::jetstream::new(client)
        };

        Ok(jetstream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env-var-mutating tests must run sequentially to avoid races.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Helper: clear all NATS_ env vars to get clean defaults.
    fn clear_nats_env() {
        for key in [
            "NATS_URL",
            "NATS_CONNECTION_NAME",
            "NATS_JETSTREAM_DOMAIN",
            "NATS_PING_INTERVAL_SECS",
            "NATS_CONNECTION_TIMEOUT_MS",
            "NATS_REQUEST_TIMEOUT_MS",
            "NATS_MAX_RETRIES",
            "NATS_RETRY_BASE_DELAY_MS",
            "NATS_CIRCUIT_BREAKER_THRESHOLD",
            "NATS_CIRCUIT_BREAKER_RESET_MS",
            "NET_ID",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn test_default_config() {
        let config = NatsConfig::default();
        assert_eq!(config.url, "nats://localhost:4333");
        assert_eq!(config.connection_name, "petri-engine");
        assert_eq!(config.ping_interval, Duration::from_secs(20));
        assert_eq!(config.connection_timeout, Duration::from_secs(10));
        assert_eq!(config.request_timeout, Duration::from_secs(10));
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.retry_base_delay, Duration::from_millis(100));
        assert_eq!(config.circuit_breaker_threshold, 5);
        assert_eq!(config.circuit_breaker_reset, Duration::from_secs(30));
        assert!(config.jetstream_domain.is_none());
        assert!(config.net_id.is_none());
    }

    #[test]
    fn test_from_env_with_defaults() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_nats_env();

        let config = NatsConfig::from_env();
        assert_eq!(config.url, "nats://localhost:4333");
        assert_eq!(config.connection_name, "petri-engine");
        assert_eq!(config.ping_interval, Duration::from_secs(20));
        assert_eq!(config.connection_timeout, Duration::from_secs(10));
        assert_eq!(config.request_timeout, Duration::from_secs(10));
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.retry_base_delay, Duration::from_millis(100));
        assert_eq!(config.circuit_breaker_threshold, 5);
        assert_eq!(config.circuit_breaker_reset, Duration::from_secs(30));
        assert!(config.jetstream_domain.is_none());
        assert!(config.net_id.is_none());
    }

    #[test]
    fn test_from_env_custom_values() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_nats_env();

        std::env::set_var("NATS_URL", "nats://remote:4222");
        std::env::set_var("NATS_CONNECTION_NAME", "my-engine");
        std::env::set_var("NATS_JETSTREAM_DOMAIN", "hub");
        std::env::set_var("NATS_PING_INTERVAL_SECS", "30");
        std::env::set_var("NATS_CONNECTION_TIMEOUT_MS", "5000");
        std::env::set_var("NATS_REQUEST_TIMEOUT_MS", "15000");
        std::env::set_var("NATS_MAX_RETRIES", "5");
        std::env::set_var("NATS_RETRY_BASE_DELAY_MS", "200");
        std::env::set_var("NATS_CIRCUIT_BREAKER_THRESHOLD", "10");
        std::env::set_var("NATS_CIRCUIT_BREAKER_RESET_MS", "60000");
        std::env::set_var("NET_ID", "net-alpha");

        let config = NatsConfig::from_env();
        assert_eq!(config.url, "nats://remote:4222");
        assert_eq!(config.connection_name, "my-engine");
        assert_eq!(config.jetstream_domain.as_deref(), Some("hub"));
        assert_eq!(config.ping_interval, Duration::from_secs(30));
        assert_eq!(config.connection_timeout, Duration::from_millis(5000));
        assert_eq!(config.request_timeout, Duration::from_millis(15000));
        assert_eq!(config.max_retries, 5);
        assert_eq!(config.retry_base_delay, Duration::from_millis(200));
        assert_eq!(config.circuit_breaker_threshold, 10);
        assert_eq!(config.circuit_breaker_reset, Duration::from_secs(60));
        assert_eq!(config.net_id.as_deref(), Some("net-alpha"));

        clear_nats_env();
    }

    #[test]
    fn test_empty_net_id_is_none() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_nats_env();
        std::env::set_var("NET_ID", "");

        let config = NatsConfig::from_env();
        assert!(config.net_id.is_none());

        clear_nats_env();
    }
}
