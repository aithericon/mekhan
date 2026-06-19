//! Brokered artifact store — zero-secret runner storage over the mekhan blob
//! proxy.
//!
//! An enrolled lab runner needs **no** S3 credentials. Instead it talks to a
//! pair of mekhan endpoints that proxy bytes to/from the platform's object
//! store, authenticated only with the runner's own bearer token:
//!
//! ```text
//! GET  {base}/api/storage/blob?key=<url-encoded key>   -> 200 octet-stream | 404 | 502
//! PUT  {base}/api/storage/blob?key=<url-encoded key>   (body = octet-stream) -> 204
//! ```
//!
//! `{base}` is the runner's brokered storage URL (`identity.json`
//! `storage_url ?? mekhan_url`). Auth is `Authorization: Bearer <runner.token>`
//! (the `rnr_<uuid>.<secret>` credential).
//!
//! The shared key derivation matches the Local/OpenDal stores
//! (`artifacts/{execution_id}/{artifact_id}/{filename}`) so an artifact uploaded
//! by a brokered runner is byte-addressable by an in-cluster worker reading the
//! same object store — the proxy is a transparent pass-through over the same key
//! space.
//!
//! Limitations vs. the OpenDal store (documented, intentional — the mekhan proxy
//! is GET/PUT only):
//! * `delete` is a logged no-op (the proxy exposes no DELETE).
//! * `list` returns `Ok(vec![])` (no LIST verb on the proxy); callers that need
//!   enumeration must use an in-cluster store.

use std::path::Path;

use async_trait::async_trait;
use reqwest::StatusCode;
use tracing::{debug, warn};

use aithericon_executor_domain::{Artifact, ArtifactManifest};

use crate::local::enrich_artifact_metadata;
use crate::traits::{ArtifactStore, StorageError, StoragePath, UploadOptions};

/// Artifact store that proxies all object I/O through the mekhan blob endpoint.
///
/// Holds only a base URL + the runner's bearer token — no cloud credentials.
pub struct BrokeredArtifactStore {
    /// Brokered base URL (no trailing slash needed; constructor trims it).
    base_url: String,
    /// `rnr_<uuid>.<secret>` bearer credential for the blob proxy.
    runner_token: String,
    client: reqwest::Client,
}

impl BrokeredArtifactStore {
    /// Construct from the brokered base URL + the runner's bearer token.
    ///
    /// `base_url` is `identity.json` `storage_url ?? mekhan_url`; a trailing
    /// slash is trimmed so endpoint paths join cleanly.
    pub fn new(base_url: String, runner_token: String, client: reqwest::Client) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            runner_token,
            client,
        }
    }

    /// Build the `{base}/api/storage/blob?key=<url-encoded key>` URL.
    fn blob_url(&self, key: &str) -> String {
        let encoded = urlencoding::encode(key);
        format!("{}/api/storage/blob?key={}", self.base_url, encoded)
    }

    /// Shared key derivation — identical to the Local/OpenDal stores so brokered
    /// uploads land at the same address as in-cluster ones.
    fn artifact_key(execution_id: &str, artifact_id: &str, filename: &str) -> String {
        format!("artifacts/{execution_id}/{artifact_id}/{filename}")
    }

    /// Derived manifest key (round-trips through put/download).
    fn manifest_key(execution_id: &str) -> String {
        format!("artifacts/{execution_id}/manifest.json")
    }

    /// GET the bytes at `key`. `None` on 404; `Err` on transport/other.
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let url = self.blob_url(key);
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.runner_token)
            .send()
            .await
            .map_err(|e| StorageError::Other(format!("brokered GET '{key}': {e}")))?;

        match resp.status() {
            StatusCode::OK => {
                let bytes = resp.bytes().await.map_err(|e| {
                    StorageError::Other(format!("brokered GET '{key}' read body: {e}"))
                })?;
                Ok(Some(bytes.to_vec()))
            }
            StatusCode::NOT_FOUND => Ok(None),
            other => {
                let body = resp.text().await.unwrap_or_default();
                Err(StorageError::Other(format!(
                    "brokered GET '{key}' returned HTTP {other}: {body}"
                )))
            }
        }
    }

    /// PUT `data` at `key`. Non-2xx -> `Err`.
    async fn put_bytes(&self, key: &str, data: Vec<u8>) -> Result<(), StorageError> {
        let url = self.blob_url(key);
        let resp = self
            .client
            .put(&url)
            .bearer_auth(&self.runner_token)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(data)
            .send()
            .await
            .map_err(|e| StorageError::Other(format!("brokered PUT '{key}': {e}")))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(StorageError::Other(format!(
                "brokered PUT '{key}' returned HTTP {status}: {body}"
            )))
        }
    }
}

#[async_trait]
impl ArtifactStore for BrokeredArtifactStore {
    async fn upload(
        &self,
        execution_id: &str,
        artifact: &Artifact,
        local_path: &Path,
        options: UploadOptions,
    ) -> Result<Artifact, StorageError> {
        let storage_path = Self::artifact_key(execution_id, &artifact.id, &artifact.filename);

        if !options.overwrite && self.exists(&StoragePath(storage_path.clone())).await? {
            return Err(StorageError::AlreadyExists(storage_path));
        }

        let data = tokio::fs::read(local_path).await.map_err(StorageError::Io)?;
        let file_size = data.len() as u64;

        self.put_bytes(&storage_path, data).await?;

        let mut uploaded = artifact.clone();
        uploaded.storage_path = Some(storage_path);
        uploaded.size_bytes = Some(file_size);

        if options.extract_metadata {
            enrich_artifact_metadata(&mut uploaded, local_path).await;
        }

        debug!(
            execution_id,
            artifact_id = %artifact.id,
            filename = %artifact.filename,
            size_bytes = file_size,
            "artifact uploaded via brokered blob proxy"
        );

        Ok(uploaded)
    }

    async fn download(
        &self,
        storage_path: &StoragePath,
        local_dest: &Path,
    ) -> Result<(), StorageError> {
        let bytes = self
            .get_bytes(&storage_path.0)
            .await?
            .ok_or_else(|| StorageError::NotFound(storage_path.to_string()))?;

        if let Some(parent) = local_dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(StorageError::Io)?;
        }
        tokio::fs::write(local_dest, bytes)
            .await
            .map_err(StorageError::Io)?;
        Ok(())
    }

    async fn put(&self, storage_path: &StoragePath, data: Vec<u8>) -> Result<(), StorageError> {
        self.put_bytes(&storage_path.0, data).await
    }

    async fn exists(&self, storage_path: &StoragePath) -> Result<bool, StorageError> {
        Ok(self.get_bytes(&storage_path.0).await?.is_some())
    }

    async fn delete(&self, storage_path: &StoragePath) -> Result<(), StorageError> {
        // The mekhan blob proxy exposes only GET/PUT — there is no DELETE verb.
        // Deletion is a best-effort no-op: object lifecycle is managed
        // server-side (retention policy on the proxied bucket), not by the
        // runner. Logged so an operator can see the intent was dropped.
        warn!(
            key = %storage_path.0,
            "brokered store has no DELETE verb; delete is a no-op"
        );
        Ok(())
    }

    async fn list(&self, _execution_id: &str) -> Result<Vec<StoragePath>, StorageError> {
        // The blob proxy has no LIST verb. A brokered runner cannot enumerate
        // its execution's artifacts through mekhan; callers needing enumeration
        // must use an in-cluster store. Returning an empty list keeps the
        // upload-then-manifest path working (the manifest is the source of
        // truth for what was produced).
        Ok(Vec::new())
    }

    async fn load_manifest(
        &self,
        execution_id: &str,
    ) -> Result<Option<ArtifactManifest>, StorageError> {
        let key = Self::manifest_key(execution_id);
        match self.get_bytes(&key).await? {
            Some(bytes) => {
                let manifest: ArtifactManifest = serde_json::from_slice(&bytes)
                    .map_err(|e| StorageError::Serialization(e.to_string()))?;
                Ok(Some(manifest))
            }
            None => Ok(None),
        }
    }

    async fn save_manifest(
        &self,
        execution_id: &str,
        manifest: &ArtifactManifest,
    ) -> Result<(), StorageError> {
        let key = Self::manifest_key(execution_id);
        let data = serde_json::to_vec_pretty(manifest)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        self.put_bytes(&key, data).await?;
        debug!(execution_id, "artifact manifest saved via brokered blob proxy");
        Ok(())
    }

    fn name(&self) -> &'static str {
        "brokered"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_url_encodes_key() {
        let store = BrokeredArtifactStore::new(
            "https://mekhan.example.com/".into(),
            "rnr_abc.def".into(),
            reqwest::Client::new(),
        );
        // Trailing slash trimmed; key url-encoded (slashes become %2F).
        assert_eq!(
            store.blob_url("artifacts/exec-1/art-1/out.bin"),
            "https://mekhan.example.com/api/storage/blob?key=artifacts%2Fexec-1%2Fart-1%2Fout.bin"
        );
    }

    #[test]
    fn artifact_key_matches_shared_derivation() {
        assert_eq!(
            BrokeredArtifactStore::artifact_key("exec-1", "art-1", "out.bin"),
            "artifacts/exec-1/art-1/out.bin"
        );
        assert_eq!(
            BrokeredArtifactStore::manifest_key("exec-1"),
            "artifacts/exec-1/manifest.json"
        );
    }

    #[test]
    fn name_is_brokered() {
        let store = BrokeredArtifactStore::new(
            "https://mekhan.example.com".into(),
            "rnr_abc.def".into(),
            reqwest::Client::new(),
        );
        assert_eq!(store.name(), "brokered");
    }
}
