//! vLLM **control-plane** client (P2 — model-pool node agent).
//!
//! This is NOT an inference adapter. Inference is conventional OpenAI HTTP that
//! the router calls straight against vLLM's `/v1/chat/completions` — it never
//! crosses this client, the engine Petri net, or the presence net (routing
//! inference through a 1-in-flight control channel would starve vLLM's
//! continuous batcher). This client only drives vLLM's *admin* surface so the
//! node agent can:
//!
//! - load / unload LoRA adapters at runtime (`/v1/load_lora_adapter`,
//!   `/v1/unload_lora_adapter`),
//! - swap the resident base via sleep/wake (`/sleep`, `/wake_up`),
//! - probe the currently-served models (`GET /v1/models`) to (re)publish the
//!   runner's served-model interface catalog.
//!
//! It mirrors the *mechanism* of [`crate::ollama_subprocess`]'s
//! `model_load`/`model_unload`/probe — same reqwest verb pattern, same
//! 404-tolerant unload semantics — but talks HTTP to an **external** vLLM
//! server (no subprocess spawn; vLLM is launched out-of-band by the deploy).
//!
//! ## vLLM launch-contract assumptions (fail-soft if unmet)
//!
//! These admin endpoints depend on vLLM launch flags that may be absent:
//! - runtime LoRA load/unload requires `VLLM_ALLOW_RUNTIME_LORA_UPDATING=1`,
//! - `/sleep` + `/wake_up` require `enable_sleep_mode` at launch.
//!
//! When an endpoint is absent vLLM returns 404. Every method here treats a 404
//! as a **capability gap, not a hard error**: unload (post-condition "absent")
//! and sleep/wake (best-effort base swap) succeed on 404 with a logged warning;
//! the probe skips an absent admin endpoint and returns an empty set rather than
//! crashing the daemon. The deploy docs must list the required flags.

use serde::Deserialize;
use tracing::warn;

use crate::port::LlmError;

/// A model row vLLM currently serves, as surfaced by `GET /v1/models`.
///
/// vLLM exposes LoRA adapters as model rows whose `parent` (or `root`) points
/// at the base they attach to. We split on that: a row with no parent is the
/// served **base**, a row with a parent is a **LoRA**.
///
/// `max_num_seqs` (the per-engine concurrency budget C, `=--max-num-seqs`) is
/// **NOT** in `/v1/models` — it is a launch arg. It is therefore attributed
/// only to the `Base` (the agent fills it from config; LoRAs share the base's
/// budget and carry a base back-pointer instead).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadedModel {
    /// The resident base engine. `max_num_seqs` (C) is filled from config by the
    /// agent, not probed — see the type doc.
    Base {
        model_id: String,
        max_num_seqs: Option<u32>,
    },
    /// A runtime LoRA adapter attached to `base`. Shares the base's C budget;
    /// the back-pointer lets the router know adapters contend for one engine.
    Lora { adapter_id: String, base: String },
}

/// vLLM admin client. Holds a base URL (`http://host:port`, no trailing slash)
/// and a reusable reqwest client. Cheap to clone.
#[derive(Debug, Clone)]
pub struct VllmAdapter {
    base_url: String,
    client: reqwest::Client,
}

/// Shape of one row in vLLM's `GET /v1/models` `data` array. vLLM marks LoRA
/// rows with a non-null `parent` pointing at the base model id (older builds
/// used `root`; we accept either, preferring `parent`).
#[derive(Debug, Deserialize)]
struct ModelRow {
    id: String,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    root: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelRow>,
}

impl VllmAdapter {
    /// Construct a client for a vLLM OpenAI server at `base_url`
    /// (e.g. `http://localhost:8000`). A trailing slash is trimmed so callers
    /// can append `/v1/...` without doubling it.
    pub fn new(base_url: impl Into<String>) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        Self {
            base_url,
            client: reqwest::Client::new(),
        }
    }

    /// The configured base URL (trailing slash trimmed).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Load a LoRA adapter at runtime via `POST /v1/load_lora_adapter` with
    /// `{"lora_name", "lora_path"}`. Requires vLLM launched with
    /// `VLLM_ALLOW_RUNTIME_LORA_UPDATING=1`.
    ///
    /// A 404 here is a **capability gap** (the endpoint is disabled): we log a
    /// warning and return `Ok(())` so the agent stays up — the catalog re-push
    /// that follows simply won't show the adapter (the probe is the source of
    /// truth). Any other non-success status is a real error.
    pub async fn load_lora(&self, adapter_id: &str, source: &str) -> Result<(), LlmError> {
        let url = format!("{}/v1/load_lora_adapter", self.base_url);
        let body = serde_json::json!({ "lora_name": adapter_id, "lora_path": source });
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Api(format!("vllm /v1/load_lora_adapter failed: {e}")))?;

        if resp.status().as_u16() == 404 {
            warn!(
                %adapter_id,
                "vllm /v1/load_lora_adapter returned 404 — runtime LoRA loading is \
                 disabled (launch vLLM with VLLM_ALLOW_RUNTIME_LORA_UPDATING=1); skipping"
            );
            return Ok(());
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!(
                "vllm /v1/load_lora_adapter returned {status}: {text}"
            )));
        }
        Ok(())
    }

    /// Unload a LoRA adapter via `POST /v1/unload_lora_adapter` with
    /// `{"lora_name"}`.
    ///
    /// 404-tolerant: the post-condition is "adapter absent", so a 404 (already
    /// gone, or the endpoint disabled) is success — mirrors ollama's
    /// `model_unload`.
    pub async fn unload_lora(&self, adapter_id: &str) -> Result<(), LlmError> {
        let url = format!("{}/v1/unload_lora_adapter", self.base_url);
        let body = serde_json::json!({ "lora_name": adapter_id });
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Api(format!("vllm /v1/unload_lora_adapter failed: {e}")))?;

        // 404 is acceptable — the adapter wasn't present (or the endpoint is
        // disabled), which is the post-condition we want.
        if !resp.status().is_success() && resp.status().as_u16() != 404 {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!(
                "vllm /v1/unload_lora_adapter returned {status}: {text}"
            )));
        }
        Ok(())
    }

    /// Put the resident base engine to sleep via `POST /sleep` (a base swap —
    /// frees GPU memory so a different base can wake). Requires vLLM launched
    /// with `enable_sleep_mode`.
    ///
    /// 404-tolerant: a 404 means the build doesn't support sleep/wake; we log a
    /// capability warning and return `Ok(())` rather than fail the agent.
    pub async fn sleep(&self) -> Result<(), LlmError> {
        let url = format!("{}/sleep", self.base_url);
        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .map_err(|e| LlmError::Api(format!("vllm /sleep failed: {e}")))?;

        if resp.status().as_u16() == 404 {
            warn!(
                "vllm /sleep returned 404 — sleep/wake base-swap is unavailable \
                 (launch vLLM with enable_sleep_mode); skipping"
            );
            return Ok(());
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!(
                "vllm /sleep returned {status}: {text}"
            )));
        }
        Ok(())
    }

    /// Wake a slept base engine via `POST /wake_up`. Requires
    /// `enable_sleep_mode` (see [`Self::sleep`]).
    ///
    /// 404-tolerant: same capability-gap semantics as [`Self::sleep`].
    pub async fn wake_up(&self) -> Result<(), LlmError> {
        let url = format!("{}/wake_up", self.base_url);
        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .map_err(|e| LlmError::Api(format!("vllm /wake_up failed: {e}")))?;

        if resp.status().as_u16() == 404 {
            warn!(
                "vllm /wake_up returned 404 — sleep/wake base-swap is unavailable \
                 (launch vLLM with enable_sleep_mode); skipping"
            );
            return Ok(());
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!(
                "vllm /wake_up returned {status}: {text}"
            )));
        }
        Ok(())
    }

    /// Probe the currently-served models via `GET /v1/models` and split into a
    /// served base + its LoRA adapters.
    ///
    /// vLLM marks a LoRA row with a `parent` (or `root`) pointing at the base
    /// id; a row with no parent is the base. `max_num_seqs` is left `None` here
    /// (it is not in `/v1/models`) — the agent fills C from config.
    ///
    /// **Fail-soft**: a 404 (admin surface absent) logs a capability warning and
    /// returns an empty set rather than erroring, so the agent never crashes on
    /// a vLLM build that doesn't expose the endpoint. A transport error or a
    /// non-404 failure status is still a real error.
    pub async fn probe_loaded_models(&self) -> Result<Vec<LoadedModel>, LlmError> {
        let url = format!("{}/v1/models", self.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| LlmError::Api(format!("vllm /v1/models failed: {e}")))?;

        if resp.status().as_u16() == 404 {
            warn!("vllm /v1/models returned 404 — cannot probe served models; reporting none");
            return Ok(Vec::new());
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!(
                "vllm /v1/models returned {status}: {text}"
            )));
        }

        let body: ModelsResponse = resp
            .json()
            .await
            .map_err(|e| LlmError::Api(format!("vllm /v1/models parse: {e}")))?;

        Ok(body
            .data
            .into_iter()
            .map(|row| {
                // vLLM marks a LoRA row with `parent` = the base it attaches to.
                // A BASE row has `parent: null` but `root` pointing at ITSELF
                // (vLLM ≥0.20 fills `root` = the model's own id). So a row is a
                // LoRA only when it has a parent, or a `root` naming a DIFFERENT
                // model — a self-referential `root` is the base, not an adapter.
                let base = row
                    .parent
                    .clone()
                    .or_else(|| row.root.clone().filter(|r| *r != row.id));
                match base {
                    Some(base) => LoadedModel::Lora {
                        adapter_id: row.id,
                        base,
                    },
                    None => LoadedModel::Base {
                        model_id: row.id,
                        max_num_seqs: None,
                    },
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn load_lora_posts_name_and_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/load_lora_adapter"))
            .and(body_json(
                serde_json::json!({ "lora_name": "my-lora", "lora_path": "/models/my-lora" }),
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let adapter = VllmAdapter::new(server.uri());
        adapter
            .load_lora("my-lora", "/models/my-lora")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn load_lora_404_is_capability_gap_not_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/load_lora_adapter"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let adapter = VllmAdapter::new(server.uri());
        // 404 => Ok (endpoint disabled; warn + skip, don't crash the agent).
        adapter.load_lora("my-lora", "/x").await.unwrap();
    }

    #[tokio::test]
    async fn load_lora_500_is_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/load_lora_adapter"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let adapter = VllmAdapter::new(server.uri());
        let err = adapter.load_lora("my-lora", "/x").await.unwrap_err();
        assert!(matches!(err, LlmError::Api(_)));
    }

    #[tokio::test]
    async fn unload_lora_posts_name() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/unload_lora_adapter"))
            .and(body_json(serde_json::json!({ "lora_name": "my-lora" })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let adapter = VllmAdapter::new(server.uri());
        adapter.unload_lora("my-lora").await.unwrap();
    }

    #[tokio::test]
    async fn unload_lora_404_is_tolerated() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/unload_lora_adapter"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let adapter = VllmAdapter::new(server.uri());
        // Post-condition "absent" — 404 is success.
        adapter.unload_lora("gone").await.unwrap();
    }

    #[tokio::test]
    async fn sleep_and_wake_hit_their_paths() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/sleep"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/wake_up"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let adapter = VllmAdapter::new(server.uri());
        adapter.sleep().await.unwrap();
        adapter.wake_up().await.unwrap();
    }

    #[tokio::test]
    async fn sleep_404_is_capability_gap() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/sleep"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let adapter = VllmAdapter::new(server.uri());
        // enable_sleep_mode not set => 404 => warn + Ok.
        adapter.sleep().await.unwrap();
    }

    #[tokio::test]
    async fn probe_splits_base_and_loras_by_parent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "object": "list",
                "data": [
                    { "id": "meta-llama/Llama-3-8B", "object": "model" },
                    // A real vLLM ≥0.20 base row: parent null, root = ITSELF.
                    // Must classify as Base, not a self-referential LoRA
                    // (regression: live probe of `facebook/opt-125m` mislabeled
                    // the base as a LoRA because `root` was non-null).
                    { "id": "facebook/opt-125m", "object": "model", "parent": null, "root": "facebook/opt-125m" },
                    { "id": "adapter-a", "object": "model", "parent": "meta-llama/Llama-3-8B" },
                    { "id": "adapter-b", "object": "model", "root": "meta-llama/Llama-3-8B" }
                ]
            })))
            .mount(&server)
            .await;

        let adapter = VllmAdapter::new(server.uri());
        let models = adapter.probe_loaded_models().await.unwrap();
        assert_eq!(
            models,
            vec![
                LoadedModel::Base {
                    model_id: "meta-llama/Llama-3-8B".into(),
                    max_num_seqs: None,
                },
                LoadedModel::Base {
                    model_id: "facebook/opt-125m".into(),
                    max_num_seqs: None,
                },
                LoadedModel::Lora {
                    adapter_id: "adapter-a".into(),
                    base: "meta-llama/Llama-3-8B".into(),
                },
                LoadedModel::Lora {
                    adapter_id: "adapter-b".into(),
                    base: "meta-llama/Llama-3-8B".into(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn probe_404_reports_empty_not_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let adapter = VllmAdapter::new(server.uri());
        assert!(adapter.probe_loaded_models().await.unwrap().is_empty());
    }
}
