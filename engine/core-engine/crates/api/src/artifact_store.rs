//! Artifact store for uploading files referenced by `storage_path` inputs.
//!
//! The SDK's `ctx.stage_file()` uploads files here before deploying the
//! scenario. The executor then downloads them at job execution time via
//! its own storage configuration (opendal).
//!
//! Requires the `artifact-store` feature flag and environment variables:
//!
//! ```text
//! ARTIFACT_STORE_ENDPOINT=http://localhost:9005
//! ARTIFACT_STORE_BUCKET=bo-models
//! ARTIFACT_STORE_REGION=us-east-1           # optional
//! ARTIFACT_STORE_PREFIX=                    # optional path prefix
//! ARTIFACT_STORE_ACCESS_KEY=rustfsadmin
//! ARTIFACT_STORE_SECRET_KEY=rustfsadmin
//! ```

#[cfg(feature = "artifact-store")]
mod inner {
    use aithericon_executor_storage::build_operator;
    use aithericon_executor_storage_types::{StorageBackend, StorageConfig, StorageCredentials};
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    /// Shared artifact store state wrapping an opendal operator.
    #[derive(Clone)]
    pub struct ArtifactStoreState {
        operator: opendal::Operator,
        prefix: String,
    }

    impl ArtifactStoreState {
        /// Create from `ARTIFACT_STORE_*` environment variables.
        ///
        /// Returns `None` if `ARTIFACT_STORE_ENDPOINT` is not set.
        pub fn from_env() -> Option<Self> {
            let endpoint = std::env::var("ARTIFACT_STORE_ENDPOINT").ok()?;
            let bucket = std::env::var("ARTIFACT_STORE_BUCKET").unwrap_or_default();
            let region = std::env::var("ARTIFACT_STORE_REGION").ok();
            let prefix = std::env::var("ARTIFACT_STORE_PREFIX").unwrap_or_default();
            let access_key = std::env::var("ARTIFACT_STORE_ACCESS_KEY").unwrap_or_default();
            let secret_key = std::env::var("ARTIFACT_STORE_SECRET_KEY").unwrap_or_default();

            let config = StorageConfig {
                backend: StorageBackend::S3,
                endpoint,
                bucket,
                region,
                prefix: prefix.clone(),
                credentials: StorageCredentials {
                    access_key,
                    secret_key,
                },
                retry: Default::default(),
                resource_alias: None,
            };

            match build_operator(&config) {
                Ok(operator) => Some(Self { operator, prefix }),
                Err(e) => {
                    tracing::error!(error = %e, "Failed to build artifact store operator");
                    None
                }
            }
        }
    }

    /// Handler: `PUT /api/artifacts/*path`
    ///
    /// Accepts raw bytes and writes them to the artifact store at the given path.
    /// Used by the SDK's deploy flow to stage scripts before deploying scenarios.
    pub async fn upload_artifact(
        State(store): State<ArtifactStoreState>,
        axum::extract::Path(storage_path): axum::extract::Path<String>,
        body: Bytes,
    ) -> impl IntoResponse {
        // Axum 0.7 catch-all includes leading '/' — strip it
        let clean_path = storage_path.trim_start_matches('/');

        let remote_path = if store.prefix.is_empty() {
            clean_path.to_string()
        } else {
            format!("{}{}", store.prefix, clean_path)
        };

        match store.operator.write(&remote_path, body.to_vec()).await {
            Ok(_) => {
                tracing::info!(
                    path = %storage_path,
                    remote = %remote_path,
                    "Artifact uploaded"
                );
                StatusCode::CREATED
            }
            Err(e) => {
                tracing::error!(
                    path = %storage_path,
                    error = %e,
                    "Failed to upload artifact"
                );
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }
}

#[cfg(feature = "artifact-store")]
pub use inner::*;

// Stub when feature is disabled — ArtifactStoreState::from_env() always returns None.
#[cfg(not(feature = "artifact-store"))]
mod stub {
    #[derive(Clone)]
    pub struct ArtifactStoreState;

    impl ArtifactStoreState {
        pub fn from_env() -> Option<Self> {
            None
        }
    }
}

#[cfg(not(feature = "artifact-store"))]
pub use stub::*;
