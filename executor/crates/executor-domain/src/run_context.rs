use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::event::StagedEvent;
use crate::job::ExecutionSpec;
use crate::run_dir::RunDirectory;

/// Context passed to backends for execution. Accumulated by staging hooks.
///
/// This is pure data — no I/O methods. Backends read from this to configure
/// the execution environment (env vars, working dir, timeout, etc.).
///
/// ## Secrets — on-disk vs in-memory separation
///
/// `{{secret:KEY}}` patterns are resolved by the staging pipeline. The resolved
/// plaintext is **never** serialized to disk (no `context.json` leak — Gap #1).
/// The on-disk shape carries only the unresolved templates:
///
/// * `env`, `spec.config`, `spec.inputs[].source.value`,
///   `spec.inputs[].source.storage`, `spec.outputs[].upload_to.storage` —
///   serialized, may contain `{{secret:KEY}}`
/// * `resolved_env`, `resolved_config`, `resolved_input_storage`,
///   `resolved_output_storage`, `resolved_inline_inputs` — `#[serde(skip)]`,
///   populated by `PlanSecretsHook`
///
/// Backends read from the `resolved_*` side-channel when spawning child
/// processes (`Command::env(k, v)`) or making outbound HTTP requests. The
/// child process is the only sink for plaintext secrets.
#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunContext {
    /// Execution identifier.
    pub execution_id: String,

    /// What to execute. Carries unresolved `{{secret:KEY}}` templates.
    pub spec: ExecutionSpec,

    /// Structured run directory paths.
    pub run_dir: RunDirectory,

    /// Execution timeout.
    #[serde(with = "crate::serde_duration")]
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub timeout: Duration,

    /// Accumulated environment variables (from spec + hooks + backend).
    ///
    /// Serialized to `context.json`. May contain `{{secret:KEY}}` templates.
    /// Backends MUST NOT read from this when spawning children; use
    /// `resolved_env` instead.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Resolved env values for child spawn. NEVER serialized.
    ///
    /// Populated by `PlanSecretsHook`. Contains the plaintext secret values
    /// keyed by env-var name. Feed this — not `env` — into
    /// `tokio::process::Command::env(k, v)`.
    #[serde(skip)]
    pub resolved_env: HashMap<String, String>,

    /// Resolved `spec.config` overlay. NEVER serialized.
    ///
    /// `None` means no resolution was needed (no `{{secret:...}}` patterns
    /// were present); callers fall back to `spec.config`. The HTTP backend is
    /// the only consumer — it has no spawned child to receive env vars.
    #[serde(skip)]
    pub resolved_config: Option<serde_json::Value>,

    /// Resolved per-input storage configs. NEVER serialized.
    ///
    /// Keyed by input name. Used by `StageInputsHook` instead of
    /// `spec.inputs[].source.storage` when present.
    #[serde(skip)]
    pub resolved_input_storage: HashMap<String, serde_json::Value>,

    /// Resolved per-output upload storage configs. NEVER serialized.
    ///
    /// Keyed by output name. Used by the output-upload sweep instead of
    /// `spec.outputs[].upload_to.storage` when present.
    #[serde(skip)]
    pub resolved_output_storage: HashMap<String, serde_json::Value>,

    /// Resolved inline input JSON values. NEVER serialized.
    ///
    /// Keyed by input name. Populated for any `spec.inputs[].source =
    /// Inline { value }` whose `value` carried `{{secret:KEY}}` templates.
    /// `StageInputsHook` reads this instead of `value` when present, so the
    /// staged input file on disk receives plaintext while `spec.inputs[]`
    /// (and therefore `context.json`) keeps the unresolved template.
    ///
    /// This is the path the compiler-emitted `__resources["<slug>"]` envelope
    /// flows through (Python AutomatedSteps read `<slug>.json` directly).
    #[serde(skip)]
    pub resolved_inline_inputs: HashMap<String, serde_json::Value>,

    /// Metadata echoed through status updates.
    #[serde(default)]
    pub metadata: HashMap<String, String>,

    /// Staged input files: name → local path.
    #[serde(default)]
    pub staged_inputs: HashMap<String, PathBuf>,

    /// Expected output files: name → relative path in outputs_dir.
    #[serde(default)]
    pub expected_outputs: HashMap<String, PathBuf>,

    /// Events collected during staging, flushed after StreamContext is built.
    #[serde(default)]
    pub staged_events: Vec<StagedEvent>,

    /// Opaque backend-specific state.
    #[serde(default)]
    pub backend_state: serde_json::Value,
}

/// Hand-written `Debug` impl that elides the `resolved_*` fields.
///
/// Defense in depth against accidental `tracing::debug!(?ctx, …)` writing
/// plaintext secrets into structured logs.
impl fmt::Debug for RunContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunContext")
            .field("execution_id", &self.execution_id)
            .field("spec", &self.spec)
            .field("run_dir", &self.run_dir)
            .field("timeout", &self.timeout)
            .field("env", &self.env)
            .field(
                "resolved_env",
                &format_args!("<{} resolved entries, elided>", self.resolved_env.len()),
            )
            .field(
                "resolved_config",
                &format_args!(
                    "<{}, elided>",
                    if self.resolved_config.is_some() {
                        "resolved"
                    } else {
                        "none"
                    }
                ),
            )
            .field(
                "resolved_input_storage",
                &format_args!(
                    "<{} resolved entries, elided>",
                    self.resolved_input_storage.len()
                ),
            )
            .field(
                "resolved_output_storage",
                &format_args!(
                    "<{} resolved entries, elided>",
                    self.resolved_output_storage.len()
                ),
            )
            .field(
                "resolved_inline_inputs",
                &format_args!(
                    "<{} resolved entries, elided>",
                    self.resolved_inline_inputs.len()
                ),
            )
            .field("metadata", &self.metadata)
            .field("staged_inputs", &self.staged_inputs)
            .field("expected_outputs", &self.expected_outputs)
            .field("staged_events", &self.staged_events)
            .field("backend_state", &self.backend_state)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_context_serde_roundtrip() {
        let ctx = RunContext {
            execution_id: "exec-789".into(),
            spec: ExecutionSpec {
                backend: "process".into(),
                inputs: vec![],
                outputs: vec![],
                config: serde_json::json!({
                    "command": "python3",
                    "args": ["train.py"],
                    "inherit_env": true
                }),
                    config_ref: None,
            },
            run_dir: RunDirectory::new(&PathBuf::from("/tmp"), "exec-789"),
            timeout: Duration::from_secs(3600),
            env: HashMap::from([("AITHERICON_EXECUTION_ID".into(), "exec-789".into())]),
            resolved_env: HashMap::new(),
            resolved_config: None,
            resolved_input_storage: HashMap::new(),
            resolved_output_storage: HashMap::new(),
            resolved_inline_inputs: HashMap::new(),
            metadata: HashMap::from([("user".into(), "alice".into())]),
            staged_inputs: Default::default(),
            expected_outputs: Default::default(),
            staged_events: vec![],
            backend_state: serde_json::Value::Null,
        };

        let json = serde_json::to_string(&ctx).unwrap();
        let deserialized: RunContext = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.execution_id, "exec-789");
        assert_eq!(deserialized.timeout, Duration::from_secs(3600));
    }

    /// Round-tripping a `RunContext` through JSON must drop every
    /// `resolved_*` field. They are populated only in-memory by the
    /// staging hooks. Defense against accidental on-disk leakage.
    #[test]
    fn resolved_fields_do_not_round_trip() {
        let mut ctx = RunContext {
            execution_id: "rt-test".into(),
            spec: ExecutionSpec {
                backend: "process".into(),
                inputs: vec![],
                outputs: vec![],
                config: serde_json::json!({"command": "true"}),
                config_ref: None,
            },
            run_dir: RunDirectory::new(&PathBuf::from("/tmp"), "rt-test"),
            timeout: Duration::from_secs(60),
            env: HashMap::new(),
            resolved_env: HashMap::from([("API_KEY".into(), "PLAINTEXT-VALUE-XYZ".into())]),
            resolved_config: Some(serde_json::json!({"token": "PLAINTEXT-VALUE-XYZ"})),
            resolved_input_storage: HashMap::from([(
                "i1".into(),
                serde_json::json!({"key": "PLAINTEXT-VALUE-XYZ"}),
            )]),
            resolved_output_storage: HashMap::from([(
                "o1".into(),
                serde_json::json!({"key": "PLAINTEXT-VALUE-XYZ"}),
            )]),
            resolved_inline_inputs: HashMap::from([(
                "local_pg.json".into(),
                serde_json::json!({"password": "PLAINTEXT-VALUE-XYZ"}),
            )]),
            metadata: HashMap::new(),
            staged_inputs: Default::default(),
            expected_outputs: Default::default(),
            staged_events: vec![],
            backend_state: serde_json::Value::Null,
        };

        let json = serde_json::to_string(&ctx).unwrap();
        assert!(
            !json.contains("PLAINTEXT-VALUE-XYZ"),
            "resolved_* plaintext leaked into JSON: {json}"
        );
        assert!(!json.contains("resolved_env"));
        assert!(!json.contains("resolved_config"));
        assert!(!json.contains("resolved_input_storage"));
        assert!(!json.contains("resolved_output_storage"));
        assert!(!json.contains("resolved_inline_inputs"));

        let deserialized: RunContext = serde_json::from_str(&json).unwrap();
        assert!(deserialized.resolved_env.is_empty());
        assert!(deserialized.resolved_config.is_none());
        assert!(deserialized.resolved_input_storage.is_empty());
        assert!(deserialized.resolved_output_storage.is_empty());
        assert!(deserialized.resolved_inline_inputs.is_empty());

        // Sanity: the Debug impl must not include plaintext either.
        ctx.resolved_env.insert("K".into(), "SECRET".into());
        let dbg = format!("{ctx:?}");
        assert!(
            !dbg.contains("SECRET"),
            "Debug impl leaked resolved_env plaintext: {dbg}"
        );
    }
}
