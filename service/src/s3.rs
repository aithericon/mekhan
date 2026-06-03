use aws_credential_types::Credentials;
use aws_sdk_s3::config::{BehaviorVersion, Region};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use uuid::Uuid;

use crate::config::S3Config;

#[derive(Debug, thiserror::Error)]
pub enum ArtifactStoreError {
    #[error("S3 error: {0}")]
    S3(String),
}

impl<E: std::fmt::Display> From<aws_sdk_s3::error::SdkError<E>> for ArtifactStoreError {
    fn from(e: aws_sdk_s3::error::SdkError<E>) -> Self {
        ArtifactStoreError::S3(e.to_string())
    }
}

#[derive(Clone)]
pub struct ArtifactStore {
    client: Client,
    bucket: String,
}

impl ArtifactStore {
    /// Create a new MinIO-compatible S3 client.
    pub fn new(config: &S3Config) -> Self {
        let credentials = Credentials::new(
            &config.access_key,
            &config.secret_key,
            None,
            None,
            "mekhan-static",
        );

        let s3_config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(config.region.clone()))
            .endpoint_url(&config.endpoint)
            .force_path_style(true)
            .credentials_provider(credentials)
            .build();

        let client = Client::from_conf(s3_config);

        Self {
            client,
            bucket: config.bucket.clone(),
        }
    }

    /// Ensure the bucket exists, creating it if necessary.
    pub async fn ensure_bucket(&self) -> Result<(), ArtifactStoreError> {
        let exists = self.client.head_bucket().bucket(&self.bucket).send().await;

        if exists.is_ok() {
            return Ok(());
        }

        self.client
            .create_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .map_err(|e| ArtifactStoreError::S3(format!("create bucket: {e}")))?;

        tracing::info!(bucket = %self.bucket, "created S3 bucket");
        Ok(())
    }

    /// Upload a file and return the S3 key.
    /// Key format: `templates/{template_id}/v{version}/{node_id}/{filename}`
    pub async fn upload_file(
        &self,
        template_id: Uuid,
        version: i32,
        node_id: &str,
        filename: &str,
        content: &[u8],
    ) -> Result<String, ArtifactStoreError> {
        let key = format!("templates/{template_id}/v{version}/{node_id}/{filename}");

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(ByteStream::from(content.to_vec()))
            .cache_control("immutable")
            .send()
            .await
            .map_err(|e| ArtifactStoreError::S3(format!("upload {key}: {e}")))?;

        tracing::debug!(key = %key, "uploaded artifact to S3");

        Ok(key)
    }

    /// Upload a blob (e.g. image) and return the S3 key.
    /// Key format: `templates/{template_id}/blobs/{node_id}/{filename}`
    pub async fn upload_blob(
        &self,
        template_id: Uuid,
        node_id: &str,
        filename: &str,
        content: &[u8],
        content_type: &str,
    ) -> Result<String, ArtifactStoreError> {
        let key = format!("templates/{template_id}/blobs/{node_id}/{filename}");

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(ByteStream::from(content.to_vec()))
            .content_type(content_type)
            .cache_control("public, max-age=31536000, immutable")
            .send()
            .await
            .map_err(|e| ArtifactStoreError::S3(format!("upload blob {key}: {e}")))?;

        tracing::debug!(key = %key, content_type = %content_type, "uploaded blob to S3");

        Ok(key)
    }

    /// Upload a per-node static config blob and return the S3 key. Key format:
    /// `templates/{template_id}/v{version}/{node_id}/node-config.json`.
    ///
    /// The compiler offloads each `AutomatedStep` node's resolved
    /// (validated + `$ref`-inlined) config to S3 at publish time so the
    /// per-job NATS token only carries a `config_ref`. The executor's
    /// `FetchConfigHook` downloads the blob right before staging — see
    /// `ExecutionSpec::config_ref` in `executor-domain`.
    pub async fn upload_node_config(
        &self,
        template_id: Uuid,
        version: i32,
        node_id: &str,
        content: &[u8],
    ) -> Result<String, ArtifactStoreError> {
        let key = Self::node_config_key(template_id, version, node_id);

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(ByteStream::from(content.to_vec()))
            .content_type("application/json")
            .cache_control("immutable")
            .send()
            .await
            .map_err(|e| ArtifactStoreError::S3(format!("upload node config {key}: {e}")))?;

        tracing::debug!(key = %key, bytes = content.len(), "uploaded node config to S3");

        Ok(key)
    }

    /// Compute the S3 key the compiler should emit for a node's static
    /// config blob without performing an upload. Used at compile time so the
    /// Rhai literal embeds the right key before publish actually writes the
    /// blob — both sides agree on the format.
    pub fn node_config_key(template_id: Uuid, version: i32, node_id: &str) -> String {
        format!("templates/{template_id}/v{version}/{node_id}/node-config.json")
    }

    /// Upload a file for an asset `File` field and return the S3 key.
    /// Key format: `assets/{asset_id}/v{version}/{field}/{filename}`. The key
    /// is publish-stable (immutable-per-version, like `upload_file`) so a
    /// running instance pinned to `version` keeps resolving the same object
    /// even after the asset is edited into a newer version (docs/20 §5/§6).
    pub async fn upload_asset_file(
        &self,
        asset_id: Uuid,
        version: i32,
        field: &str,
        filename: &str,
        content: &[u8],
        content_type: &str,
    ) -> Result<String, ArtifactStoreError> {
        let key = format!("assets/{asset_id}/v{version}/{field}/{filename}");

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(ByteStream::from(content.to_vec()))
            .content_type(content_type)
            .cache_control("immutable")
            .send()
            .await
            .map_err(|e| ArtifactStoreError::S3(format!("upload asset file {key}: {e}")))?;

        tracing::debug!(key = %key, content_type = %content_type, "uploaded asset file to S3");

        Ok(key)
    }

    /// Retrieve a file from S3 by key. Returns (bytes, content_type).
    pub async fn get_file(&self, key: &str) -> Result<(Vec<u8>, String), ArtifactStoreError> {
        let resp = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| ArtifactStoreError::S3(format!("get {key}: {e}")))?;

        let content_type = resp
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();

        let bytes = resp
            .body
            .collect()
            .await
            .map_err(|e| ArtifactStoreError::S3(format!("read body {key}: {e}")))?
            .into_bytes()
            .to_vec();

        Ok((bytes, content_type))
    }

    /// Delete every object under `prefix`. Used by the retention sweep to GC
    /// per-instance agent transcript blobs (`instances/{instance_id}/`).
    /// Paginates the listing and deletes objects individually so a partial
    /// failure still removes what it can; a no-op when the prefix is empty.
    pub async fn delete_prefix(&self, prefix: &str) -> Result<(), ArtifactStoreError> {
        let mut continuation: Option<String> = None;
        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix);
            if let Some(token) = &continuation {
                req = req.continuation_token(token);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| ArtifactStoreError::S3(format!("list {prefix}: {e}")))?;

            for obj in resp.contents() {
                if let Some(key) = obj.key() {
                    self.client
                        .delete_object()
                        .bucket(&self.bucket)
                        .key(key)
                        .send()
                        .await
                        .map_err(|e| ArtifactStoreError::S3(format!("delete {key}: {e}")))?;
                }
            }

            if resp.is_truncated().unwrap_or(false) {
                continuation = resp.next_continuation_token().map(|s| s.to_string());
                if continuation.is_none() {
                    break;
                }
            } else {
                break;
            }
        }
        Ok(())
    }
}
