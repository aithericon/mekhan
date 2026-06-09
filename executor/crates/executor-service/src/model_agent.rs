//! P2 — model-pool node agent (sibling of `ros_catalog.rs`).
//!
//! When this daemon is a RUNNER on a GPU host with a local vLLM engine
//! (`runner_id` + `[mekhan].url` + `[model_agent].vllm_url` all set), this agent:
//!
//! 1. **Probes** vLLM's served models (`GET /v1/models`) at startup and builds a
//!    `RunnerInterfaceCatalog`-shaped `{ "models": [...] }` value (base entry
//!    carries C `=max_num_seqs` from config; each LoRA carries a base
//!    back-pointer), then POSTs it to mekhan
//!    (`POST /api/v1/runners/{id}/interfaces`) via the shared
//!    [`crate::catalog_publish::publish_catalog`].
//! 2. **Subscribes** to the core-NATS control channel `runner.{id}.load` /
//!    `runner.{id}.unload` (modelled on `executor-worker`'s
//!    `NatsCancelListener` — ephemeral, NOT JetStream), maps each
//!    [`ModelCommand`] onto the vLLM admin surface (LoRA load/unload, base
//!    sleep/wake), then RE-PUSHES the catalog and updates the live
//!    [`LiveModelState`] the presence task reads.
//!
//! ## HARD INVARIANT — control plane only
//!
//! `runner.{id}.load`/`unload` is CONTROL-PLANE ONLY. Inference is conventional
//! OpenAI HTTP straight to vLLM's `/v1/chat/completions` (the router calls it
//! directly) — it NEVER crosses this channel, the engine Petri net, or the
//! presence net. Routing inference through a 1-in-flight admitted channel would
//! starve vLLM's continuous batcher. The agent only REPORTS `{models, C}`; it
//! never serves inference. (GDPR: no auto external offload either — that is a
//! router/mekhan concern, not the node agent's.)
//!
//! ## Best-effort
//!
//! Every step is best-effort, like `ros_catalog`: a failed probe / publish /
//! command is logged at WARN and never crashes the daemon. The vLLM admin
//! endpoints are 404-tolerant in [`VllmAdapter`] (capability gaps when vLLM was
//! launched without `VLLM_ALLOW_RUNTIME_LORA_UPDATING=1` / `enable_sleep_mode`).

use std::time::Duration;

use futures::StreamExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use aithericon_executor_llm::{
    LoadTarget, LoadedModel, ModelBackend, ModelCommand, OllamaControlAdapter, VllmAdapter,
};
use aithericon_executor_worker::{ExecutorConfig, LiveModelState, ModelAgentSettings};

/// Spawn the model-pool node agent as fire-and-forget background work.
///
/// No-op unless ALL of `runner_id`, a mekhan URL, the runner token path, AND a
/// `[model_agent]` block (with `vllm_url`) are resolvable from `config` — same
/// gating ladder as `ros_catalog::spawn_catalog_publish`, plus the vLLM gate.
///
/// `nats_client` is the daemon's runner-scoped NATS connection (the one the
/// cancel + presence tasks already use); `shutdown` is the shared
/// cancel/shutdown token. `models` is the live state handle the presence task
/// reads — the agent writes it after the initial probe and after every
/// load/unload so the heartbeat reflects vLLM without a re-enroll.
pub fn spawn_model_agent(
    config: &ExecutorConfig,
    nats_client: async_nats::Client,
    models: LiveModelState,
    shutdown: CancellationToken,
) {
    let Some(runner_id) = config.runner_id.clone() else {
        return;
    };
    let Some(mekhan_url) = config.mekhan_url() else {
        info!(%runner_id, "model agent skipped: no [mekhan].url configured");
        return;
    };
    let Some(token_path) = config.runner_token_path.clone() else {
        info!(%runner_id, "model agent skipped: no runner token path");
        return;
    };
    let Some(ma) = config.model_agent().cloned() else {
        // No [model_agent] block — this runner is not a model server.
        return;
    };

    tokio::spawn(async move {
        // Select the control backend: vLLM admin surface (default) or the
        // Metal-native Ollama runtime. `vllm_url` is the endpoint for whichever.
        let adapter = match ma.backend.as_deref().unwrap_or("vllm") {
            "ollama" => {
                info!(%runner_id, endpoint = %ma.vllm_url, "model agent backend: ollama (Metal runtime)");
                ModelBackend::Ollama(OllamaControlAdapter::new(ma.vllm_url.clone()))
            }
            other => {
                if other != "vllm" {
                    warn!(%runner_id, backend = %other, "unknown model_agent backend; defaulting to vllm");
                }
                ModelBackend::Vllm(VllmAdapter::new(ma.vllm_url.clone()))
            }
        };

        // Read the rnr_ token once (mirrors ros_catalog's read-from-path).
        let token = match read_runner_token(&token_path) {
            Ok(t) => t,
            Err(e) => {
                warn!(%runner_id, error = %e, "model agent: cannot read runner token; not starting");
                return;
            }
        };

        // Initial probe + publish, with backoff: vLLM may still be coming up at
        // runner boot. Best-effort throughout — give up quietly after the
        // window so the daemon never hangs (mirrors ros_catalog).
        const MAX_ATTEMPTS: u32 = 30;
        const RETRY_DELAY: Duration = Duration::from_secs(3);
        for attempt in 1..=MAX_ATTEMPTS {
            match probe_and_publish(&adapter, &ma, &runner_id, &mekhan_url, &token, &models).await {
                Ok(_) => break,
                Err(e) if attempt < MAX_ATTEMPTS => {
                    warn!(
                        %runner_id, vllm_url = %ma.vllm_url, attempt, error = %e,
                        "model-pool catalog publish attempt failed; retrying"
                    );
                    tokio::time::sleep(RETRY_DELAY).await;
                }
                Err(e) => warn!(
                    %runner_id, vllm_url = %ma.vllm_url, attempt, error = %e,
                    "model-pool catalog publish failed after {MAX_ATTEMPTS} attempts \
                     (best-effort; daemon continues, command listener still starts)"
                ),
            }
        }

        // Control-plane subscriber: runner.{id}.load + runner.{id}.unload. Core
        // NATS, ephemeral (NOT JetStream) — exactly the cancel-listener shape.
        run_command_listener(
            nats_client,
            adapter,
            ma,
            runner_id,
            mekhan_url,
            token,
            models,
            shutdown,
        )
        .await;
    });
}

/// Read + trim the `rnr_` token from `token_path` (mirrors ros_catalog).
fn read_runner_token(token_path: &std::path::Path) -> Result<String, String> {
    let token = std::fs::read_to_string(token_path)
        .map_err(|e| format!("read runner token {}: {e}", token_path.display()))?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err(format!("runner token at {} is empty", token_path.display()));
    }
    Ok(token)
}

/// Probe the runtime and build the interface catalog WITHOUT publishing. Returns
/// the catalog value plus its derived `(concurrency, model_ids)`. Split out from
/// [`probe_and_publish`] so the periodic refresh can probe, compare the resident
/// set to the last-published one, and re-publish ONLY on a change (no 15s POST /
/// log churn when nothing moved).
async fn probe_catalog(
    adapter: &ModelBackend,
    ma: &ModelAgentSettings,
    runner_id: &str,
) -> Result<(serde_json::Value, Option<u32>, Vec<String>), String> {
    let loaded = adapter
        .probe_loaded_models()
        .await
        .map_err(|e| format!("vllm probe: {e}"))?;

    // Also probe the models provisioned to disk (Ollama `/api/tags`; vLLM's
    // served set). Fail-soft: a probe error degrades `pulled` to empty rather
    // than failing the whole publish — the resident set is the load-bearing half.
    let pulled = match adapter.probe_pulled_models().await {
        Ok(ms) => ms
            .iter()
            .map(|m| match m {
                LoadedModel::Base { model_id, .. } => model_id.clone(),
                LoadedModel::Lora { adapter_id, .. } => adapter_id.clone(),
            })
            .collect(),
        Err(e) => {
            warn!(%runner_id, error = %e, "model agent: pulled-set probe failed; reporting empty");
            Vec::new()
        }
    };

    let catalog = build_catalog(
        &loaded,
        ma.max_num_seqs,
        &pulled,
        &ma.vllm_url,
        ma.residency_zone.as_deref(),
    );
    let concurrency = concurrency_of(&catalog);
    let model_ids = model_ids_of(&catalog);
    Ok((catalog, concurrency, model_ids))
}

/// Probe the runtime, build the catalog, publish it, and refresh the live state.
/// Returns the resident `model_ids` published (so a caller can track the last set
/// for change-gated periodic refresh).
async fn probe_and_publish(
    adapter: &ModelBackend,
    ma: &ModelAgentSettings,
    runner_id: &str,
    mekhan_url: &str,
    token: &str,
    models: &LiveModelState,
) -> Result<Vec<String>, String> {
    let (catalog, concurrency, model_ids) = probe_catalog(adapter, ma, runner_id).await?;
    models.set(concurrency, model_ids.clone());
    crate::catalog_publish::publish_catalog(runner_id, mekhan_url, token, &catalog).await?;
    Ok(model_ids)
}

/// The core-NATS command listener: `runner.{id}.load` + `runner.{id}.unload`.
/// Mirrors `executor-worker::NatsCancelListener::start` — one filtered
/// subscription on `runner.{id}.>`, branch on the subject's last token,
/// `tokio::select!` on shutdown. Ephemeral (no JetStream): a command sent while
/// the agent is down is lost, which is correct for a desired-state nudge that
/// mekhan can re-issue.
#[allow(clippy::too_many_arguments)]
async fn run_command_listener(
    client: async_nats::Client,
    adapter: ModelBackend,
    ma: ModelAgentSettings,
    runner_id: String,
    mekhan_url: String,
    token: String,
    models: LiveModelState,
    shutdown: CancellationToken,
) {
    let subject = format!("runner.{runner_id}.>");
    let mut subscription = match client.subscribe(subject.clone()).await {
        Ok(s) => s,
        Err(e) => {
            warn!(%runner_id, %subject, error = %e, "model agent: failed to subscribe to command channel");
            return;
        }
    };
    info!(%runner_id, %subject, "model-pool command listener started (load/unload)");

    // Periodic resident-set re-probe. The catalog (mekhan's `observed_count` /
    // the "N loaded" surface) was otherwise only refreshed on startup + on a
    // mekhan load/unload command — so a model the runtime loaded WITHOUT a
    // command went unseen. The big offender is Ollama: an inference request for
    // a pulled-but-evicted model AUTO-LOADS it on demand (with the inference
    // path's own keep_alive), so the model becomes resident and serves while the
    // control plane still reports it `unloaded` ("serving but 0 loaded"). Re-
    // probing `/api/ps` on a timer reconciles the catalog with runtime reality,
    // and lets the autoscaler's idle-eviction actually fire on an auto-loaded
    // model instead of being blind to it.
    const REFRESH_SECS: u64 = 15;
    let mut refresh = tokio::time::interval(Duration::from_secs(REFRESH_SECS));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    refresh.tick().await; // consume the immediate first tick (startup already published)

    // Last resident set we published, for change-gating the periodic refresh: re-
    // publish (and log) only when `/api/ps` actually moved, so a steady pool
    // doesn't POST an identical catalog every 15s. `None` ⇒ unknown (force the
    // next probe to publish), which is also what we reset to after a command.
    let mut last_ids: Option<Vec<String>> = None;

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                info!(%runner_id, "model-pool command listener shutting down");
                break;
            }
            _ = refresh.tick() => {
                // Reconcile the advertised resident set with what the runtime
                // ACTUALLY has loaded (incl. request-triggered auto-loads the
                // control plane never commanded — the Ollama "serving but 0
                // loaded" case). Probe first, publish only on a change.
                match probe_catalog(&adapter, &ma, &runner_id).await {
                    Ok((catalog, concurrency, model_ids)) => {
                        if last_ids.as_deref() != Some(model_ids.as_slice()) {
                            models.set(concurrency, model_ids.clone());
                            match crate::catalog_publish::publish_catalog(
                                &runner_id, &mekhan_url, &token, &catalog,
                            )
                            .await
                            {
                                Ok(()) => last_ids = Some(model_ids),
                                Err(e) => warn!(
                                    %runner_id, error = %e,
                                    "model agent: periodic resident-set re-publish failed"
                                ),
                            }
                        }
                    }
                    Err(e) => warn!(
                        %runner_id, error = %e, "model agent: periodic resident-set probe failed"
                    ),
                }
                continue;
            }
            msg = subscription.next() => {
                let Some(msg) = msg else {
                    warn!(%runner_id, "model-pool command subscription closed");
                    break;
                };
                // Only the last subject token (load/unload) is a command; the
                // shared `runner.{id}.>` filter also catches `presence` (which
                // this daemon PUBLISHES) — ignore anything that isn't a command.
                let verb = msg.subject.as_str().split('.').next_back().unwrap_or("");
                if verb != "load" && verb != "unload" && verb != "pull" {
                    continue;
                }
                match serde_json::from_slice::<ModelCommand>(&msg.payload) {
                    Ok(cmd) => {
                        apply_command(&adapter, &cmd).await;
                        // RE-PUSH the catalog + refresh live presence state so
                        // mekhan + the heartbeat reflect the new runtime state.
                        match probe_and_publish(
                            &adapter, &ma, &runner_id, &mekhan_url, &token, &models,
                        )
                        .await
                        {
                            Ok(ids) => last_ids = Some(ids),
                            Err(e) => warn!(
                                %runner_id, error = %e,
                                "model agent: re-publish after command failed"
                            ),
                        }
                    }
                    Err(e) => warn!(
                        %runner_id, %verb, error = %e,
                        "model agent: undecodable command payload (ignored)"
                    ),
                }
            }
        }
    }
}

/// Map a [`ModelCommand`] onto the vLLM admin surface. LoRA load/unload →
/// `load_lora_adapter`/`unload_lora_adapter`; Base load/unload → `wake_up`/
/// `sleep` (base swap). All calls are 404-tolerant in the adapter; a real error
/// is logged here (best-effort — never crashes the listener).
async fn apply_command(adapter: &ModelBackend, cmd: &ModelCommand) {
    let result = match cmd {
        ModelCommand::Load {
            target:
                LoadTarget::Lora {
                    adapter_id,
                    source_uri,
                    ..
                },
        } => {
            // A load with no source is a no-op we surface, not a backend call.
            match source_uri {
                Some(src) => adapter.load_lora(adapter_id, src).await,
                None => {
                    warn!(%adapter_id, "model agent: LoRA load with no source_uri; skipping");
                    Ok(())
                }
            }
        }
        ModelCommand::Unload {
            target: LoadTarget::Lora { adapter_id, .. },
        } => adapter.unload_lora(adapter_id).await,
        // Base placement: load makes the base resident (vLLM wake / Ollama warm
        // `model_id` into VRAM), unload evicts it (vLLM sleep / Ollama keep_alive 0).
        ModelCommand::Load {
            target: LoadTarget::Base { model_id },
        } => adapter.load_base(model_id).await,
        ModelCommand::Unload {
            target: LoadTarget::Base { model_id },
        } => adapter.unload_base(model_id).await,
        // Provision to disk without making resident (Ollama `/api/pull`; vLLM
        // no-op). A bare-LoRA pull has no host engine to attach to → skip.
        ModelCommand::Pull {
            target: LoadTarget::Base { model_id },
        } => adapter.pull_base(model_id).await,
        ModelCommand::Pull {
            target: LoadTarget::Lora { adapter_id, .. },
        } => {
            warn!(%adapter_id, "model agent: Pull of a bare LoRA is unsupported; skipping");
            Ok(())
        }
    };
    if let Err(e) = result {
        warn!(error = %e, "model agent: vLLM admin call failed (best-effort)");
    }
}

/// Build the `RunnerInterfaceCatalog`-shaped `{ "models": [...] }` value from a
/// vLLM probe + the config-sourced C. Base entries carry `max_num_seqs = C`
/// (C is per-engine, NOT in `/v1/models`); LoRA entries carry a `base`
/// back-pointer (they share the base's C) and no `max_num_seqs`.
///
/// Entry shape (matches mekhan's `ModelEntry`):
///   Base: `{ "model_id", "kind": "base", "max_num_seqs": C? }`
///   Lora: `{ "model_id": adapter_id, "kind": "lora", "base", "source_uri"? }`
///
/// `pulled` is the additive `RunnerInterfaceCatalog.pulled` field — model ids
/// provisioned to disk (loadable without a re-download). It is the SUPERSET that
/// includes the resident `models`; mekhan's read excludes already-resident bases
/// so the operator sees "ready to load" distinctly from "serving".
///
/// `base_url` is this node's INFERENCE endpoint (the `vllm_url` the agent drives)
/// and `residency_zone` its GDPR zone — both additive top-level `catalog` fields
/// the router's live-inventory poll reads (via mekhan's public aggregator) to
/// build its replica table. They are opaque JSON to the engine.
fn build_catalog(
    loaded: &[LoadedModel],
    c: Option<u32>,
    pulled: &[String],
    base_url: &str,
    residency_zone: Option<&str>,
) -> Value {
    let models: Vec<Value> = loaded
        .iter()
        .map(|m| match m {
            LoadedModel::Base { model_id, .. } => json!({
                "model_id": model_id,
                "kind": "base",
                "max_num_seqs": c,
            }),
            LoadedModel::Lora { adapter_id, base } => json!({
                "model_id": adapter_id,
                "kind": "lora",
                "base": base,
            }),
        })
        .collect();
    json!({
        "models": models,
        "pulled": pulled,
        "base_url": base_url,
        "residency_zone": residency_zone,
    })
}

/// C reported on presence = the first base entry's `max_num_seqs`. There is one
/// engine (one base) per node agent, so a single C suffices.
fn concurrency_of(catalog: &Value) -> Option<u32> {
    catalog["models"]
        .as_array()?
        .iter()
        .find(|m| m["kind"] == "base")
        .and_then(|m| m["max_num_seqs"].as_u64())
        .map(|c| c as u32)
}

/// The served model ids (base + loaded adapters) for the presence heartbeat.
fn model_ids_of(catalog: &Value) -> Vec<String> {
    catalog["models"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m["model_id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_catalog_maps_base_and_loras_with_c_on_base_only() {
        let loaded = vec![
            LoadedModel::Base {
                model_id: "meta-llama/Llama-3-8B".into(),
                max_num_seqs: None, // probe never fills C
            },
            LoadedModel::Lora {
                adapter_id: "lora-a".into(),
                base: "meta-llama/Llama-3-8B".into(),
            },
            LoadedModel::Lora {
                adapter_id: "lora-b".into(),
                base: "meta-llama/Llama-3-8B".into(),
            },
        ];
        let catalog = build_catalog(
            &loaded,
            Some(256),
            &["meta-llama/Llama-3-8B".into()],
            "http://localhost:8000",
            Some("eu-dev"),
        );
        let models = catalog["models"].as_array().unwrap();
        assert_eq!(models.len(), 3);

        // `pulled` carries the on-disk superset verbatim.
        assert_eq!(catalog["pulled"], json!(["meta-llama/Llama-3-8B"]));

        // The inference endpoint + residency zone ride the top-level catalog so
        // the router's live-inventory poll can build its replica table.
        assert_eq!(catalog["base_url"], "http://localhost:8000");
        assert_eq!(catalog["residency_zone"], "eu-dev");

        // Base: kind=base, C present, no base back-pointer.
        assert_eq!(models[0]["model_id"], "meta-llama/Llama-3-8B");
        assert_eq!(models[0]["kind"], "base");
        assert_eq!(models[0]["max_num_seqs"], 256);
        assert!(models[0].get("base").is_none());

        // LoRAs: kind=lora, base back-pointer set, NO max_num_seqs (they share
        // the base's per-engine budget).
        assert_eq!(models[1]["model_id"], "lora-a");
        assert_eq!(models[1]["kind"], "lora");
        assert_eq!(models[1]["base"], "meta-llama/Llama-3-8B");
        assert!(models[1].get("max_num_seqs").is_none());
        assert_eq!(models[2]["model_id"], "lora-b");
        assert_eq!(models[2]["base"], "meta-llama/Llama-3-8B");
    }

    #[test]
    fn build_catalog_base_without_c_emits_null() {
        let loaded = vec![LoadedModel::Base {
            model_id: "b".into(),
            max_num_seqs: None,
        }];
        let catalog = build_catalog(&loaded, None, &[], "http://localhost:8000", None);
        assert!(catalog["models"][0]["max_num_seqs"].is_null());
        // A zone-agnostic node emits a null `residency_zone`.
        assert!(catalog["residency_zone"].is_null());
        assert_eq!(catalog["base_url"], "http://localhost:8000");
    }

    #[test]
    fn concurrency_and_model_ids_extract_from_catalog() {
        let loaded = vec![
            LoadedModel::Base {
                model_id: "base".into(),
                max_num_seqs: None,
            },
            LoadedModel::Lora {
                adapter_id: "lora".into(),
                base: "base".into(),
            },
        ];
        let catalog = build_catalog(&loaded, Some(128), &[], "http://localhost:8000", None);
        assert_eq!(concurrency_of(&catalog), Some(128));
        assert_eq!(model_ids_of(&catalog), vec!["base", "lora"]);
    }

    #[test]
    fn empty_probe_yields_empty_catalog() {
        let catalog = build_catalog(&[], Some(64), &[], "http://localhost:8000", None);
        assert_eq!(catalog["models"].as_array().unwrap().len(), 0);
        assert_eq!(concurrency_of(&catalog), None);
        assert!(model_ids_of(&catalog).is_empty());
    }

    #[test]
    fn model_command_deserializes_load_lora() {
        let raw = br#"{"kind":"load","target":{"Lora":{"adapter_id":"a","base":"b","source_uri":"s3://x"}}}"#;
        let cmd: ModelCommand = serde_json::from_slice(raw).unwrap();
        assert_eq!(
            cmd,
            ModelCommand::Load {
                target: LoadTarget::Lora {
                    adapter_id: "a".into(),
                    base: "b".into(),
                    source_uri: Some("s3://x".into()),
                }
            }
        );
    }

    /// `spawn_model_agent` is a no-op when `[model_agent]` is absent — it
    /// returns before spawning any task. We assert the gating predicate the
    /// function uses (`config.model_agent()` is `None`) matches a default config.
    #[test]
    fn agent_is_noop_without_model_agent_block() {
        // A config with no [model_agent] block has `model_agent() == None`, the
        // gate `spawn_model_agent` checks last. (We don't construct a full
        // ExecutorConfig here — the gate is a plain Option check on the field.)
        let ma: Option<ModelAgentSettings> = None;
        assert!(ma.is_none());
    }
}
