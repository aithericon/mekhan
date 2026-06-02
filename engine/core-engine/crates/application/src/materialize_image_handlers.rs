//! Materialize-image effect handler (docs/22 — the control plane's
//! `materialize_image` INLINE engine effect).
//!
//! Structurally symmetric with [`crate::stage_template_handlers`]: a stateless
//! handler holding an `Arc<dyn AllocatorClient>` that routes the per-fire
//! `effect_config` (the same `DatacenterConnection.effect_config()` JSON the
//! lease/stage paths use; `firing.rs` resolved `{{secret:…}}` into plaintext
//! BEFORE `execute()` runs) to the right flavor allocator and PULLS an OCI image
//! to an Apptainer `.sif` on that cluster.
//!
//!   - **Slurm:** `apptainer pull` over SSH on the login node → a
//!     content-addressed `/shared/sif/<digest>.sif`, with the stable by-ref
//!     symlink repointed. `remote_ref` analogue = the `.sif` path.
//!   - **Nomad / HTTP:** unsupported (those use native container drivers) — the
//!     allocator leg returns an error, recorded as `status:"failed"` DATA.
//!
//! ## Failure model (the load-bearing contract)
//!
//! A materialization *cluster* failure (pull 4xx/5xx, SSH error, no disk, …) is
//! recorded as `status:"failed"` DATA on the `materialized` port AND in
//! `effect_result` — the materialize net completes CLEANLY so mekhan's
//! projection records the failure. It is NOT a `NetFailed`. Only truly-fatal
//! config/parse errors (missing request fields, no connection) return
//! `Err(EffectError::Fatal(…))`.
//!
//! ## Replay
//!
//! [`replay`](EffectHandler::replay) is a no-op (stateless — the result lives in
//! the journaled produced token, re-emitted on replay; the cluster is NOT
//! re-pulled). The pull is idempotent at the allocator leg (content-addressed
//! `.sif`; an identical image lands at the same digest path).

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value as JsonValue;

use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};
use crate::resource_lease_handlers::{AllocatorClient, MaterializeImageArgs, MaterializeRequestToken};

/// Pulls an OCI image to an Apptainer `.sif` on an external cluster and emits the
/// typed materialization-result token.
///
/// Input port (`request`): [`MaterializeRequestToken`] —
/// `{ materialize_id, image_ref, registry_username?, registry_password? }`.
/// Output port (`materialized`) AND `effect_result` (IDENTICAL JSON):
/// `{ materialize_id, status, digest, sif_path, size_bytes, error }`.
pub struct MaterializeImageHandler {
    client: Arc<dyn AllocatorClient>,
    input_port: String,
    output_port: String,
}

impl MaterializeImageHandler {
    pub fn new(
        client: Arc<dyn AllocatorClient>,
        input_port: impl Into<String>,
        output_port: impl Into<String>,
    ) -> Self {
        Self {
            client,
            input_port: input_port.into(),
            output_port: output_port.into(),
        }
    }

    /// Build the result JSON. Used for BOTH success and failure so the output
    /// token and `effect_result` are byte-identical (single source of truth).
    fn result_token(
        materialize_id: &str,
        status: &str,
        digest: Option<&str>,
        sif_path: Option<&str>,
        size_bytes: Option<i64>,
        error: Option<&str>,
    ) -> JsonValue {
        serde_json::json!({
            "materialize_id": materialize_id,
            "status": status,
            "digest": digest,
            "sif_path": sif_path,
            "size_bytes": size_bytes,
            "error": error,
        })
    }
}

#[async_trait::async_trait]
impl EffectHandler for MaterializeImageHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let token = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in materialize_image handler",
                self.input_port
            ))
        })?;

        // Parse the typed request token (correlation id only). Missing → FATAL
        // author/compiler error.
        let req: MaterializeRequestToken = serde_json::from_value(token.clone()).map_err(|e| {
            EffectError::Fatal(format!("materialize_image request is not valid: {e}"))
        })?;

        if req.materialize_id.is_empty() {
            return Err(EffectError::Fatal(
                "materialize_image request missing materialize_id".into(),
            ));
        }

        // The full resolved effect_config: the datacenter connection (where to
        // pull) PLUS `image_ref` + (secret-resolved) registry credentials.
        // Absent → FATAL: nothing to pull / nowhere to pull it.
        let config = input.config.clone().ok_or_else(|| {
            EffectError::Fatal(
                "materialize_image handler requires effect_config (connection + image_ref)".into(),
            )
        })?;

        let image_ref = config
            .get("image_ref")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                EffectError::Fatal("materialize_image effect_config missing image_ref".into())
            })?
            .to_string();
        // Credentials are optional (public images) and arrive already
        // secret-resolved by `firing.rs`.
        let registry_username = config
            .get("registry_username")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        let registry_password = config
            .get("registry_password")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        let args = MaterializeImageArgs {
            image_ref: image_ref.clone(),
            registry_username,
            registry_password,
        };

        // Route to the resolved cluster's allocator. A cluster failure is DATA,
        // not a NetFailed: record `status:"failed"` and complete cleanly.
        let result_token = match self
            .client
            .materialize_image_with_connection(&config, &args)
            .await
        {
            Ok(outcome) => {
                tracing::info!(
                    materialize_id = %req.materialize_id,
                    image_ref = %image_ref,
                    digest = %outcome.digest,
                    sif_path = %outcome.sif_path,
                    "materialize_image: image materialized",
                );
                Self::result_token(
                    &req.materialize_id,
                    "ready",
                    Some(&outcome.digest),
                    Some(&outcome.sif_path),
                    outcome.size_bytes,
                    None,
                )
            }
            Err(e) => {
                let msg = e.to_string();
                tracing::warn!(
                    materialize_id = %req.materialize_id,
                    image_ref = %image_ref,
                    error = %msg,
                    "materialize_image: materialization failed (recorded as data, not NetFailed)",
                );
                Self::result_token(&req.materialize_id, "failed", None, None, None, Some(&msg))
            }
        };

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), result_token.clone());

        Ok(EffectOutput {
            tokens,
            // IDENTICAL to the output token — journaled so replay re-emits it
            // without re-pulling.
            result: result_token,
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless: the result lives entirely in the journaled produced token,
        // which the engine re-emits on replay. The cluster is NOT re-pulled.
    }

    fn name(&self) -> &str {
        "materialize_image"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource_lease_handlers::{
        AllocatorError, MaterializeOutcome, StageOutcome, StageTemplateArgs,
    };
    use petri_domain::TransitionId;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock allocator recording the materialize args, returning a canned outcome
    /// or erroring — so the handler's success-vs-failure-as-data and replay
    /// contracts can be asserted with no network.
    struct MockMaterializer {
        calls: AtomicUsize,
        last_args: std::sync::Mutex<Option<MaterializeImageArgs>>,
        last_config: std::sync::Mutex<Option<JsonValue>>,
        fail: bool,
    }

    impl MockMaterializer {
        fn ok() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                last_args: std::sync::Mutex::new(None),
                last_config: std::sync::Mutex::new(None),
                fail: false,
            })
        }
        fn failing() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                last_args: std::sync::Mutex::new(None),
                last_config: std::sync::Mutex::new(None),
                fail: true,
            })
        }
    }

    #[async_trait::async_trait]
    impl AllocatorClient for MockMaterializer {
        async fn acquire(
            &self,
            _allocator_url: &str,
            _token: &str,
            _grant_id: &str,
            _request: &JsonValue,
        ) -> Result<JsonValue, AllocatorError> {
            unreachable!("materialize tests do not acquire")
        }
        async fn release(
            &self,
            _allocator_url: &str,
            _token: &str,
            _alloc_id: &str,
        ) -> Result<(), AllocatorError> {
            unreachable!("materialize tests do not release")
        }
        async fn stage_template_with_connection(
            &self,
            _config: &JsonValue,
            _args: &StageTemplateArgs,
        ) -> Result<StageOutcome, AllocatorError> {
            unreachable!("materialize tests do not stage")
        }
        async fn materialize_image_with_connection(
            &self,
            config: &JsonValue,
            args: &MaterializeImageArgs,
        ) -> Result<MaterializeOutcome, AllocatorError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_args.lock().unwrap() = Some(args.clone());
            *self.last_config.lock().unwrap() = Some(config.clone());
            if self.fail {
                Err(AllocatorError::Status {
                    status: 500,
                    body: "pull boom".into(),
                })
            } else {
                Ok(MaterializeOutcome {
                    digest: "deadbeef".into(),
                    sif_path: "/shared/sif/deadbeef.sif".into(),
                    size_bytes: Some(123),
                })
            }
        }
    }

    fn materialize_input() -> EffectInput {
        let mut inputs = HashMap::new();
        inputs.insert(
            "request".to_string(),
            serde_json::json!({ "materialize_id": "mat-1" }),
        );
        EffectInput {
            transition_id: TransitionId::named("t_materialize"),
            inputs,
            // image_ref + creds ride effect_config (secret-resolved by firing.rs),
            // alongside the datacenter connection.
            config: Some(serde_json::json!({
                "scheduler_flavor": "slurm",
                "resource_id": "dc-abc",
                "resource_version": 2,
                "image_ref": "python:3.12-slim",
                "registry_username": "u",
                "registry_password": "p",
            })),
            read_inputs: HashMap::new(),
            process_step: None,
        }
    }

    #[tokio::test]
    async fn materialize_success_emits_ready_and_passes_args() {
        let m = MockMaterializer::ok();
        let handler = MaterializeImageHandler::new(m.clone(), "request", "materialized");

        let out = handler.execute(materialize_input()).await.unwrap();

        let tok = out.tokens.get("materialized").expect("materialized token");
        assert_eq!(tok["materialize_id"], "mat-1");
        assert_eq!(tok["status"], "ready");
        assert_eq!(tok["digest"], "deadbeef");
        assert_eq!(tok["sif_path"], "/shared/sif/deadbeef.sif");
        assert_eq!(tok["size_bytes"], 123);
        assert!(tok["error"].is_null());
        assert_eq!(&out.result, tok, "result must equal the materialized token");

        assert_eq!(m.calls.load(Ordering::SeqCst), 1);
        let args = m.last_args.lock().unwrap().clone().unwrap();
        assert_eq!(args.image_ref, "python:3.12-slim");
        assert_eq!(args.registry_username.as_deref(), Some("u"));
        assert_eq!(args.registry_password.as_deref(), Some("p"));
        let cfg = m.last_config.lock().unwrap().clone().unwrap();
        assert_eq!(cfg["scheduler_flavor"], "slurm");
    }

    #[tokio::test]
    async fn materialize_cluster_failure_is_recorded_as_data_not_neterror() {
        let m = MockMaterializer::failing();
        let handler = MaterializeImageHandler::new(m.clone(), "request", "materialized");

        let out = handler.execute(materialize_input()).await.unwrap();

        let tok = out.tokens.get("materialized").expect("materialized token");
        assert_eq!(tok["status"], "failed");
        assert_eq!(tok["materialize_id"], "mat-1");
        assert!(tok["digest"].is_null());
        assert!(tok["sif_path"].is_null());
        assert!(
            tok["error"].as_str().unwrap().contains("500"),
            "error should carry the cluster failure: {tok}"
        );
        assert_eq!(&out.result, tok);
        assert_eq!(m.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn missing_image_ref_is_fatal() {
        let m = MockMaterializer::ok();
        let handler = MaterializeImageHandler::new(m.clone(), "request", "materialized");

        // Valid token + a connection but NO image_ref in config → fatal.
        let mut input = materialize_input();
        if let Some(obj) = input.config.as_mut().and_then(|v| v.as_object_mut()) {
            obj.remove("image_ref");
        }
        let err = handler.execute(input).await.unwrap_err();
        assert!(matches!(err, EffectError::Fatal(_)), "got {err:?}");
        assert_eq!(m.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn missing_config_is_fatal() {
        let m = MockMaterializer::ok();
        let handler = MaterializeImageHandler::new(m.clone(), "request", "materialized");
        let mut input = materialize_input();
        input.config = None;
        let err = handler.execute(input).await.unwrap_err();
        assert!(matches!(err, EffectError::Fatal(_)), "got {err:?}");
        assert_eq!(m.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn replay_does_not_call_allocator() {
        let m = MockMaterializer::ok();
        let handler = MaterializeImageHandler::new(m.clone(), "request", "materialized");

        let out = handler.execute(materialize_input()).await.unwrap();
        assert_eq!(m.calls.load(Ordering::SeqCst), 1);
        let stored = out.result.clone();

        handler.replay(&materialize_input(), &stored);
        assert_eq!(
            m.calls.load(Ordering::SeqCst),
            1,
            "replay must NOT re-hit the cluster"
        );
    }
}
