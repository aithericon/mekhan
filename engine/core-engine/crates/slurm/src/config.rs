//! Slurm SSH configuration from environment variables.

use config::{Config, Environment};
use serde::Deserialize;

/// Intermediate struct for config-rs deserialization.
///
/// Field names map directly to `SLURM_SSH_*` and `SLURM_*` env vars
/// (the `SLURM_` prefix is stripped by config-rs).
#[derive(Deserialize)]
struct SlurmConfigRaw {
    ssh_host: String,
    #[serde(default = "defaults::ssh_port")]
    ssh_port: u16,
    ssh_user: String,
    #[serde(default = "defaults::ssh_key")]
    ssh_key: String,
    #[serde(default = "defaults::ssh_known_hosts")]
    ssh_known_hosts: String,
    #[serde(default = "defaults::poll_interval_secs")]
    poll_interval_secs: u64,
    #[serde(default = "defaults::template_dir")]
    template_dir: String,
    #[serde(default = "defaults::lookback_window_secs")]
    lookback_window_secs: u64,
    #[serde(default = "defaults::command_timeout_secs")]
    command_timeout_secs: u64,
}

mod defaults {
    pub fn ssh_port() -> u16 {
        22
    }
    pub fn ssh_key() -> String {
        "~/.ssh/id_rsa".to_string()
    }
    pub fn ssh_known_hosts() -> String {
        "strict".to_string()
    }
    pub fn poll_interval_secs() -> u64 {
        5
    }
    pub fn template_dir() -> String {
        "/opt/petri/templates".to_string()
    }
    pub fn lookback_window_secs() -> u64 {
        3600
    }
    pub fn command_timeout_secs() -> u64 {
        60
    }
}

/// Slurm SSH connection and polling configuration.
///
/// Reads from environment variables with `SLURM_` prefix.
///
/// ## Environment Variables
///
/// - `SLURM_SSH_HOST` — Login node hostname *(required, gate)*
/// - `SLURM_SSH_PORT` — SSH port (default: 22)
/// - `SLURM_SSH_USER` — SSH username *(required)*
/// - `SLURM_SSH_KEY` — Private key path (default: `~/.ssh/id_rsa`)
/// - `SLURM_SSH_KNOWN_HOSTS` — `strict` / `add` / `accept` (default: `strict`)
/// - `SLURM_POLL_INTERVAL_SECS` — Watcher poll frequency in seconds (default: 5)
/// - `SLURM_TEMPLATE_DIR` — Job script directory on login node (default: `/opt/petri/templates`)
/// - `SLURM_LOOKBACK_WINDOW_SECS` — sacct history window on first start (default: 3600)
/// - `SLURM_COMMAND_TIMEOUT_SECS` — Per-SSH-command timeout (default: 60). Caps how long a
///   single `sbatch`/`squeue`/`sacct` invocation can hang before the session is treated as
///   dead and the caller's reconnect path triggers (e.g. after laptop sleep kills the
///   ControlMaster master process).
#[derive(Clone, Debug)]
pub struct SlurmConfig {
    /// Login node hostname.
    pub ssh_host: String,
    /// SSH port (default: 22).
    pub ssh_port: u16,
    /// SSH username.
    pub ssh_user: String,
    /// Path to SSH private key.
    pub ssh_key: String,
    /// Known hosts check mode: `strict`, `add`, or `accept`.
    pub ssh_known_hosts: String,
    /// Watcher poll interval in seconds.
    pub poll_interval_secs: u64,
    /// Job script template directory on the login node.
    pub template_dir: String,
    /// How far back to query sacct on first start (seconds).
    pub lookback_window_secs: u64,
    /// Per-SSH-command timeout in seconds. On expiry, the command returns
    /// `SshError::Connection(Disconnected)` so the caller's reconnect path fires.
    pub command_timeout_secs: u64,
}

impl Default for SlurmConfig {
    fn default() -> Self {
        Self {
            ssh_host: "localhost".to_string(),
            ssh_port: 22,
            ssh_user: "petri".to_string(),
            ssh_key: "~/.ssh/id_rsa".to_string(),
            ssh_known_hosts: "strict".to_string(),
            poll_interval_secs: 5,
            template_dir: "/opt/petri/templates".to_string(),
            lookback_window_secs: 3600,
            command_timeout_secs: 60,
        }
    }
}

/// Resolved Slurm connection parameters parsed from a datacenter resource's
/// `effect_config` (NOT process env).
///
/// This is the multi-cluster analogue of [`SlurmConfig::from_env`]: the
/// `ClusterRegistry` parses the resolved effect_config into one of these and
/// hands it to [`SlurmConfig::from_connection`]. Only the connection-shaping
/// fields are carried; the polling/timeout knobs keep their defaults (the
/// resource models a *cluster connection*, not the watcher's cadence).
///
/// `ssh_key` is the inline PEM the caller has ALREADY written to a 0600 temp
/// file — pass that file PATH here (mirroring [`SlurmConfig::ssh_key`], which is
/// a path field, not key material). `None` falls back to the default key path.
#[derive(Clone, Debug, Default)]
pub struct SlurmConnectionParams {
    /// Login node hostname *(required)*.
    pub ssh_host: String,
    /// SSH port (default: 22 when `None`).
    pub ssh_port: Option<u16>,
    /// SSH username *(required)*.
    pub ssh_user: String,
    /// Path to the SSH private key the registry materialised from the inline
    /// PEM secret (default `~/.ssh/id_rsa` when `None`).
    pub ssh_key: Option<String>,
    /// Known-hosts check mode: `strict` / `add` / `accept` (default `strict`).
    pub ssh_known_hosts: Option<String>,
    /// Job-script template directory on the login node (default
    /// `/opt/petri/templates` when `None`).
    pub template_dir: Option<String>,
}

impl SlurmConfig {
    /// Build configuration from an explicit resolved connection (the datacenter
    /// resource's `effect_config`), NOT env.
    ///
    /// Connection-shaping fields come from `params`; the polling/timeout/lookback
    /// knobs keep [`SlurmConfig::default`] values (the resource describes a cluster
    /// connection, not the watcher cadence). `ssh_key` is the temp-file PATH the
    /// registry wrote the inline PEM to.
    pub fn from_connection(params: SlurmConnectionParams) -> Self {
        let defaults = SlurmConfig::default();
        SlurmConfig {
            ssh_host: params.ssh_host,
            ssh_port: params.ssh_port.unwrap_or(defaults.ssh_port),
            ssh_user: params.ssh_user,
            ssh_key: params.ssh_key.unwrap_or(defaults.ssh_key),
            ssh_known_hosts: params.ssh_known_hosts.unwrap_or(defaults.ssh_known_hosts),
            poll_interval_secs: defaults.poll_interval_secs,
            template_dir: params.template_dir.unwrap_or(defaults.template_dir),
            lookback_window_secs: defaults.lookback_window_secs,
            command_timeout_secs: defaults.command_timeout_secs,
        }
    }

    /// Create configuration from environment variables.
    ///
    /// Returns `None` if `SLURM_SSH_HOST` is not set, indicating Slurm is not configured.
    /// Uses `config-rs` with the `SLURM_` prefix.
    pub fn from_env() -> Option<Self> {
        // SLURM_SSH_HOST is the gate — if absent, Slurm is not configured.
        if std::env::var("SLURM_SSH_HOST").is_err() {
            return None;
        }

        let raw: SlurmConfigRaw = Config::builder()
            .add_source(Environment::with_prefix("SLURM").try_parsing(true))
            .build()
            .expect("failed to build Slurm configuration")
            .try_deserialize()
            .expect("failed to deserialize Slurm configuration");

        Some(SlurmConfig {
            ssh_host: raw.ssh_host,
            ssh_port: raw.ssh_port,
            ssh_user: raw.ssh_user,
            ssh_key: raw.ssh_key,
            ssh_known_hosts: raw.ssh_known_hosts,
            poll_interval_secs: raw.poll_interval_secs,
            template_dir: raw.template_dir,
            lookback_window_secs: raw.lookback_window_secs,
            command_timeout_secs: raw.command_timeout_secs,
        })
    }

    /// SSH destination string: `user@host`.
    pub fn destination(&self) -> String {
        format!("{}@{}", self.ssh_user, self.ssh_host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn clear_slurm_env() {
        for key in [
            "SLURM_SSH_HOST",
            "SLURM_SSH_PORT",
            "SLURM_SSH_USER",
            "SLURM_SSH_KEY",
            "SLURM_SSH_KNOWN_HOSTS",
            "SLURM_POLL_INTERVAL_SECS",
            "SLURM_TEMPLATE_DIR",
            "SLURM_LOOKBACK_WINDOW_SECS",
            "SLURM_COMMAND_TIMEOUT_SECS",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn test_default_config() {
        let config = SlurmConfig::default();
        assert_eq!(config.ssh_host, "localhost");
        assert_eq!(config.ssh_port, 22);
        assert_eq!(config.ssh_key, "~/.ssh/id_rsa");
        assert_eq!(config.ssh_known_hosts, "strict");
        assert_eq!(config.poll_interval_secs, 5);
        assert_eq!(config.template_dir, "/opt/petri/templates");
        assert_eq!(config.lookback_window_secs, 3600);
    }

    #[test]
    fn test_from_env_returns_none_without_host() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_slurm_env();

        assert!(SlurmConfig::from_env().is_none());
    }

    #[test]
    fn test_destination() {
        let config = SlurmConfig {
            ssh_user: "alice".to_string(),
            ssh_host: "slurm-login.cluster".to_string(),
            ..Default::default()
        };
        assert_eq!(config.destination(), "alice@slurm-login.cluster");
    }

    #[test]
    fn test_from_connection_full() {
        let config = SlurmConfig::from_connection(SlurmConnectionParams {
            ssh_host: "login.hpc".to_string(),
            ssh_port: Some(2222),
            ssh_user: "bob".to_string(),
            ssh_key: Some("/tmp/petri-key-abc".to_string()),
            ssh_known_hosts: Some("accept".to_string()),
            template_dir: Some("/scratch/petri/templates".to_string()),
        });
        assert_eq!(config.ssh_host, "login.hpc");
        assert_eq!(config.ssh_port, 2222);
        assert_eq!(config.ssh_user, "bob");
        assert_eq!(config.ssh_key, "/tmp/petri-key-abc");
        assert_eq!(config.ssh_known_hosts, "accept");
        assert_eq!(config.template_dir, "/scratch/petri/templates");
        // Watcher cadence/timeout knobs keep their defaults (not on the connection).
        assert_eq!(config.poll_interval_secs, 5);
        assert_eq!(config.lookback_window_secs, 3600);
        assert_eq!(config.command_timeout_secs, 60);
    }

    #[test]
    fn test_from_connection_defaults_optional_fields() {
        // Only the required fields supplied; the rest fall back to defaults.
        let config = SlurmConfig::from_connection(SlurmConnectionParams {
            ssh_host: "login.hpc".to_string(),
            ssh_user: "carol".to_string(),
            ..Default::default()
        });
        assert_eq!(config.ssh_host, "login.hpc");
        assert_eq!(config.ssh_user, "carol");
        assert_eq!(config.ssh_port, 22);
        assert_eq!(config.ssh_key, "~/.ssh/id_rsa");
        assert_eq!(config.ssh_known_hosts, "strict");
        assert_eq!(config.template_dir, "/opt/petri/templates");
    }
}
