//! Wire DTOs for the model load/unload command channel (P2 — model-pool).
//!
//! These are the serde shapes carried on the **core-NATS** subjects
//! `runner.{id}.load` / `runner.{id}.unload` (inside the runner JWT's
//! `SUB runner.{id}.>` grant). They are CONTROL-PLANE ONLY: a command asks the
//! node agent to load/unload a base or LoRA on its local vLLM. Inference NEVER
//! travels this channel — it is conventional OpenAI HTTP straight to vLLM,
//! never net-admitted, never routed here.
//!
//! No HTTP, no engine, no JetStream — pure payload. This module is shared so the
//! (later, out-of-scope) mekhan-side publisher and this subscriber agree on the
//! envelope. This phase ships only the SUBSCRIBER (the node agent); the
//! publisher is mekhan-side greenfield.
//!
//! Envelope (matches docs/29 §4):
//! ```json
//! { "kind": "load",
//!   "target": { "Lora": { "adapter_id": "my-lora", "base": "<base>",
//!                         "source_uri": "s3://..." } } }
//! ```

use serde::{Deserialize, Serialize};

/// A load/unload/pull command for the node agent. `kind` selects the verb;
/// `target` is what to act on (a base engine or a LoRA adapter).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ModelCommand {
    /// Make `target` resident: LoRA → `load_lora_adapter`; Base → `wake_up`.
    Load { target: LoadTarget },
    /// Make `target` absent: LoRA → `unload_lora_adapter`; Base → `sleep`.
    Unload { target: LoadTarget },
    /// **Provision** `target` onto the node's local engine WITHOUT making it
    /// resident — fetch the weights to disk so a later `Load` is cheap. The
    /// Metal-native (Ollama) path is `POST /api/pull`; on vLLM, where the base is
    /// fixed at engine launch and HF weights are pulled then, this is a logged
    /// capability gap (no-op). A `Pull` carries a `Base` target (pulling a bare
    /// LoRA without a host engine is meaningless).
    Pull { target: LoadTarget },
}

/// What a [`ModelCommand`] acts on. A LoRA MUST carry its `base` back-pointer —
/// C (`max_num_seqs`) is per-engine (per base), shared across that base's
/// adapters, so the control plane and router always know which engine an
/// adapter contends for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoadTarget {
    /// A base engine, addressed by its served model id.
    Base { model_id: String },
    /// A LoRA adapter attached to `base`. `source_uri` is where to fetch the
    /// adapter weights from (optional on unload, where only `adapter_id` and
    /// `base` matter).
    Lora {
        adapter_id: String,
        base: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_uri: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_lora_deserializes_from_docs_envelope() {
        let raw = serde_json::json!({
            "kind": "load",
            "target": { "Lora": {
                "adapter_id": "my-lora",
                "base": "meta-llama/Llama-3-8B",
                "source_uri": "s3://bucket/my-lora"
            } }
        });
        let cmd: ModelCommand = serde_json::from_value(raw).unwrap();
        assert_eq!(
            cmd,
            ModelCommand::Load {
                target: LoadTarget::Lora {
                    adapter_id: "my-lora".into(),
                    base: "meta-llama/Llama-3-8B".into(),
                    source_uri: Some("s3://bucket/my-lora".into()),
                }
            }
        );
    }

    #[test]
    fn unload_base_deserializes() {
        let raw = serde_json::json!({
            "kind": "unload",
            "target": { "Base": { "model_id": "meta-llama/Llama-3-8B" } }
        });
        let cmd: ModelCommand = serde_json::from_value(raw).unwrap();
        assert_eq!(
            cmd,
            ModelCommand::Unload {
                target: LoadTarget::Base {
                    model_id: "meta-llama/Llama-3-8B".into(),
                }
            }
        );
    }

    #[test]
    fn unload_lora_tolerates_absent_source_uri() {
        let raw = serde_json::json!({
            "kind": "unload",
            "target": { "Lora": { "adapter_id": "my-lora", "base": "b" } }
        });
        let cmd: ModelCommand = serde_json::from_value(raw).unwrap();
        assert_eq!(
            cmd,
            ModelCommand::Unload {
                target: LoadTarget::Lora {
                    adapter_id: "my-lora".into(),
                    base: "b".into(),
                    source_uri: None,
                }
            }
        );
    }

    #[test]
    fn pull_base_serializes_with_snake_case_kind() {
        let cmd = ModelCommand::Pull {
            target: LoadTarget::Base {
                model_id: "llama3.2:1b".into(),
            },
        };
        let v = serde_json::to_value(&cmd).unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "kind": "pull", "target": { "Base": { "model_id": "llama3.2:1b" } } })
        );
        let back: ModelCommand = serde_json::from_value(v).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn load_command_roundtrips_through_json() {
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
        // `kind` is the discriminant tag.
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["kind"], "load");
    }
}
