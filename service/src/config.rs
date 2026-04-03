use config::{Config, Environment, File};
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub database_url: String,
    #[serde(default = "default_petri_lab_url")]
    pub petri_lab_url: String,
    #[serde(default = "default_nats_url")]
    pub nats_url: String,
    #[serde(default)]
    pub cleanup: CleanupConfig,
    #[serde(default)]
    pub s3: S3Config,
    #[serde(default)]
    pub artifact_s3: Option<S3Config>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct S3Config {
    #[serde(default = "default_s3_endpoint")]
    pub endpoint: String,
    #[serde(default = "default_s3_bucket")]
    pub bucket: String,
    #[serde(default)]
    pub access_key: String,
    #[serde(default)]
    pub secret_key: String,
    #[serde(default = "default_s3_region")]
    pub region: String,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            endpoint: default_s3_endpoint(),
            bucket: default_s3_bucket(),
            access_key: String::new(),
            secret_key: String::new(),
            region: default_s3_region(),
        }
    }
}

fn default_s3_endpoint() -> String {
    "http://localhost:9000".to_string()
}

fn default_s3_bucket() -> String {
    "mekhan-artifacts".to_string()
}

fn default_s3_region() -> String {
    "us-east-1".to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct CleanupConfig {
    #[serde(default = "default_retention_hours")]
    pub retention_hours: u64,
    #[serde(default = "default_sweep_interval_minutes")]
    pub sweep_interval_minutes: u64,
    #[serde(default = "default_purge_events")]
    pub purge_events: bool,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            retention_hours: default_retention_hours(),
            sweep_interval_minutes: default_sweep_interval_minutes(),
            purge_events: default_purge_events(),
        }
    }
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    3100
}

fn default_petri_lab_url() -> String {
    "http://localhost:3030".to_string()
}

fn default_nats_url() -> String {
    "nats://localhost:4333".to_string()
}

fn default_retention_hours() -> u64 {
    72
}

fn default_sweep_interval_minutes() -> u64 {
    60
}

fn default_purge_events() -> bool {
    true
}

impl AppConfig {
    pub fn load() -> Result<Self, config::ConfigError> {
        let config = Config::builder()
            .set_default("host", default_host())?
            .set_default("port", default_port() as i64)?
            .set_default("petri_lab_url", default_petri_lab_url())?
            .set_default("nats_url", default_nats_url())?
            .add_source(File::with_name("mekhan").required(false))
            .add_source(
                Environment::with_prefix("MEKHAN")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()?;

        config.try_deserialize()
    }
}
