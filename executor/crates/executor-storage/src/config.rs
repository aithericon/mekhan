// Re-export storage types from the shared crate.
// This preserves backward compatibility for all downstream consumers.
pub use aithericon_executor_storage_types::{
    StorageBackend, StorageConfig, StorageCredentials,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_s3_config() {
        let toml = r#"
            backend = "s3"
            endpoint = "https://s3.amazonaws.com"
            bucket = "my-bucket"
            region = "us-east-1"
            prefix = "executor/"
            [credentials]
            access_key = "AKIA"
            secret_key = "secret"
        "#;

        let config: StorageConfig = toml::from_str(toml).unwrap();
        assert!(matches!(config.backend, StorageBackend::S3));
        assert_eq!(config.bucket, "my-bucket");
        assert_eq!(config.credentials.access_key, "AKIA");
    }

    #[test]
    fn deserialize_local_config() {
        let toml = r#"
            backend = "local"
            endpoint = "/data/storage"
        "#;

        let config: StorageConfig = toml::from_str(toml).unwrap();
        assert!(matches!(config.backend, StorageBackend::Local));
        assert_eq!(config.endpoint, "/data/storage");
        assert!(config.prefix.is_empty());
    }
}
