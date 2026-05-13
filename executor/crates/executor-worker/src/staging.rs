use std::path::PathBuf;
use std::sync::Arc;

use aithericon_secrets::SecretStore;
use async_trait::async_trait;
use tracing::{debug, info};

use aithericon_executor_domain::{
    EventCategory, ExecutionJob, ExecutorError, RunContext, StagedEvent, StatusDetail,
};
use aithericon_executor_storage::{ArtifactStore, StoragePath};

/// A hook in the staging pipeline, called in order before backend.prepare().
#[async_trait]
pub trait StagingHook: Send + Sync + 'static {
    /// Human-readable hook name.
    fn name(&self) -> &'static str;

    /// Transform the RunContext. Called in pipeline order.
    async fn stage(&self, job: &ExecutionJob, ctx: RunContext)
        -> Result<RunContext, ExecutorError>;
}

/// Ordered pipeline of staging hooks.
pub struct StagingPipeline {
    hooks: Vec<Box<dyn StagingHook>>,
}

impl StagingPipeline {
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    /// Add a hook to the end of the pipeline.
    pub fn add_hook<H: StagingHook>(mut self, hook: H) -> Self {
        self.hooks.push(Box::new(hook));
        self
    }

    /// Run all hooks in order, then call backend.prepare().
    pub async fn prepare(
        &self,
        job: &ExecutionJob,
        mut ctx: RunContext,
        backend: &dyn aithericon_executor_backend::ExecutionBackend,
    ) -> Result<RunContext, ExecutorError> {
        for hook in &self.hooks {
            debug!(hook = hook.name(), "running staging hook");
            ctx = hook.stage(job, ctx).await?;
        }
        ctx = backend.prepare(job, ctx).await?;
        Ok(ctx)
    }
}

impl Default for StagingPipeline {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a default staging pipeline with all built-in hooks.
pub fn default_pipeline(
    base_dir: PathBuf,
    store: Option<Arc<dyn ArtifactStore>>,
    secret_store: Option<Arc<dyn SecretStore>>,
    vault_addr: Option<String>,
    nix_hook: Option<crate::nix::NixEnvironmentHook>,
) -> StagingPipeline {
    let mut pipeline = StagingPipeline::new()
        .add_hook(CreateRunDirectoryHook)
        .add_hook(InjectEnvironmentHook);

    // Secrets AFTER environment injection, BEFORE inputs staging
    if let Some(secrets) = secret_store {
        pipeline = pipeline.add_hook(InjectSecretsHook {
            store: secrets,
            vault_addr,
        });
    }

    pipeline = pipeline.add_hook(StageInputsHook {
        base_dir: base_dir.clone(),
        store,
    });

    // Nix environment resolution AFTER inputs (may inspect them), BEFORE context write
    if let Some(nix) = nix_hook {
        pipeline = pipeline.add_hook(nix);
    }

    pipeline.add_hook(WriteContextHook)
}

// ─── Built-in hooks ──────────────────────────────────────────────────────────

/// Creates the run directory tree (mkdir -p for all subdirs).
pub struct CreateRunDirectoryHook;

#[async_trait]
impl StagingHook for CreateRunDirectoryHook {
    fn name(&self) -> &'static str {
        "create_run_directory"
    }

    async fn stage(
        &self,
        _job: &ExecutionJob,
        ctx: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        for dir in ctx.run_dir.all_dirs() {
            tokio::fs::create_dir_all(dir).await.map_err(|e| {
                ExecutorError::RunDirectory(format!("mkdir {}: {e}", dir.display()))
            })?;
        }
        info!(root = %ctx.run_dir.root.display(), "run directory created");
        Ok(ctx)
    }
}

/// Injects AITHERICON_* environment variables into the RunContext.
pub struct InjectEnvironmentHook;

#[async_trait]
impl StagingHook for InjectEnvironmentHook {
    fn name(&self) -> &'static str {
        "inject_environment"
    }

    async fn stage(
        &self,
        _job: &ExecutionJob,
        mut ctx: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        ctx.env.insert(
            "AITHERICON_RUN_DIR".into(),
            ctx.run_dir.root.to_string_lossy().into_owned(),
        );
        ctx.env.insert(
            "AITHERICON_IPC_SOCKET".into(),
            ctx.run_dir.ipc_socket.to_string_lossy().into_owned(),
        );
        ctx.env.insert(
            "AITHERICON_INPUTS_DIR".into(),
            ctx.run_dir.inputs_dir.to_string_lossy().into_owned(),
        );
        ctx.env.insert(
            "AITHERICON_OUTPUTS_DIR".into(),
            ctx.run_dir.outputs_dir.to_string_lossy().into_owned(),
        );
        ctx.env.insert(
            "AITHERICON_ARTIFACTS_DIR".into(),
            ctx.run_dir.artifacts_dir.to_string_lossy().into_owned(),
        );
        ctx.env
            .insert("AITHERICON_EXECUTION_ID".into(), ctx.execution_id.clone());

        debug!("injected AITHERICON_* env vars");
        Ok(ctx)
    }
}

/// Stages input files from storage into the inputs/ directory.
pub struct StageInputsHook {
    pub base_dir: PathBuf,
    pub store: Option<Arc<dyn ArtifactStore>>,
}

/// If `name` has no file extension but `remote_path` does, append the
/// extension from `remote_path`. Otherwise return `name` unchanged.
fn name_with_extension(name: &str, remote_path: &str) -> String {
    use std::path::Path;
    if Path::new(name).extension().is_none() {
        if let Some(ext) = Path::new(remote_path).extension().and_then(|e| e.to_str()) {
            return format!("{name}.{ext}");
        }
    }
    name.to_string()
}

#[async_trait]
impl StagingHook for StageInputsHook {
    fn name(&self) -> &'static str {
        "stage_inputs"
    }

    async fn stage(
        &self,
        _job: &ExecutionJob,
        mut ctx: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        // Extract input declarations from spec
        let inputs = &ctx.spec.inputs;

        for input in inputs {
            match &input.source {
                aithericon_executor_domain::InputSource::Inline { value } => {
                    let dest = ctx.run_dir.inputs_dir.join(&input.name);
                    let data = serde_json::to_vec_pretty(value).map_err(|e| {
                        ExecutorError::StagingFailed(format!(
                            "failed to serialize inline input '{}': {e}",
                            input.name
                        ))
                    })?;
                    tokio::fs::write(&dest, data).await.map_err(|e| {
                        ExecutorError::StagingFailed(format!(
                            "failed to write inline input '{}': {e}",
                            input.name
                        ))
                    })?;
                    ctx.staged_inputs.insert(input.name.clone(), dest);
                }
                aithericon_executor_domain::InputSource::Raw { content } => {
                    let dest = ctx.run_dir.inputs_dir.join(&input.name);
                    tokio::fs::write(&dest, content.as_bytes()).await.map_err(|e| {
                        ExecutorError::StagingFailed(format!(
                            "failed to write raw input '{}': {e}",
                            input.name
                        ))
                    })?;
                    ctx.staged_inputs.insert(input.name.clone(), dest);
                }
                aithericon_executor_domain::InputSource::StoragePath { path, storage } => {
                    match storage {
                        #[cfg(feature = "opendal")]
                        Some(config) => {
                            let final_name = name_with_extension(&input.name, path);
                            let dest = ctx.run_dir.inputs_dir.join(&final_name);
                            let (operator, prefix) =
                                aithericon_executor_storage::build_operator_with_prefix(config)
                                    .map_err(|e| {
                                        ExecutorError::StagingFailed(format!(
                                            "storage operator for input '{}': {e}",
                                            input.name
                                        ))
                                    })?;
                            let remote_path = format!("{}{}", prefix, path);
                            let data = operator.read(&remote_path).await.map_err(|e| {
                                ExecutorError::StagingFailed(format!(
                                    "download input '{}' from '{}': {e}",
                                    input.name, path
                                ))
                            })?;
                            tokio::fs::write(&dest, data.to_vec()).await.map_err(
                                |e| {
                                    ExecutorError::StagingFailed(format!(
                                        "write input '{}': {e}",
                                        input.name
                                    ))
                                },
                            )?;
                            ctx.staged_events.push(StagedEvent {
                                category: EventCategory::Artifact,
                                detail: StatusDetail::ArtifactConsumed {
                                    input_name: input.name.clone(),
                                    storage_path: path.clone(),
                                    size_bytes: tokio::fs::metadata(&dest)
                                        .await
                                        .ok()
                                        .map(|m| m.len()),
                                },
                            });
                            ctx.staged_inputs.insert(input.name.clone(), dest);
                        }
                        #[cfg(not(feature = "opendal"))]
                        Some(_) => {
                            return Err(ExecutorError::StagingFailed(format!(
                                "per-input storage config on '{}' requires the 'opendal' feature",
                                input.name
                            )));
                        }
                        None => {
                            // Fallback: global ArtifactStore (existing behavior)
                            let final_name = name_with_extension(&input.name, path);
                            let dest = ctx.run_dir.inputs_dir.join(&final_name);
                            if let Some(store) = &self.store {
                                let storage_path = StoragePath(path.clone());
                                store.download(&storage_path, &dest).await.map_err(
                                    |e| {
                                        ExecutorError::StagingFailed(format!(
                                            "download input '{}' from '{}': {e}",
                                            input.name, path
                                        ))
                                    },
                                )?;
                                ctx.staged_events.push(StagedEvent {
                                    category: EventCategory::Artifact,
                                    detail: StatusDetail::ArtifactConsumed {
                                        input_name: input.name.clone(),
                                        storage_path: path.clone(),
                                        size_bytes: tokio::fs::metadata(&dest)
                                            .await
                                            .ok()
                                            .map(|m| m.len()),
                                    },
                                });
                                ctx.staged_inputs.insert(input.name.clone(), dest);
                            } else if input.required {
                                return Err(ExecutorError::InputNotFound(format!(
                                    "storage input '{}' requires ArtifactStore but none is configured",
                                    input.name
                                )));
                            } else {
                                debug!(
                                    name = %input.name,
                                    path,
                                    "skipping optional storage input (no store configured)"
                                );
                            }
                        }
                    }
                }
                #[cfg(feature = "url-inputs")]
                aithericon_executor_domain::InputSource::Url { url } => {
                    let dest = ctx.run_dir.inputs_dir.join(&input.name);
                    let response = reqwest::get(url.as_str()).await.map_err(|e| {
                        ExecutorError::StagingFailed(format!(
                            "download URL input '{}' ({}): {e}",
                            input.name, url
                        ))
                    })?;
                    if !response.status().is_success() {
                        if input.required {
                            return Err(ExecutorError::StagingFailed(format!(
                                "URL input '{}' returned HTTP {}: {}",
                                input.name,
                                response.status(),
                                url
                            )));
                        }
                        debug!(
                            name = %input.name,
                            %url,
                            status = %response.status(),
                            "skipping optional URL input (HTTP error)"
                        );
                        continue;
                    }
                    let bytes = response.bytes().await.map_err(|e| {
                        ExecutorError::StagingFailed(format!(
                            "read URL input '{}': {e}",
                            input.name
                        ))
                    })?;
                    tokio::fs::write(&dest, &bytes).await.map_err(|e| {
                        ExecutorError::StagingFailed(format!(
                            "write URL input '{}': {e}",
                            input.name
                        ))
                    })?;
                    ctx.staged_inputs.insert(input.name.clone(), dest);
                    debug!(
                        name = %input.name,
                        %url,
                        bytes = bytes.len(),
                        "URL input staged"
                    );
                }
                #[cfg(not(feature = "url-inputs"))]
                aithericon_executor_domain::InputSource::Url { url } => {
                    if input.required {
                        return Err(ExecutorError::StagingFailed(format!(
                            "URL input '{}' (url: {}) requires the 'url-inputs' feature",
                            input.name, url
                        )));
                    }
                    debug!(
                        name = %input.name,
                        url,
                        "skipping URL input (url-inputs feature not enabled)"
                    );
                }
            }
        }

        // Record expected outputs
        let outputs = &ctx.spec.outputs;

        for output in outputs {
            if let Some(path) = &output.path {
                ctx.expected_outputs
                    .insert(output.name.clone(), ctx.run_dir.outputs_dir.join(path));
            }
        }

        if !inputs.is_empty() || !outputs.is_empty() {
            debug!(
                inputs = inputs.len(),
                outputs = outputs.len(),
                "staged inputs and recorded expected outputs"
            );
        }

        Ok(ctx)
    }
}

/// Serializes the RunContext to context.json in the run directory.
pub struct WriteContextHook;

#[async_trait]
impl StagingHook for WriteContextHook {
    fn name(&self) -> &'static str {
        "write_context"
    }

    async fn stage(
        &self,
        _job: &ExecutionJob,
        ctx: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        let data = serde_json::to_vec_pretty(&ctx).map_err(|e| {
            ExecutorError::StagingFailed(format!("failed to serialize RunContext: {e}"))
        })?;
        tokio::fs::write(&ctx.run_dir.context_file, data)
            .await
            .map_err(|e| {
                ExecutorError::StagingFailed(format!("failed to write context.json: {e}"))
            })?;
        debug!(path = %ctx.run_dir.context_file.display(), "context.json written");
        Ok(ctx)
    }
}

/// Resolves `{{secret:KEY}}` patterns in `RunContext.env` values and `spec.config`.
///
/// When the job carries a `wrapped_secrets` token and `vault_addr` is configured,
/// the hook unwraps the Vault wrapping token to obtain resolved secrets, then
/// uses them to resolve `{{secret:KEY}}` refs. This means the executor does NOT
/// need broad secret store access — it only unwraps what was explicitly wrapped.
///
/// Falls back to `self.store` for direct resolution when no wrapping token is present.
pub struct InjectSecretsHook {
    pub store: Arc<dyn SecretStore>,
    /// Vault address for unwrapping wrapped secrets. Only `VAULT_ADDR` is needed,
    /// not `VAULT_TOKEN` — the wrapping token itself is used as auth.
    pub vault_addr: Option<String>,
}

#[async_trait]
impl StagingHook for InjectSecretsHook {
    fn name(&self) -> &'static str {
        "inject_secrets"
    }

    async fn stage(
        &self,
        job: &ExecutionJob,
        mut ctx: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        // Determine which store to use for resolving {{secret:KEY}} patterns.
        // If the job carries a Vault wrapping token, unwrap it to get an in-memory
        // store. Otherwise, fall back to the configured store (env, vault, etc.).
        let effective_store: Arc<dyn SecretStore> =
            match (&job.wrapped_secrets, &self.vault_addr) {
                #[cfg(feature = "vault")]
                (Some(wrapping_token), Some(vault_addr)) => {
                    debug!("unwrapping Vault wrapping token for secrets");
                    let unwrapped =
                        aithericon_secrets::vault_unwrap_secrets(vault_addr, wrapping_token)
                            .await
                            .map_err(|e| {
                                ExecutorError::SecretResolutionFailed(format!(
                                    "failed to unwrap secrets: {e}"
                                ))
                            })?;
                    Arc::new(aithericon_secrets::InMemorySecretStore::new(unwrapped))
                }
                _ => self.store.clone(),
            };

        // 1. Resolve {{secret:KEY}} patterns in env values
        for value in ctx.env.values_mut() {
            if value.contains("{{secret:") {
                let json_val = serde_json::Value::String(value.clone());
                let resolved =
                    aithericon_secrets::resolve_secrets(&json_val, effective_store.as_ref())
                        .await
                        .map_err(|e| ExecutorError::SecretResolutionFailed(e.to_string()))?;
                if let serde_json::Value::String(s) = resolved {
                    *value = s;
                }
            }
        }

        // 2. Resolve {{secret:KEY}} patterns in spec.config
        ctx.spec.config =
            aithericon_secrets::resolve_secrets(&ctx.spec.config, effective_store.as_ref())
                .await
                .map_err(|e| ExecutorError::SecretResolutionFailed(e.to_string()))?;

        // 3. Resolve {{secret:KEY}} in per-input storage configs
        for input in ctx.spec.inputs.iter_mut() {
            if let aithericon_executor_domain::InputSource::StoragePath {
                storage: Some(ref mut config),
                ..
            } = &mut input.source
            {
                let mut val = serde_json::to_value(&*config).map_err(|e| {
                    ExecutorError::SecretResolutionFailed(format!(
                        "failed to serialize storage config for input '{}': {e}",
                        input.name
                    ))
                })?;
                val = aithericon_secrets::resolve_secrets(&val, effective_store.as_ref())
                    .await
                    .map_err(|e| ExecutorError::SecretResolutionFailed(e.to_string()))?;
                *config = serde_json::from_value(val).map_err(|e| {
                    ExecutorError::SecretResolutionFailed(format!(
                        "failed to deserialize resolved storage config for input '{}': {e}",
                        input.name
                    ))
                })?;
            }
        }

        // 4. Resolve {{secret:KEY}} in per-output upload configs
        for output in ctx.spec.outputs.iter_mut() {
            if let Some(ref mut upload) = output.upload_to {
                let mut val = serde_json::to_value(&upload.storage).map_err(|e| {
                    ExecutorError::SecretResolutionFailed(format!(
                        "failed to serialize storage config for output '{}': {e}",
                        output.name
                    ))
                })?;
                val = aithericon_secrets::resolve_secrets(&val, effective_store.as_ref())
                    .await
                    .map_err(|e| ExecutorError::SecretResolutionFailed(e.to_string()))?;
                upload.storage = serde_json::from_value(val).map_err(|e| {
                    ExecutorError::SecretResolutionFailed(format!(
                        "failed to deserialize resolved storage config for output '{}': {e}",
                        output.name
                    ))
                })?;
            }
        }

        debug!("resolved secrets in RunContext");
        Ok(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aithericon_executor_backend::ProcessConfig;
    use aithericon_executor_domain::RunDirectory;
    use std::collections::HashMap;
    use std::time::Duration;

    fn test_job() -> ExecutionJob {
        ExecutionJob {
            execution_id: "test-staging".into(),
            spec: ProcessConfig {
                command: "echo".into(),
                args: vec!["hello".into()],
                env: Default::default(),
                working_dir: None,
                inherit_env: true,
            }
            .into_spec(),
            metadata: HashMap::new(),
            timeout: None,
            priority: aithericon_executor_domain::JobPriority::Medium,
            stream_events: None,
            wrapped_secrets: None,
        }
    }

    fn test_context(base_dir: &PathBuf) -> RunContext {
        RunContext {
            execution_id: "test-staging".into(),
            spec: ProcessConfig {
                command: "echo".into(),
                args: vec!["hello".into()],
                env: Default::default(),
                working_dir: None,
                inherit_env: true,
            }
            .into_spec(),
            run_dir: RunDirectory::new(base_dir, "test-staging"),
            timeout: Duration::from_secs(60),
            env: HashMap::new(),
            metadata: HashMap::new(),
            staged_inputs: HashMap::new(),
            expected_outputs: HashMap::new(),
            staged_events: vec![],
            backend_state: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn create_run_directory_hook() {
        let tmp = std::env::temp_dir().join(format!("staging-test-{}", std::process::id()));
        let ctx = test_context(&tmp);

        let hook = CreateRunDirectoryHook;
        let result = hook.stage(&test_job(), ctx).await.unwrap();

        assert!(result.run_dir.root.exists());
        assert!(result.run_dir.inputs_dir.exists());
        assert!(result.run_dir.outputs_dir.exists());
        assert!(result.run_dir.artifacts_dir.exists());
        assert!(result.run_dir.logs_dir.exists());

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn inject_environment_hook() {
        let tmp = PathBuf::from("/tmp/staging-env-test");
        let ctx = test_context(&tmp);

        let hook = InjectEnvironmentHook;
        let result = hook.stage(&test_job(), ctx).await.unwrap();

        assert!(result.env.contains_key("AITHERICON_RUN_DIR"));
        assert!(result.env.contains_key("AITHERICON_IPC_SOCKET"));
        assert!(result.env.contains_key("AITHERICON_INPUTS_DIR"));
        assert!(result.env.contains_key("AITHERICON_OUTPUTS_DIR"));
        assert!(result.env.contains_key("AITHERICON_ARTIFACTS_DIR"));
        assert_eq!(
            result.env.get("AITHERICON_EXECUTION_ID").unwrap(),
            "test-staging"
        );
    }

    #[tokio::test]
    async fn write_context_hook() {
        let tmp = std::env::temp_dir().join(format!("staging-ctx-{}", std::process::id()));
        let ctx = test_context(&tmp);

        // Create the directory first
        let hook_create = CreateRunDirectoryHook;
        let ctx = hook_create.stage(&test_job(), ctx).await.unwrap();

        let hook = WriteContextHook;
        let result = hook.stage(&test_job(), ctx).await.unwrap();

        assert!(result.run_dir.context_file.exists());

        // Verify it's valid JSON
        let data = std::fs::read_to_string(&result.run_dir.context_file).unwrap();
        let _: RunContext = serde_json::from_str(&data).unwrap();

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ========================================================================
    // InjectSecretsHook tests
    // ========================================================================

    struct MockSecretStore(HashMap<String, String>);

    #[async_trait]
    impl aithericon_secrets::SecretStore for MockSecretStore {
        async fn get(&self, key: &str) -> Result<String, aithericon_secrets::SecretError> {
            self.0
                .get(key)
                .cloned()
                .ok_or_else(|| aithericon_secrets::SecretError::NotFound(key.to_string()))
        }
        fn name(&self) -> &str {
            "mock"
        }
    }

    #[tokio::test]
    async fn test_inject_secrets_resolves_env_values() {
        let store = Arc::new(MockSecretStore(HashMap::from([(
            "MY_SECRET".into(),
            "resolved_value".into(),
        )])));
        let hook = InjectSecretsHook { store, vault_addr: None };

        let tmp = PathBuf::from("/tmp/staging-secrets-env-test");
        let mut ctx = test_context(&tmp);
        ctx.env
            .insert("API_KEY".into(), "{{secret:MY_SECRET}}".into());
        ctx.env
            .insert("PLAIN".into(), "no_secrets_here".into());

        let result = hook.stage(&test_job(), ctx).await.unwrap();

        assert_eq!(result.env["API_KEY"], "resolved_value");
        assert_eq!(result.env["PLAIN"], "no_secrets_here");
    }

    #[tokio::test]
    async fn test_inject_secrets_resolves_spec_config() {
        let store = Arc::new(MockSecretStore(HashMap::from([(
            "API_TOKEN".into(),
            "sk-abc123".into(),
        )])));
        let hook = InjectSecretsHook { store, vault_addr: None };

        let tmp = PathBuf::from("/tmp/staging-secrets-config-test");
        let mut ctx = test_context(&tmp);
        ctx.spec.config = serde_json::json!({"token": "{{secret:API_TOKEN}}", "url": "https://api.example.com"});

        let result = hook.stage(&test_job(), ctx).await.unwrap();

        assert_eq!(
            result.spec.config,
            serde_json::json!({"token": "sk-abc123", "url": "https://api.example.com"})
        );
    }

    #[tokio::test]
    async fn test_inject_secrets_missing_key_returns_error() {
        let store = Arc::new(MockSecretStore(HashMap::new())); // empty — all lookups fail
        let hook = InjectSecretsHook { store, vault_addr: None };

        let tmp = PathBuf::from("/tmp/staging-secrets-fail-test");
        let mut ctx = test_context(&tmp);
        ctx.env
            .insert("TOKEN".into(), "{{secret:MISSING}}".into());

        let result = hook.stage(&test_job(), ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ExecutorError::SecretResolutionFailed(msg) => {
                assert!(msg.contains("MISSING"), "Error should mention the key: {msg}");
            }
            other => panic!("Expected SecretResolutionFailed, got {:?}", other),
        }
    }

    // ========================================================================
    // name_with_extension tests
    // ========================================================================

    #[test]
    fn test_name_with_extension_appends_when_missing() {
        assert_eq!(
            name_with_extension("obs_0", "artifacts/model.json"),
            "obs_0.json"
        );
    }

    #[test]
    fn test_name_with_extension_preserves_existing() {
        assert_eq!(
            name_with_extension("data.json", "artifacts/model.json"),
            "data.json"
        );
    }

    #[test]
    fn test_name_with_extension_no_remote_ext() {
        assert_eq!(name_with_extension("obs_0", "noextension"), "obs_0");
    }

    #[test]
    fn test_name_with_extension_compound_ext() {
        assert_eq!(name_with_extension("obs_0", "file.tar.gz"), "obs_0.gz");
    }

    #[test]
    fn test_name_with_extension_empty_name() {
        assert_eq!(name_with_extension("", "model.json"), ".json");
    }
}
