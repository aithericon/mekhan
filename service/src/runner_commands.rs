//! The mekhan-side model load/unload PUBLISHER (docs/31 Phase 3, Loop 2).
//!
//! The placement controller (`crate::autoscaler::placement`) decides a model
//! should become resident on a running node (an adapter load, or a base wake) and
//! emits a [`ModelCommand`] on the node's runner-scoped control subject. The node
//! agent (executor `model_agent`) is the SUBSCRIBER — built + live (P2); only this
//! publisher was greenfield.
//!
//! ## Wire envelope — MIRROR, not a path-dep
//!
//! `executor/` is its own workspace root; `service` MUST NOT path-dep
//! `executor-llm` (the subscriber lives behind its `vllm` feature, and the
//! executor deploys standalone). Per the docs/31 dossier §D verdict, the
//! lower-risk option is to MIRROR the envelope: define wire-identical
//! [`ModelCommand`] / [`LoadTarget`] here and lock the JSON shape with a parity
//! test against the executor's documented contract
//! (`executor/crates/executor-llm/src/model_command.rs`). The wire JSON
//! (`{kind, target:{Base|Lora}}`, snake_case tag, optional `source_uri`) is the
//! immutable contract once shipped.
//!
//! ## Transport — CORE NATS, fire-and-forget
//!
//! The command rides the EXISTING `runner.{id}.>` SUBSCRIBE grant the runner JWT
//! already carries (no JWT re-mint — `runner.{id}.load` / `runner.{id}.unload`
//! fall under the `runner.{id}.>` wildcard). It is published on the CORE client
//! (`nats.client().publish`), NOT JetStream: ephemeral control, fire-and-forget,
//! idempotently re-issued every scheduler tick until the inventory/heartbeat
//! confirms the change — mirroring the presence-heartbeat plane, not the durable
//! engine-injection plane.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::nats::MekhanNats;

/// A load/unload command for the node agent. `kind` selects the verb; `target` is
/// what to act on (a base engine or a LoRA adapter).
///
/// WIRE-IDENTICAL mirror of `executor_llm::model_command::ModelCommand`. Serde
/// tag = `kind`, snake_case (`"load"` / `"unload"`); the parity test below locks
/// the exact JSON against the executor's documented envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ModelCommand {
    /// Make `target` resident: LoRA → load adapter; Base → wake.
    Load { target: LoadTarget },
    /// Make `target` absent: LoRA → unload adapter; Base → sleep.
    Unload { target: LoadTarget },
    /// **Provision** `target` to disk without making it resident (Ollama
    /// `/api/pull`; a no-op capability gap on vLLM, whose base is fixed at launch).
    /// Carries a `Base` target — a later `Load` then warms it cheaply.
    Pull { target: LoadTarget },
}

/// What a [`ModelCommand`] acts on. A LoRA MUST carry its `base` back-pointer —
/// `C` (`max_num_seqs`) is per-engine (per base), shared across that base's
/// adapters.
///
/// WIRE-IDENTICAL mirror of `executor_llm::model_command::LoadTarget` (externally
/// tagged enum — `{"Base":{...}}` / `{"Lora":{...}}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum LoadTarget {
    /// A base engine, addressed by its served model id.
    Base { model_id: String },
    /// A LoRA adapter attached to `base`. `source_uri` is where to fetch the
    /// adapter weights from (optional on unload).
    Lora {
        adapter_id: String,
        base: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_uri: Option<String>,
    },
}

/// Publish a [`ModelCommand`] to `runner_id` on its runner-scoped CORE subject
/// (`runner.{id}.load` for a `Load`, `runner.{id}.unload` for an `Unload`).
///
/// Fire-and-forget: there is no ack, no JetStream. A serialize failure is logged
/// and skipped (the next tick re-issues — placement is desired-state, not an RPC).
/// Returns `Ok(())` on a successful publish, `Err` only on the NATS transport
/// error (the caller fails-soft per tick).
pub async fn publish_model_command(
    nats: &MekhanNats,
    runner_id: Uuid,
    cmd: &ModelCommand,
) -> Result<(), async_nats::Error> {
    let verb = match cmd {
        ModelCommand::Load { .. } => "load",
        ModelCommand::Unload { .. } => "unload",
        ModelCommand::Pull { .. } => "pull",
    };
    let subject = format!("runner.{runner_id}.{verb}");

    let payload = match serde_json::to_vec(cmd) {
        Ok(bytes) => bytes,
        Err(e) => {
            // Never panic on a serialize miss; the next tick retries.
            tracing::warn!(%runner_id, "skipping unserializable ModelCommand: {e}");
            return Ok(());
        }
    };

    nats.client().publish(subject, payload.into()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mirror MUST serialize to the EXACT wire shape the executor subscriber
    /// deserializes (`executor/crates/executor-llm/src/model_command.rs`):
    /// `{ "kind": "load", "target": { "Lora": { adapter_id, base, source_uri } } }`.
    #[test]
    fn load_lora_serializes_to_executor_envelope() {
        let cmd = ModelCommand::Load {
            target: LoadTarget::Lora {
                adapter_id: "my-lora".into(),
                base: "meta-llama/Llama-3-8B".into(),
                source_uri: Some("s3://bucket/my-lora".into()),
            },
        };
        let v: serde_json::Value = serde_json::to_value(&cmd).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "kind": "load",
                "target": { "Lora": {
                    "adapter_id": "my-lora",
                    "base": "meta-llama/Llama-3-8B",
                    "source_uri": "s3://bucket/my-lora"
                } }
            })
        );
    }

    #[test]
    fn unload_base_serializes_to_executor_envelope() {
        let cmd = ModelCommand::Unload {
            target: LoadTarget::Base {
                model_id: "meta-llama/Llama-3-8B".into(),
            },
        };
        let v: serde_json::Value = serde_json::to_value(&cmd).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "kind": "unload",
                "target": { "Base": { "model_id": "meta-llama/Llama-3-8B" } }
            })
        );
    }

    #[test]
    fn pull_base_serializes_to_executor_envelope() {
        // Wire-identical to the executor mirror's `pull_base_serializes_*` test.
        let cmd = ModelCommand::Pull {
            target: LoadTarget::Base {
                model_id: "llama3.2:1b".into(),
            },
        };
        let v: serde_json::Value = serde_json::to_value(&cmd).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "kind": "pull",
                "target": { "Base": { "model_id": "llama3.2:1b" } }
            })
        );
    }

    #[test]
    fn unload_lora_omits_absent_source_uri() {
        // `skip_serializing_if = "Option::is_none"` ⇒ `source_uri` absent on the
        // wire when None (matches the executor `unload_lora_tolerates_absent_source_uri`).
        let cmd = ModelCommand::Unload {
            target: LoadTarget::Lora {
                adapter_id: "my-lora".into(),
                base: "b".into(),
                source_uri: None,
            },
        };
        let v: serde_json::Value = serde_json::to_value(&cmd).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "kind": "unload",
                "target": { "Lora": { "adapter_id": "my-lora", "base": "b" } }
            })
        );
        assert!(v["target"]["Lora"].get("source_uri").is_none());
    }

    #[test]
    fn command_roundtrips_through_json() {
        let cmd = ModelCommand::Load {
            target: LoadTarget::Lora {
                adapter_id: "a".into(),
                base: "b".into(),
                source_uri: Some("uri".into()),
            },
        };
        let bytes = serde_json::to_vec(&cmd).unwrap();
        let back: ModelCommand = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(cmd, back);
    }
}
