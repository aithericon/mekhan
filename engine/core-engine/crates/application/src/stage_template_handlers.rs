//! Stage-template effect handler (Phase 4 — the control plane's `stage_template`
//! INLINE engine effect).
//!
//! Symmetric in structure with [`crate::resource_lease_handlers`]: a stateless
//! handler holding an `Arc<dyn AllocatorClient>` that routes the per-fire
//! `effect_config` (mekhan baked the same `DatacenterConnection.effect_config()`
//! JSON the lease adapter uses; `firing.rs` resolved `{{secret:…}}` into plaintext
//! BEFORE `execute()` runs) to the right flavor allocator and REGISTERS a job
//! template onto that cluster.
//!
//!   - **Nomad:** render the typed spec → a Nomad PARAMETERIZED job JSON and
//!     `PUT /v1/job/{slug}` (the REGISTER endpoint, NOT dispatch). `remote_ref`
//!     = the slug.
//!   - **Slurm:** render → an sbatch script, written + rsync'd over SSH to
//!     `{template_dir}/{slug}.sh`. `remote_ref` = the remote path.
//!
//! ## Failure model (the load-bearing contract)
//!
//! A staging *cluster* failure (register PUT 4xx/5xx, SSH error, …) is recorded
//! as `status:"failed"` DATA on the `staged` port AND in `effect_result` — the
//! staging net completes CLEANLY so mekhan's projection records the failure. It
//! is NOT a `NetFailed`. Only truly-fatal config/parse errors (missing request
//! fields, unparseable config) return `Err(EffectError::Fatal(…))`.
//!
//! ## Replay
//!
//! [`replay`](EffectHandler::replay) is a no-op (stateless — the staged result
//! lives entirely in the journaled produced token, re-emitted by the engine on
//! replay; the cluster is NOT re-hit). Registration is idempotent at the
//! allocator leg (Nomad tolerates a re-register; Slurm overwrites the script).

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value as JsonValue;

use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};
use crate::resource_lease_handlers::{AllocatorClient, StageRequestToken, StageTemplateArgs};

/// Registers a job template onto an external cluster and emits the typed
/// staging-result token.
///
/// Input port (`request`): [`StageRequestToken`] —
/// `{ staging_id, slug, spec, escape_hatch, package_ref }`.
/// Output port (`staged`) AND `effect_result` (IDENTICAL JSON):
/// `{ staging_id, status, remote_ref, slug, error }`.
pub struct StageTemplateHandler {
    client: Arc<dyn AllocatorClient>,
    input_port: String,
    output_port: String,
}

impl StageTemplateHandler {
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

    /// Build the staging-result JSON. Used for BOTH success and failure so the
    /// output token and `effect_result` are byte-identical and the shape is the
    /// single source of truth.
    fn result_token(
        staging_id: &str,
        slug: &str,
        status: &str,
        remote_ref: Option<&str>,
        error: Option<&str>,
    ) -> JsonValue {
        serde_json::json!({
            "staging_id": staging_id,
            "status": status,
            "remote_ref": remote_ref,
            "slug": slug,
            "error": error,
        })
    }
}

#[async_trait::async_trait]
impl EffectHandler for StageTemplateHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let token = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in stage_template handler",
                self.input_port
            ))
        })?;

        // Parse the typed request token. Missing required fields (staging_id,
        // slug) are a FATAL author/compiler error — there is nothing to record.
        let req: StageRequestToken = serde_json::from_value(token.clone())
            .map_err(|e| EffectError::Fatal(format!("stage_template request is not valid: {e}")))?;

        if req.staging_id.is_empty() {
            return Err(EffectError::Fatal(
                "stage_template request missing staging_id".into(),
            ));
        }
        if req.slug.is_empty() {
            return Err(EffectError::Fatal(
                "stage_template request missing slug".into(),
            ));
        }

        // The full resolved effect_config (same `DatacenterConnection.effect_config()`
        // JSON the lease path consumes). Absent → FATAL: there is no cluster to
        // stage onto.
        let config = input.config.clone().ok_or_else(|| {
            EffectError::Fatal(
                "stage_template handler requires a datacenter connection in effect_config".into(),
            )
        })?;

        // v1 package_ref: thread it through but treat it as a best-effort no-op.
        // Building a container/cache subsystem is out of scope for Phase 4.
        if let Some(ref pkg) = req.package_ref {
            tracing::info!(
                staging_id = %req.staging_id,
                slug = %req.slug,
                catalogue_entry_id = %pkg.catalogue_entry_id,
                "stage_template: package_ref present — v1 best-effort no-op (basic delivery only) \
                 TODO: resolve + deliver the catalogue package before registration",
            );
        }

        let args = StageTemplateArgs {
            slug: req.slug.clone(),
            spec: req.spec,
            escape_hatch: req.escape_hatch,
            package_ref: req.package_ref,
        };

        // Route to the resolved cluster's allocator. A cluster failure is DATA,
        // not a NetFailed: record `status:"failed"` and complete cleanly.
        let result_token = match self
            .client
            .stage_template_with_connection(&config, &args)
            .await
        {
            Ok(outcome) => {
                tracing::info!(
                    staging_id = %req.staging_id,
                    slug = %req.slug,
                    remote_ref = %outcome.remote_ref,
                    "stage_template: template staged",
                );
                Self::result_token(
                    &req.staging_id,
                    &req.slug,
                    "staged",
                    Some(&outcome.remote_ref),
                    None,
                )
            }
            Err(e) => {
                let msg = e.to_string();
                tracing::warn!(
                    staging_id = %req.staging_id,
                    slug = %req.slug,
                    error = %msg,
                    "stage_template: staging failed (recorded as data, not NetFailed)",
                );
                Self::result_token(&req.staging_id, &req.slug, "failed", None, Some(&msg))
            }
        };

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), result_token.clone());

        Ok(EffectOutput {
            tokens,
            // IDENTICAL to the output token — journaled so replay re-emits it
            // without re-hitting the cluster.
            result: result_token,
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless: the staging result lives entirely in the journaled produced
        // token, which the engine re-emits on replay. The cluster is NOT re-hit.
    }

    fn name(&self) -> &str {
        "stage_template"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::ExecutionMode;
    use crate::resource_lease_handlers::{AllocatorError, StageOutcome};
    use petri_domain::TransitionId;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock allocator that records the staging args and either returns a canned
    /// `remote_ref` or errors — so the handler's success-vs-failure-as-data and
    /// replay contracts can be asserted with no network.
    struct MockStager {
        stage_calls: AtomicUsize,
        last_args: std::sync::Mutex<Option<StageTemplateArgs>>,
        last_config: std::sync::Mutex<Option<JsonValue>>,
        fail: bool,
    }

    impl MockStager {
        fn ok() -> Arc<Self> {
            Arc::new(Self {
                stage_calls: AtomicUsize::new(0),
                last_args: std::sync::Mutex::new(None),
                last_config: std::sync::Mutex::new(None),
                fail: false,
            })
        }
        fn failing() -> Arc<Self> {
            Arc::new(Self {
                stage_calls: AtomicUsize::new(0),
                last_args: std::sync::Mutex::new(None),
                last_config: std::sync::Mutex::new(None),
                fail: true,
            })
        }
    }

    #[async_trait::async_trait]
    impl AllocatorClient for MockStager {
        async fn acquire(
            &self,
            _allocator_url: &str,
            _token: &str,
            _grant_id: &str,
            _request: &JsonValue,
        ) -> Result<JsonValue, AllocatorError> {
            unreachable!("stage tests do not acquire")
        }
        async fn release(
            &self,
            _allocator_url: &str,
            _token: &str,
            _alloc_id: &str,
        ) -> Result<(), AllocatorError> {
            unreachable!("stage tests do not release")
        }
        async fn stage_template_with_connection(
            &self,
            config: &JsonValue,
            args: &StageTemplateArgs,
        ) -> Result<StageOutcome, AllocatorError> {
            self.stage_calls.fetch_add(1, Ordering::SeqCst);
            *self.last_args.lock().unwrap() = Some(args.clone());
            *self.last_config.lock().unwrap() = Some(config.clone());
            if self.fail {
                Err(AllocatorError::Status {
                    status: 500,
                    body: "boom".into(),
                })
            } else {
                Ok(StageOutcome {
                    remote_ref: format!("ref-for-{}", args.slug),
                })
            }
        }
    }

    fn stage_input() -> EffectInput {
        let mut inputs = HashMap::new();
        inputs.insert(
            "request".to_string(),
            serde_json::json!({
                "staging_id": "stg-1",
                "slug": "train-job",
                "spec": {
                    "cpus": 4,
                    "gpus": 1,
                    "gpu_type": "a100",
                    "mem_mb": 8192,
                    "time_limit": "01:00:00",
                    "partition": "gpu",
                    "image": "py:3.12",
                    "entrypoint": "python run.py",
                    "env": { "FOO": "bar" }
                },
                "escape_hatch": { "sbatch_directives": [], "hcl_stanza": null },
                "package_ref": null
            }),
        );
        EffectInput {
            transition_id: TransitionId::named("t_stage"),
            inputs,
            config: Some(serde_json::json!({
                "scheduler_flavor": "nomad",
                "resource_id": "dc-abc",
                "resource_version": 2,
                "nomad_addr": "http://nomad.test:4646",
            })),
            read_inputs: HashMap::new(),
            process_step: None,
        }
    }

    #[tokio::test]
    async fn stage_success_emits_staged_status_and_passes_args() {
        let stager = MockStager::ok();
        let handler = StageTemplateHandler::new(stager.clone(), "request", "staged");

        let out = handler.execute(stage_input()).await.unwrap();

        // Output token == effect_result, status "staged", remote_ref from cluster.
        let staged = out.tokens.get("staged").expect("staged token");
        assert_eq!(staged["staging_id"], "stg-1");
        assert_eq!(staged["status"], "staged");
        assert_eq!(staged["slug"], "train-job");
        assert_eq!(staged["remote_ref"], "ref-for-train-job");
        assert!(staged["error"].is_null());
        assert_eq!(&out.result, staged, "result must equal the staged token");

        // The allocator saw the parsed args (typed spec threaded through).
        assert_eq!(stager.stage_calls.load(Ordering::SeqCst), 1);
        let args = stager.last_args.lock().unwrap().clone().unwrap();
        assert_eq!(args.slug, "train-job");
        assert_eq!(args.spec.cpus, Some(4));
        assert_eq!(args.spec.gpus, Some(1));
        assert_eq!(args.spec.gpu_type.as_deref(), Some("a100"));
        assert_eq!(args.spec.env.get("FOO").map(String::as_str), Some("bar"));
        // The full effect_config (datacenter connection) reached the allocator.
        let cfg = stager.last_config.lock().unwrap().clone().unwrap();
        assert_eq!(cfg["scheduler_flavor"], "nomad");
        assert_eq!(cfg["resource_id"], "dc-abc");
    }

    #[tokio::test]
    async fn stage_cluster_failure_is_recorded_as_data_not_neterror() {
        let stager = MockStager::failing();
        let handler = StageTemplateHandler::new(stager.clone(), "request", "staged");

        // A cluster failure returns Ok(...) with status:"failed" — NOT Err.
        let out = handler.execute(stage_input()).await.unwrap();

        let staged = out.tokens.get("staged").expect("staged token");
        assert_eq!(staged["status"], "failed");
        assert_eq!(staged["staging_id"], "stg-1");
        assert_eq!(staged["slug"], "train-job");
        assert!(staged["remote_ref"].is_null());
        assert!(
            staged["error"].as_str().unwrap().contains("500"),
            "error should carry the cluster failure: {staged}"
        );
        assert_eq!(&out.result, staged);
        assert_eq!(stager.stage_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn missing_request_fields_are_fatal() {
        let stager = MockStager::ok();
        let handler = StageTemplateHandler::new(stager.clone(), "request", "staged");

        // Missing slug → Fatal (compiler/author bug, not recordable data).
        let mut inputs = HashMap::new();
        inputs.insert(
            "request".to_string(),
            serde_json::json!({ "staging_id": "stg-1" }),
        );
        let input = EffectInput {
            transition_id: TransitionId::named("t_stage"),
            inputs,
            config: Some(serde_json::json!({ "scheduler_flavor": "nomad" })),
            read_inputs: HashMap::new(),
            process_step: None,
        };
        let err = handler.execute(input).await.unwrap_err();
        assert!(matches!(err, EffectError::Fatal(_)), "got {err:?}");
        assert_eq!(stager.stage_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn missing_config_is_fatal() {
        let stager = MockStager::ok();
        let handler = StageTemplateHandler::new(stager.clone(), "request", "staged");
        let mut input = stage_input();
        input.config = None;
        let err = handler.execute(input).await.unwrap_err();
        assert!(matches!(err, EffectError::Fatal(_)), "got {err:?}");
        assert_eq!(stager.stage_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn replay_does_not_call_allocator() {
        let stager = MockStager::ok();
        let handler = StageTemplateHandler::new(stager.clone(), "request", "staged");

        let out = handler.execute(stage_input()).await.unwrap();
        assert_eq!(stager.stage_calls.load(Ordering::SeqCst), 1);
        let stored = out.result.clone();

        // Engine replay path calls replay(), never execute().
        let _ = ExecutionMode::Replay;
        handler.replay(&stage_input(), &stored);

        assert_eq!(
            stager.stage_calls.load(Ordering::SeqCst),
            1,
            "replay must NOT re-hit the cluster"
        );
    }

    #[tokio::test]
    async fn package_ref_is_threaded_but_noop() {
        let stager = MockStager::ok();
        let handler = StageTemplateHandler::new(stager.clone(), "request", "staged");

        let mut input = stage_input();
        if let Some(obj) = input
            .inputs
            .get_mut("request")
            .and_then(|v| v.as_object_mut())
        {
            obj.insert(
                "package_ref".to_string(),
                serde_json::json!({ "catalogue_entry_id": "cat-7" }),
            );
        }
        let out = handler.execute(input).await.unwrap();
        // Still staged (no-op delivery), and the args carried the package_ref.
        assert_eq!(out.tokens.get("staged").unwrap()["status"], "staged");
        let args = stager.last_args.lock().unwrap().clone().unwrap();
        assert_eq!(
            args.package_ref.unwrap().catalogue_entry_id,
            "cat-7".to_string()
        );
    }
}
