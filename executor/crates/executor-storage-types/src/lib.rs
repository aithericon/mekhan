use serde::{Deserialize, Serialize};

/// Which storage backend to use.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema, utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum StorageBackend {
    /// Local filesystem (default).
    Local,
    /// Amazon S3 (or S3-compatible like MinIO).
    S3,
    /// Google Cloud Storage.
    Gcs,
    /// Azure Blob Storage.
    #[serde(alias = "azure")]
    AzBlob,
}

/// Credentials for accessing a storage backend.
///
/// Loaded from environment variables (`EXECUTOR_STORAGE_CREDENTIALS_ACCESS_KEY`,
/// `EXECUTOR_STORAGE_CREDENTIALS_SECRET_KEY`) or from the `[storage.credentials]`
/// section in `executor.toml`.
///
/// Credential fields support `{{secret:KEY}}` patterns that are resolved
/// by the staging pipeline before use.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema, utoipa::ToSchema))]
pub struct StorageCredentials {
    /// Access key (S3 access key ID, GCS HMAC key, Azure account name).
    #[serde(default)]
    pub access_key: String,

    /// Secret key (S3 secret access key, GCS HMAC secret, Azure account key).
    #[serde(default)]
    pub secret_key: String,
}

/// Retry behaviour for transient storage errors (504, connection drops, etc.).
///
/// Wired into the OpenDAL `RetryLayer`, which only retries operations whose
/// errors return `is_temporary() == true` — non-transient failures (404,
/// 403, etc.) are not retried.
///
/// Defaults are chosen to absorb a typical Hetzner / S3-compatible 504 burst
/// (~3 attempts spaced 200ms / 400ms+jitter / 800ms+jitter ≈ 2s total) without
/// blowing past a Slurm sbatch's wall-clock budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema, utoipa::ToSchema))]
pub struct RetryConfig {
    /// Maximum retry attempts (excluding the initial attempt). Default 3.
    #[serde(default = "default_retry_max_attempts")]
    pub max_attempts: u32,

    /// Initial backoff delay in milliseconds. Default 200.
    #[serde(default = "default_retry_min_delay_ms")]
    pub min_delay_ms: u64,

    /// Cap on backoff delay in milliseconds. Default 10_000 (10s).
    #[serde(default = "default_retry_max_delay_ms")]
    pub max_delay_ms: u64,
}

fn default_retry_max_attempts() -> u32 {
    3
}
fn default_retry_min_delay_ms() -> u64 {
    200
}
fn default_retry_max_delay_ms() -> u64 {
    10_000
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: default_retry_max_attempts(),
            min_delay_ms: default_retry_min_delay_ms(),
            max_delay_ms: default_retry_max_delay_ms(),
        }
    }
}

/// Configuration for a storage backend.
///
/// Used both at the executor level (global artifact store) and at the
/// per-input / per-output level for multi-backend staging.
///
/// # Environment variables (executor-level)
///
/// ```text
/// EXECUTOR_STORAGE_BACKEND=s3
/// EXECUTOR_STORAGE_ENDPOINT=https://s3.amazonaws.com
/// EXECUTOR_STORAGE_BUCKET=my-bucket
/// EXECUTOR_STORAGE_REGION=us-east-1
/// EXECUTOR_STORAGE_PREFIX=executor/
/// EXECUTOR_STORAGE_CREDENTIALS_ACCESS_KEY=AKIA...
/// EXECUTOR_STORAGE_CREDENTIALS_SECRET_KEY=wJa...
/// ```
///
/// # executor.toml
///
/// ```toml
/// [storage]
/// backend = "s3"
/// endpoint = "https://minio.internal:9000"
/// bucket = "artifacts"
/// region = "us-east-1"
/// prefix = "executor/"
///
/// [storage.credentials]
/// access_key = "minioadmin"
/// secret_key = "minioadmin"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema, utoipa::ToSchema))]
pub struct StorageConfig {
    /// Which backend to use.
    pub backend: StorageBackend,

    /// Endpoint URL. For S3: the S3 endpoint. For local: the root directory path.
    pub endpoint: String,

    /// Bucket or container name (ignored for local).
    #[serde(default)]
    pub bucket: String,

    /// Region (optional, for S3/GCS).
    #[serde(default)]
    pub region: Option<String>,

    /// Path prefix within the bucket (e.g. "executor/").
    #[serde(default = "default_prefix")]
    pub prefix: String,

    /// Credentials for the storage backend.
    #[serde(default)]
    pub credentials: StorageCredentials,

    /// Retry behaviour for transient storage errors. Applied via OpenDAL's
    /// `RetryLayer`, so only errors with `is_temporary() == true` are retried.
    #[serde(default)]
    pub retry: RetryConfig,

    /// Optional workspace resource binding (e.g. an `s3` resource). When
    /// set, the executor's file-ops backend overlays `endpoint`, `region`,
    /// `bucket`, and credentials from the resource envelope at run time —
    /// per-step inline values still win on a field-by-field basis. Empty
    /// or absent means "use the inline fields directly".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_alias: Option<String>,
}

fn default_prefix() -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_config_serde_roundtrip() {
        let config = StorageConfig {
            backend: StorageBackend::S3,
            endpoint: "https://s3.amazonaws.com".into(),
            bucket: "my-bucket".into(),
            region: Some("us-east-1".into()),
            prefix: "executor/".into(),
            credentials: StorageCredentials {
                access_key: "AKIAIOSFODNN7EXAMPLE".into(),
                secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            },
            retry: RetryConfig::default(),
            resource_alias: None,
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: StorageConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.bucket, "my-bucket");
        assert_eq!(deserialized.prefix, "executor/");
        assert_eq!(deserialized.credentials.access_key, "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(deserialized.retry.max_attempts, 3);
    }

    #[test]
    fn retry_config_uses_defaults_when_omitted() {
        let json = r#"{"backend": "local", "endpoint": "/tmp/store"}"#;
        let config: StorageConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.retry.max_attempts, 3);
        assert_eq!(config.retry.min_delay_ms, 200);
        assert_eq!(config.retry.max_delay_ms, 10_000);
    }

    #[test]
    fn retry_config_individual_field_defaults() {
        let json = r#"{"backend": "local", "endpoint": "/tmp/store", "retry": {"max_attempts": 5}}"#;
        let config: StorageConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.retry.max_attempts, 5);
        // others fall back to defaults
        assert_eq!(config.retry.min_delay_ms, 200);
        assert_eq!(config.retry.max_delay_ms, 10_000);
    }

    #[test]
    fn storage_config_defaults() {
        // Minimal config: only backend and endpoint are required
        let json = r#"{"backend": "local", "endpoint": "/tmp/store"}"#;
        let config: StorageConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.bucket, "");
        assert!(config.region.is_none());
        assert_eq!(config.prefix, "");
        assert_eq!(config.credentials.access_key, "");
    }

    #[test]
    fn storage_backend_rename_all() {
        let json = r#""s3""#;
        let backend: StorageBackend = serde_json::from_str(json).unwrap();
        assert!(matches!(backend, StorageBackend::S3));

        let json = r#""gcs""#;
        let backend: StorageBackend = serde_json::from_str(json).unwrap();
        assert!(matches!(backend, StorageBackend::Gcs));

        // azure alias
        let json = r#""azure""#;
        let backend: StorageBackend = serde_json::from_str(json).unwrap();
        assert!(matches!(backend, StorageBackend::AzBlob));
    }
}
