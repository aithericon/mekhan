//! Unified model-pool **control-plane** backend (P2 — node agent).
//!
//! The node agent ([`crate::...model_agent`] in `executor-service`) drives a
//! local model server's ADMIN surface to place/evict models on demand. Two
//! server flavours are supported behind one [`ModelBackend`] enum:
//!
//! - **vLLM** ([`crate::adapters::vllm::VllmAdapter`]) — GPU/CUDA (or CPU) engine;
//!   load/unload = runtime LoRA adapters, base swap = `/sleep`+`/wake_up`.
//! - **Ollama** ([`OllamaControlAdapter`]) — the Metal-native path on Apple
//!   Silicon (vLLM has no Metal backend). "Load a base" = warm the model into
//!   VRAM (`POST /api/generate {model, keep_alive}`); "unload" = evict it
//!   (`keep_alive: 0`). Ollama has no runtime-LoRA API, so LoRA ops are a logged
//!   capability gap (no-op), mirroring vLLM's 404 tolerance.
//!
//! This is CONTROL-PLANE ONLY. Inference is conventional OpenAI HTTP straight to
//! the server — it never crosses this client, the engine net, or the presence
//! net. The agent only probes "what is resident" + applies load/unload.

use serde::Deserialize;
use tracing::warn;

use crate::adapters::vllm::{LoadedModel, VllmAdapter};
use crate::port::LlmError;

/// Ollama runtime control client. Drives the native `/api/*` admin surface:
/// `/api/ps` (resident models = what is currently placed) for the probe, and
/// `/api/generate` with `keep_alive` to warm/evict a model in VRAM. Holds a base
/// URL (`http://host:11434`, no trailing slash) + a reusable client with a long
/// timeout (a cold warm-up pulls weights onto the GPU). Cheap to clone.
#[derive(Debug, Clone)]
pub struct OllamaControlAdapter {
    base_url: String,
    client: reqwest::Client,
}

/// One row of Ollama's `GET /api/ps` (resident) / `GET /api/tags` `models` array.
#[derive(Debug, Deserialize)]
struct OllamaModelRow {
    #[serde(default)]
    name: String,
    #[serde(default)]
    model: String,
}

#[derive(Debug, Deserialize)]
struct OllamaModelsResponse {
    #[serde(default)]
    models: Vec<OllamaModelRow>,
}

impl OllamaControlAdapter {
    /// Construct a client for an Ollama server at `base_url`
    /// (e.g. `http://localhost:11434`). Trailing slash trimmed.
    pub fn new(base_url: impl Into<String>) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .unwrap_or_default();
        Self { base_url, client }
    }

    /// The configured base URL (trailing slash trimmed).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Probe the **resident** models via `GET /api/ps`. A model is "placed" on
    /// this engine once it is loaded into VRAM, so `/api/ps` (not `/api/tags`,
    /// which lists everything pulled-to-disk) is the control-plane truth: a
    /// load/unload command visibly changes this set. Each resident model maps to
    /// a [`LoadedModel::Base`] (Ollama has no runtime-LoRA notion here).
    pub async fn probe_loaded_models(&self) -> Result<Vec<LoadedModel>, LlmError> {
        let url = format!("{}/api/ps", self.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| LlmError::Api(format!("ollama /api/ps failed: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!("ollama /api/ps returned {status}: {text}")));
        }
        let body: OllamaModelsResponse = resp
            .json()
            .await
            .map_err(|e| LlmError::Api(format!("ollama /api/ps parse: {e}")))?;
        Ok(body
            .models
            .into_iter()
            .map(|m| {
                let id = if !m.name.is_empty() { m.name } else { m.model };
                LoadedModel::Base {
                    model_id: id,
                    max_num_seqs: None,
                }
            })
            .collect())
    }

    /// Probe the models **pulled to disk** via `GET /api/tags` — the superset of
    /// `/api/ps` (resident). These are loadable WITHOUT a re-download, so the
    /// control plane surfaces them as "provisioned, ready to load". Each maps to a
    /// [`LoadedModel::Base`] (Ollama has no runtime-LoRA notion here).
    pub async fn probe_pulled_models(&self) -> Result<Vec<LoadedModel>, LlmError> {
        let url = format!("{}/api/tags", self.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| LlmError::Api(format!("ollama /api/tags failed: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!("ollama /api/tags returned {status}: {text}")));
        }
        let body: OllamaModelsResponse = resp
            .json()
            .await
            .map_err(|e| LlmError::Api(format!("ollama /api/tags parse: {e}")))?;
        Ok(body
            .models
            .into_iter()
            .map(|m| {
                let id = if !m.name.is_empty() { m.name } else { m.model };
                LoadedModel::Base {
                    model_id: id,
                    max_num_seqs: None,
                }
            })
            .collect())
    }

    /// Provision a model to disk via `POST /api/pull {model, stream:false}` —
    /// blocks until the weights are fully fetched (Ollama streams progress; with
    /// `stream:false` it returns once on completion). Multi-GB downloads take
    /// minutes, so this is issued on a per-call client with a generous timeout
    /// rather than the struct's shared 180s client. The post-condition is "pulled
    /// to disk" (visible via `/api/tags`), NOT resident — a later `load_base`
    /// warms it into VRAM.
    pub async fn pull_base(&self, model_id: &str) -> Result<(), LlmError> {
        let url = format!("{}/api/pull", self.base_url);
        let body = serde_json::json!({ "model": model_id, "stream": false });
        // A dedicated long-timeout client: a cold pull of a large model can run
        // many minutes; the shared 180s client would abort it.
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3600))
            .build()
            .unwrap_or_else(|_| self.client.clone());
        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Api(format!("ollama pull failed: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!("ollama pull {model_id} returned {status}: {text}")));
        }
        // `stream:false` still returns a final JSON body `{status:"success"}` on
        // success; a non-"success" status field is a soft failure (e.g. a bad
        // model name returns 200 with `{error:...}` on some builds).
        let v: serde_json::Value = resp.json().await.unwrap_or_default();
        if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
            return Err(LlmError::Api(format!("ollama pull {model_id}: {err}")));
        }
        Ok(())
    }

    /// Warm a base model into VRAM via `POST /api/generate {model, keep_alive}`
    /// with no prompt — Ollama treats a prompt-less generate as a model LOAD and
    /// returns `{done_reason: "load"}`. `keep_alive` keeps it resident. This is
    /// the Metal-native "place this model" actuation.
    pub async fn load_base(&self, model_id: &str) -> Result<(), LlmError> {
        let url = format!("{}/api/generate", self.base_url);
        let body = serde_json::json!({ "model": model_id, "keep_alive": "1h" });
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Api(format!("ollama load (generate) failed: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!("ollama load {model_id} returned {status}: {text}")));
        }
        Ok(())
    }

    /// Evict a base model from VRAM via `POST /api/generate {model, keep_alive:0}`
    /// — Ollama unloads it immediately. The post-condition is "not resident", so
    /// a non-success is reported but the agent treats placement as desired-state.
    pub async fn unload_base(&self, model_id: &str) -> Result<(), LlmError> {
        let url = format!("{}/api/generate", self.base_url);
        let body = serde_json::json!({ "model": model_id, "keep_alive": 0 });
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Api(format!("ollama unload (generate) failed: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!(
                "ollama unload {model_id} returned {status}: {text}"
            )));
        }
        // `keep_alive: 0` returns BEFORE the VRAM is actually freed, so an
        // immediate re-probe would still see the model resident. Wait (bounded,
        // fail-soft) for it to leave `/api/ps` so the agent's follow-up catalog
        // probe reflects the eviction rather than a stale "still loaded".
        for _ in 0..20 {
            let still_resident = self
                .probe_loaded_models()
                .await
                .map(|ms| {
                    ms.iter().any(|m| {
                        matches!(m, LoadedModel::Base { model_id: id, .. } if id == model_id)
                    })
                })
                .unwrap_or(false);
            if !still_resident {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        Ok(())
    }
}

/// The model-pool control backend the node agent drives. Selected by the
/// `[model_agent].backend` config key (`"vllm"` default, or `"ollama"`).
#[derive(Debug, Clone)]
pub enum ModelBackend {
    /// vLLM admin surface (LoRA load/unload + sleep/wake base swap).
    Vllm(VllmAdapter),
    /// Ollama runtime (Metal-native base warm/evict; no runtime LoRA).
    Ollama(OllamaControlAdapter),
}

impl ModelBackend {
    /// The configured endpoint URL.
    pub fn endpoint(&self) -> &str {
        match self {
            ModelBackend::Vllm(a) => a.base_url(),
            ModelBackend::Ollama(a) => a.base_url(),
        }
    }

    /// Probe the currently-served/resident models for the interface catalog.
    pub async fn probe_loaded_models(&self) -> Result<Vec<LoadedModel>, LlmError> {
        match self {
            ModelBackend::Vllm(a) => a.probe_loaded_models().await,
            ModelBackend::Ollama(a) => a.probe_loaded_models().await,
        }
    }

    /// Probe the models **provisioned to disk** (loadable without a re-download).
    /// Ollama → `GET /api/tags` (the pulled superset of resident). vLLM → its
    /// served set (`/v1/models`): a vLLM engine's base is fixed at launch, so
    /// "provisioned" and "resident" coincide.
    pub async fn probe_pulled_models(&self) -> Result<Vec<LoadedModel>, LlmError> {
        match self {
            ModelBackend::Vllm(a) => a.probe_loaded_models().await,
            ModelBackend::Ollama(a) => a.probe_pulled_models().await,
        }
    }

    /// Provision a base model to disk (no residency change). Ollama → `/api/pull`.
    /// vLLM → logged no-op: the base is fixed at engine launch and HF weights are
    /// fetched then, so there is no runtime pull (capability gap).
    pub async fn pull_base(&self, model_id: &str) -> Result<(), LlmError> {
        match self {
            ModelBackend::Vllm(_) => {
                warn!(%model_id, "vllm backend does not support a runtime pull (base is fixed at launch); skipping");
                Ok(())
            }
            ModelBackend::Ollama(a) => a.pull_base(model_id).await,
        }
    }

    /// Load a LoRA adapter. Ollama has no runtime-LoRA API → logged no-op
    /// (capability gap), mirroring vLLM's 404 tolerance.
    pub async fn load_lora(&self, adapter_id: &str, source: &str) -> Result<(), LlmError> {
        match self {
            ModelBackend::Vllm(a) => a.load_lora(adapter_id, source).await,
            ModelBackend::Ollama(_) => {
                warn!(%adapter_id, "ollama backend does not support runtime LoRA load; skipping");
                Ok(())
            }
        }
    }

    /// Unload a LoRA adapter. Ollama → logged no-op (post-condition already met).
    pub async fn unload_lora(&self, adapter_id: &str) -> Result<(), LlmError> {
        match self {
            ModelBackend::Vllm(a) => a.unload_lora(adapter_id).await,
            ModelBackend::Ollama(_) => {
                warn!(%adapter_id, "ollama backend does not support runtime LoRA unload; skipping");
                Ok(())
            }
        }
    }

    /// Make a base model resident. vLLM → `wake_up` (the single resident engine);
    /// Ollama → warm `model_id` into VRAM (Metal-native placement).
    pub async fn load_base(&self, model_id: &str) -> Result<(), LlmError> {
        match self {
            ModelBackend::Vllm(a) => a.wake_up().await,
            ModelBackend::Ollama(a) => a.load_base(model_id).await,
        }
    }

    /// Make a base model absent. vLLM → `sleep`; Ollama → evict `model_id` from VRAM.
    pub async fn unload_base(&self, model_id: &str) -> Result<(), LlmError> {
        match self {
            ModelBackend::Vllm(a) => a.sleep().await,
            ModelBackend::Ollama(a) => a.unload_base(model_id).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn probe_maps_resident_models_to_bases() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/ps"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "models": [ { "name": "llama3.2:1b" }, { "name": "qwen3.5:9b" } ]
            })))
            .mount(&server)
            .await;
        let a = OllamaControlAdapter::new(server.uri());
        let models = a.probe_loaded_models().await.unwrap();
        assert_eq!(
            models,
            vec![
                LoadedModel::Base { model_id: "llama3.2:1b".into(), max_num_seqs: None },
                LoadedModel::Base { model_id: "qwen3.5:9b".into(), max_num_seqs: None },
            ]
        );
    }

    #[tokio::test]
    async fn load_base_posts_model_with_keep_alive() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .and(body_json(serde_json::json!({ "model": "llama3.2:1b", "keep_alive": "1h" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "llama3.2:1b", "done": true, "done_reason": "load"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let a = OllamaControlAdapter::new(server.uri());
        a.load_base("llama3.2:1b").await.unwrap();
    }

    #[tokio::test]
    async fn unload_base_posts_keep_alive_zero() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .and(body_json(serde_json::json!({ "model": "llama3.2:1b", "keep_alive": 0 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "llama3.2:1b", "done": true, "done_reason": "unload"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let a = OllamaControlAdapter::new(server.uri());
        a.unload_base("llama3.2:1b").await.unwrap();
    }

    #[tokio::test]
    async fn ollama_lora_is_capability_gap_noop() {
        let a = OllamaControlAdapter::new("http://localhost:1");
        let backend = ModelBackend::Ollama(a);
        // No HTTP call is made — LoRA is a logged no-op on Ollama.
        backend.load_lora("x", "s3://y").await.unwrap();
        backend.unload_lora("x").await.unwrap();
    }
}
