use std::path::PathBuf;
use std::sync::Arc;

use aithericon_secrets::SecretStore;
use async_trait::async_trait;
use tracing::{debug, info};

use aithericon_executor_domain::{
    EventCategory, ExecutionJob, ExecutorError, RunContext, StagedEvent, StatusDetail,
};
use aithericon_executor_storage::{ArtifactStore, StoragePath};

/// Deserialize a resolved storage config from the `PlanSecretsHook`
/// side-channel, if present.
///
/// `resolved` is the per-input/output JSON from `ctx.resolved_*_storage`;
/// `None` means no secrets needed resolving and the caller should fall back to
/// the raw `spec` view. `kind`/`name` only shape the error message (e.g.
/// `("input", "data")`). Shared by `StageInputsHook` and the output-upload
/// sweep so the deser + error wording stays in one place.
#[cfg(feature = "opendal")]
pub(crate) fn deserialize_resolved_storage(
    resolved: Option<&serde_json::Value>,
    kind: &str,
    name: &str,
) -> Result<Option<aithericon_executor_storage::StorageConfig>, ExecutorError> {
    match resolved {
        Some(json) => Some(serde_json::from_value(json.clone()).map_err(|e| {
            ExecutorError::StagingFailed(format!(
                "deserialize resolved storage config for {kind} '{name}': {e}"
            ))
        }))
        .transpose(),
        None => Ok(None),
    }
}

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
///
/// Pipeline order (Gap #1 fix):
/// 1. `CreateRunDirectoryHook`   — mkdir run dir tree, `chmod 0700` on root
/// 2. `InjectEnvironmentHook`    — `AITHERICON_*` env (non-secret)
/// 3. `PlanSecretsHook`          — populate `resolved_*` side-channel only
/// 4. `StageInputsHook`          — uses `resolved_input_storage` when present
/// 5. `NixEnvironmentHook`       — optional
/// 6. `WriteContextHook`         — serialize context.json (`#[serde(skip)]`
///    drops the resolved fields), `chmod 0600`
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

    // FetchConfigHook MUST run before PlanSecretsHook: secret resolution
    // scans `spec.config` for `{{secret:KEY}}` refs, and the config isn't
    // present yet on `config_ref`-shaped jobs (compiler offloads large
    // configs to S3 and ships a pointer). Inline-spec jobs pass through.
    if let Some(s) = store.clone() {
        pipeline = pipeline.add_hook(FetchConfigHook { store: s });
    }

    // Plan secrets AFTER environment injection + config fetch, BEFORE
    // inputs staging. PlanSecretsHook writes ONLY to `resolved_*`
    // side-channel fields. Plaintext never lands in `env`/`spec.config`/
    // `spec.inputs[*].source.storage`, so the subsequent WriteContextHook
    // serializes the unresolved templates only (Gap #1 fix).
    if let Some(secrets) = secret_store {
        pipeline = pipeline.add_hook(PlanSecretsHook {
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
///
/// On Unix the root directory is chmod'd to `0700` so that the future
/// `context.json` (containing unresolved `{{secret:KEY}}` patterns but
/// potentially nsjail-mounted alongside resolved values) is not world-readable
/// even before `WriteContextHook` tightens `context.json` itself.
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

        // Tighten the run-dir root to owner-only. Sub-directories inherit
        // O_NONE traversal protection — children write only-readable files
        // anyway, but the umask on a shared host may be permissive.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = tokio::fs::metadata(&ctx.run_dir.root)
                .await
                .map_err(|e| {
                    ExecutorError::RunDirectory(format!("stat {}: {e}", ctx.run_dir.root.display()))
                })?
                .permissions();
            perms.set_mode(0o700);
            tokio::fs::set_permissions(&ctx.run_dir.root, perms)
                .await
                .map_err(|e| {
                    ExecutorError::RunDirectory(format!(
                        "chmod 0700 {}: {e}",
                        ctx.run_dir.root.display()
                    ))
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
        job: &ExecutionJob,
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

        // The SDK's `_load_manifest()` reads AITHERICON_CHANNELS to validate
        // local `emit()`/`scatter()` calls against the compiler-declared channel
        // set. Serialize the job's manifest as the JSON array of
        // `{name, plane, element_kind, transport}` entries the SDK expects
        // (it reads fields via `.get()`, so extra fields are harmless).
        ctx.env.insert(
            "AITHERICON_CHANNELS".into(),
            serde_json::to_string(&job.channels).map_err(|e| {
                ExecutorError::StagingFailed(format!("serialize channel manifest: {e}"))
            })?,
        );

        debug!("injected AITHERICON_* env vars");
        Ok(ctx)
    }
}

/// Fetches the static config blob referenced by `ExecutionSpec.config_ref`
/// and writes it into `spec.config` so downstream hooks + backends see the
/// resolved config exactly as if it had travelled inline.
///
/// The compiler-emitted prepare-transition keeps the per-job NATS token
/// small (just a `config_ref { storage_path }`) — this hook is the
/// counterparty that materialises the config at execution time. Inline-spec
/// jobs (tests, programmatic) pass through unchanged when `config_ref` is
/// `None`.
pub struct FetchConfigHook {
    pub store: Arc<dyn ArtifactStore>,
}

#[async_trait]
impl StagingHook for FetchConfigHook {
    fn name(&self) -> &'static str {
        "fetch_config"
    }

    async fn stage(
        &self,
        _job: &ExecutionJob,
        mut ctx: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        let Some(ref config_ref) = ctx.spec.config_ref else {
            return Ok(ctx);
        };

        // Download to a transient path inside the run_dir root (created by
        // `CreateRunDirectoryHook` ahead of this hook). Deliberately NOT under
        // `inputs_dir` so the backend's input-staging logic doesn't pick it
        // up; deleted right after read so it never bleeds into artifacts.
        let blob_path = ctx.run_dir.root.join("__node_config.json");
        let storage_path = StoragePath(config_ref.storage_path.clone());
        // Clone the overlay before the mutable borrow of `ctx.spec.config`
        // below. Shallow-merged after the fetch so per-job, turn-varying
        // fields (the agent loop's `history`) win over the static blob.
        let overlay = config_ref.overlay.clone();
        self.store
            .download(&storage_path, &blob_path)
            .await
            .map_err(|e| {
                ExecutorError::StagingFailed(format!(
                    "fetch_config: download config blob '{}': {e}",
                    config_ref.storage_path
                ))
            })?;
        let bytes: Vec<u8> = tokio::fs::read(&blob_path).await.map_err(|e| {
            ExecutorError::StagingFailed(format!(
                "fetch_config: read downloaded config blob '{}': {e}",
                config_ref.storage_path
            ))
        })?;
        let resolved: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
            ExecutorError::StagingFailed(format!(
                "fetch_config: parse JSON config blob '{}': {e}",
                config_ref.storage_path
            ))
        })?;
        // Best-effort cleanup; the run-dir is the right place even if this
        // fails (whole tree is removed by the cleanup policy).
        let _ = tokio::fs::remove_file(&blob_path).await;

        debug!(
            storage_path = %config_ref.storage_path,
            bytes = bytes.len(),
            "fetched static node config",
        );
        ctx.spec.config = resolved;
        merge_config_overlay(&mut ctx.spec.config, overlay);
        Ok(ctx)
    }
}

/// Shallow-merge a per-job overlay onto a fetched static config: top-level
/// overlay keys replace the config's. Only object-on-object merges; any
/// other shape (or `None`) is a no-op. The agent loop uses this to ship
/// the turn-varying `history` inline while the large static config (system
/// prompt, tool schemas) stays in object storage.
fn merge_config_overlay(config: &mut serde_json::Value, overlay: Option<serde_json::Value>) {
    if let Some(serde_json::Value::Object(over)) = overlay {
        if let serde_json::Value::Object(base) = config {
            for (k, v) in over {
                base.insert(k, v);
            }
        }
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
                    // Prefer the resolved value from PlanSecretsHook's
                    // side-channel — `value` itself still carries
                    // `{{secret:KEY}}` templates, so writing it verbatim
                    // would land the unresolved placeholder in the input
                    // file the child reads (e.g. Python's AccessibleDict
                    // would surface the literal "{{secret:...}}" string).
                    let effective_value =
                        ctx.resolved_inline_inputs.get(&input.name).unwrap_or(value);
                    let dest = ctx.run_dir.inputs_dir.join(&input.name);
                    let data = serde_json::to_vec_pretty(effective_value).map_err(|e| {
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
                    tokio::fs::write(&dest, content.as_bytes())
                        .await
                        .map_err(|e| {
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
                            // Prefer the resolved storage config from the
                            // PlanSecretsHook side-channel — `config` itself
                            // still carries `{{secret:KEY}}` templates so we
                            // can't authenticate to the storage backend with
                            // the raw spec view when secrets are involved.
                            let resolved_owned = deserialize_resolved_storage(
                                ctx.resolved_input_storage.get(&input.name),
                                "input",
                                &input.name,
                            )?;
                            let effective_config = resolved_owned.as_ref().unwrap_or(config);

                            let final_name = name_with_extension(&input.name, path);
                            let dest = ctx.run_dir.inputs_dir.join(&final_name);
                            let (operator, prefix) =
                                aithericon_executor_storage::build_operator_with_prefix(
                                    effective_config,
                                )
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
                            tokio::fs::write(&dest, data.to_vec()).await.map_err(|e| {
                                ExecutorError::StagingFailed(format!(
                                    "write input '{}': {e}",
                                    input.name
                                ))
                            })?;
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
                                store.download(&storage_path, &dest).await.map_err(|e| {
                                    ExecutorError::StagingFailed(format!(
                                        "download input '{}' from '{}': {e}",
                                        input.name, path
                                    ))
                                })?;
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

/// Serializes the RunContext to `context.json` in the run directory.
///
/// `RunContext`'s `resolved_*` fields are `#[serde(skip)]`, so the on-disk
/// shape carries only the unresolved `{{secret:KEY}}` templates — plaintext
/// secrets never round-trip through this file (Gap #1 fix).
///
/// On Unix the file is chmod'd to `0600` immediately after the write so that
/// even with a permissive umask the file is owner-only.
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

        // Tighten context.json to owner-only. Defense in depth even though
        // the file no longer contains plaintext secrets — the unresolved
        // template strings themselves enumerate which secret keys this
        // execution needs and that itself is sensitive.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = tokio::fs::metadata(&ctx.run_dir.context_file)
                .await
                .map_err(|e| {
                    ExecutorError::StagingFailed(format!(
                        "stat {}: {e}",
                        ctx.run_dir.context_file.display()
                    ))
                })?
                .permissions();
            perms.set_mode(0o600);
            tokio::fs::set_permissions(&ctx.run_dir.context_file, perms)
                .await
                .map_err(|e| {
                    ExecutorError::StagingFailed(format!(
                        "chmod 0600 {}: {e}",
                        ctx.run_dir.context_file.display()
                    ))
                })?;
        }

        debug!(path = %ctx.run_dir.context_file.display(), "context.json written");
        Ok(ctx)
    }
}

/// Plans resolved secret values for `{{secret:KEY}}` patterns *without*
/// mutating the on-disk-serialized fields of `RunContext`.
///
/// Gap #1 fix: this hook used to be `InjectSecretsHook`, which substituted
/// plaintext directly into `ctx.env`, `ctx.spec.config`,
/// `ctx.spec.inputs[].source.storage`, and `ctx.spec.outputs[].upload_to.storage`.
/// The downstream `WriteContextHook` then serialized the whole context to
/// `context.json` — leaking plaintext to disk at default `0644` permissions.
///
/// `PlanSecretsHook` writes only to `RunContext`'s `#[serde(skip)]`
/// side-channel fields:
/// * `resolved_env`              — env keys that contained `{{secret:KEY}}`
/// * `resolved_config`           — fully-resolved `spec.config` overlay (`Option<Value>`)
/// * `resolved_input_storage`    — per-input storage config, by input name
/// * `resolved_output_storage`   — per-output storage config, by output name
///
/// Backends spawning child processes feed `resolved_env` into
/// `tokio::process::Command::env(k, v)`. The HTTP backend (no child) consumes
/// `resolved_config` at request-build time. Plaintext never lands on disk.
///
/// When the job carries a `wrapped_secrets` token and `vault_addr` is configured,
/// the hook unwraps the Vault wrapping token to obtain resolved secrets, then
/// uses them to resolve `{{secret:KEY}}` refs. This means the executor does NOT
/// need broad secret store access — it only unwraps what was explicitly wrapped.
///
/// Falls back to `self.store` for direct resolution when no wrapping token is present.
pub struct PlanSecretsHook {
    pub store: Arc<dyn SecretStore>,
    /// Vault address for unwrapping wrapped secrets. Only `VAULT_ADDR` is needed,
    /// not `VAULT_TOKEN` — the wrapping token itself is used as auth.
    pub vault_addr: Option<String>,
}

#[async_trait]
impl StagingHook for PlanSecretsHook {
    fn name(&self) -> &'static str {
        "plan_secrets"
    }

    async fn stage(
        &self,
        job: &ExecutionJob,
        mut ctx: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        // Determine which store to use for resolving {{secret:KEY}} patterns.
        // If the job carries a Vault wrapping token, unwrap it to get an in-memory
        // store. Otherwise, fall back to the configured store (env, vault, etc.).
        let effective_store: Arc<dyn SecretStore> = match (&job.wrapped_secrets, &self.vault_addr) {
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

        // 1. Plan resolved env values WITHOUT mutating ctx.env.
        //    The on-disk `env` keeps the {{secret:KEY}} templates so that
        //    context.json never carries plaintext. Backends spawning children
        //    feed `resolved_env` (merged with `env` at the call site) into
        //    Command::env(k, v).
        for (k, v) in ctx.env.iter() {
            if v.contains("{{secret:") {
                let json_val = serde_json::Value::String(v.clone());
                let resolved =
                    aithericon_secrets::resolve_secrets(&json_val, effective_store.as_ref())
                        .await
                        .map_err(|e| ExecutorError::SecretResolutionFailed(e.to_string()))?;
                if let serde_json::Value::String(s) = resolved {
                    ctx.resolved_env.insert(k.clone(), s);
                }
            }
        }

        // 2. Plan resolved spec.config WITHOUT mutating ctx.spec.config.
        //    Only the HTTP backend reads this. If the config has no secret
        //    templates we leave `resolved_config = None` so the HTTP backend
        //    falls through to `ctx.spec.config` (preserves the no-vault path).
        if json_contains_secret_template(&ctx.spec.config) {
            ctx.resolved_config = Some(
                aithericon_secrets::resolve_secrets(&ctx.spec.config, effective_store.as_ref())
                    .await
                    .map_err(|e| ExecutorError::SecretResolutionFailed(e.to_string()))?,
            );
        }

        // 3. Plan resolved per-input storage configs WITHOUT mutating
        //    spec.inputs[].source.storage. `StageInputsHook` reads
        //    `resolved_input_storage` first, falling back to spec.inputs.
        for input in ctx.spec.inputs.iter() {
            if let aithericon_executor_domain::InputSource::StoragePath {
                storage: Some(ref config),
                ..
            } = &input.source
            {
                let val = serde_json::to_value(config).map_err(|e| {
                    ExecutorError::SecretResolutionFailed(format!(
                        "failed to serialize storage config for input '{}': {e}",
                        input.name
                    ))
                })?;
                if json_contains_secret_template(&val) {
                    let resolved =
                        aithericon_secrets::resolve_secrets(&val, effective_store.as_ref())
                            .await
                            .map_err(|e| ExecutorError::SecretResolutionFailed(e.to_string()))?;
                    ctx.resolved_input_storage
                        .insert(input.name.clone(), resolved);
                }
            }
        }

        // 4. Plan resolved per-output upload configs.
        for output in ctx.spec.outputs.iter() {
            if let Some(ref upload) = output.upload_to {
                let val = serde_json::to_value(&upload.storage).map_err(|e| {
                    ExecutorError::SecretResolutionFailed(format!(
                        "failed to serialize storage config for output '{}': {e}",
                        output.name
                    ))
                })?;
                if json_contains_secret_template(&val) {
                    let resolved =
                        aithericon_secrets::resolve_secrets(&val, effective_store.as_ref())
                            .await
                            .map_err(|e| ExecutorError::SecretResolutionFailed(e.to_string()))?;
                    ctx.resolved_output_storage
                        .insert(output.name.clone(), resolved);
                }
            }
        }

        // 5. Plan resolved inline input JSON values WITHOUT mutating
        //    spec.inputs[].source.value. `StageInputsHook` reads
        //    `resolved_inline_inputs` first, falling back to `value`.
        //
        //    This is the path the compiler-emitted `__resources["<slug>"]`
        //    envelope flows through — secret fields are spliced into the AIR
        //    as `{{secret:KEY}}` strings, ride into the prepare transition's
        //    `job_inputs[].source.value`, and need to be resolved before
        //    `StageInputsHook` writes `<slug>.json` to the inputs dir (which
        //    the Python runner loads as an `AccessibleDict` global).
        for input in ctx.spec.inputs.iter() {
            if let aithericon_executor_domain::InputSource::Inline { value } = &input.source {
                if json_contains_secret_template(value) {
                    let resolved =
                        aithericon_secrets::resolve_secrets(value, effective_store.as_ref())
                            .await
                            .map_err(|e| ExecutorError::SecretResolutionFailed(e.to_string()))?;
                    ctx.resolved_inline_inputs
                        .insert(input.name.clone(), resolved);
                }
            }
        }

        debug!(
            resolved_env_count = ctx.resolved_env.len(),
            resolved_config = ctx.resolved_config.is_some(),
            resolved_inline_input_count = ctx.resolved_inline_inputs.len(),
            "planned secrets into RunContext side-channel"
        );
        Ok(ctx)
    }
}

/// Cheap structural check: does any string leaf in this JSON value contain
/// `{{secret:`? Used to short-circuit secret resolution when nothing references
/// secrets, so the no-vault tests (and any vanilla config) don't synthesize
/// `resolved_config = Some(...)` and divert the HTTP backend off `spec.config`.
fn json_contains_secret_template(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::String(s) => s.contains("{{secret:"),
        serde_json::Value::Array(a) => a.iter().any(json_contains_secret_template),
        serde_json::Value::Object(o) => o.values().any(json_contains_secret_template),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aithericon_executor_domain::RunDirectory;
    use aithericon_executor_process::ProcessConfig;
    use std::collections::HashMap;
    use std::path::Path;
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
            feed_chunks: false,
            channels: Vec::new(),
            wrapped_secrets: None,
        }
    }

    fn test_context(base_dir: &Path) -> RunContext {
        RunContext::for_test(
            "test-staging",
            ProcessConfig {
                command: "echo".into(),
                args: vec!["hello".into()],
                env: Default::default(),
                working_dir: None,
                inherit_env: true,
            }
            .into_spec(),
            RunDirectory::new(base_dir, "test-staging"),
            Duration::from_secs(60),
        )
    }

    #[test]
    fn merge_config_overlay_replaces_top_level_keys() {
        use serde_json::json;
        // Overlay's `history` replaces the empty static one; other keys stay.
        let mut cfg = json!({"model": "x", "history": [], "system_prompt": "s"});
        merge_config_overlay(
            &mut cfg,
            Some(json!({"history": [{"role": "user", "content": "hi"}]})),
        );
        assert_eq!(cfg["history"].as_array().unwrap().len(), 1);
        assert_eq!(cfg["model"], "x", "non-overlaid keys untouched");
        assert_eq!(cfg["system_prompt"], "s");

        // None overlay is a no-op.
        let mut cfg2 = json!({"a": 1});
        merge_config_overlay(&mut cfg2, None);
        assert_eq!(cfg2["a"], 1);
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
        use aithericon_executor_domain::ChannelManifestEntry;
        use serde_json::json;

        let tmp = PathBuf::from("/tmp/staging-env-test");
        let ctx = test_context(&tmp);

        let mut job = test_job();
        job.channels = vec![
            ChannelManifestEntry {
                name: "progress".into(),
                plane: "control".into(),
                element_kind: "json".into(),
                transport: "jetstream".into(),
            },
            ChannelManifestEntry {
                name: "frames".into(),
                plane: "data".into(),
                element_kind: "binary".into(),
                transport: "nats-latest".into(),
            },
        ];

        let hook = InjectEnvironmentHook;
        let result = hook.stage(&job, ctx).await.unwrap();

        assert!(result.env.contains_key("AITHERICON_RUN_DIR"));
        assert!(result.env.contains_key("AITHERICON_IPC_SOCKET"));
        assert!(result.env.contains_key("AITHERICON_INPUTS_DIR"));
        assert!(result.env.contains_key("AITHERICON_OUTPUTS_DIR"));
        assert!(result.env.contains_key("AITHERICON_ARTIFACTS_DIR"));
        assert_eq!(
            result.env.get("AITHERICON_EXECUTION_ID").unwrap(),
            "test-staging"
        );

        // The channel manifest is injected as the JSON array shape the SDK's
        // `_load_manifest()` parses.
        let channels_env = result
            .env
            .get("AITHERICON_CHANNELS")
            .expect("AITHERICON_CHANNELS injected");
        let parsed: serde_json::Value = serde_json::from_str(channels_env).unwrap();
        assert_eq!(
            parsed,
            json!([
                {"name": "progress", "plane": "control", "element_kind": "json", "transport": "jetstream"},
                {"name": "frames", "plane": "data", "element_kind": "binary", "transport": "nats-latest"},
            ])
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
    // PlanSecretsHook tests
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

    /// `PlanSecretsHook` MUST NOT mutate `ctx.env` — that would land plaintext
    /// in the serialized context.json. It writes only to `resolved_env`, and
    /// only for keys whose templates were resolved.
    #[tokio::test]
    async fn test_plan_secrets_does_not_mutate_env() {
        let store = Arc::new(MockSecretStore(HashMap::from([(
            "MY_SECRET".into(),
            "resolved_value".into(),
        )])));
        let hook = PlanSecretsHook {
            store,
            vault_addr: None,
        };

        let tmp = PathBuf::from("/tmp/staging-plan-env-test");
        let mut ctx = test_context(&tmp);
        ctx.env
            .insert("API_KEY".into(), "{{secret:MY_SECRET}}".into());
        ctx.env.insert("PLAIN".into(), "no_secrets_here".into());

        let result = hook.stage(&test_job(), ctx).await.unwrap();

        // env keeps the unresolved template
        assert_eq!(result.env["API_KEY"], "{{secret:MY_SECRET}}");
        assert_eq!(result.env["PLAIN"], "no_secrets_here");
        // resolved_env carries the plaintext, keyed by env name
        assert_eq!(result.resolved_env["API_KEY"], "resolved_value");
        // PLAIN has no secret pattern → not populated in resolved_env
        assert!(
            !result.resolved_env.contains_key("PLAIN"),
            "PLAIN had no secret template, must not appear in resolved_env"
        );
        // spec.config carried no secret → resolved_config stays None
        assert!(result.resolved_config.is_none());
    }

    /// `WriteContextHook` must preserve the `{{secret:KEY}}` template in the
    /// serialized context.json — and absolutely not include the resolved
    /// plaintext.
    #[tokio::test]
    async fn test_write_context_preserves_secret_template() {
        let store = Arc::new(MockSecretStore(HashMap::from([(
            "API_TOKEN".into(),
            "PLAINTEXT-TOKEN-XYZ".into(),
        )])));

        let tmp = std::env::temp_dir().join(format!(
            "staging-write-template-{}-{}",
            std::process::id(),
            uuid_like()
        ));
        let mut ctx = test_context(&tmp);
        ctx.env
            .insert("API_KEY".into(), "{{secret:API_TOKEN}}".into());
        ctx.spec.config =
            serde_json::json!({"token": "{{secret:API_TOKEN}}", "url": "https://api.example.com"});

        // Run create → plan → write.
        let ctx = CreateRunDirectoryHook
            .stage(&test_job(), ctx)
            .await
            .unwrap();
        let ctx = PlanSecretsHook {
            store,
            vault_addr: None,
        }
        .stage(&test_job(), ctx)
        .await
        .unwrap();

        // Sanity: plaintext is in the side-channel only.
        assert_eq!(ctx.resolved_env["API_KEY"], "PLAINTEXT-TOKEN-XYZ");

        let ctx = WriteContextHook.stage(&test_job(), ctx).await.unwrap();

        let on_disk = std::fs::read_to_string(&ctx.run_dir.context_file).unwrap();
        assert!(
            on_disk.contains("{{secret:API_TOKEN}}"),
            "context.json should preserve the unresolved template: {on_disk}"
        );
        assert!(
            !on_disk.contains("PLAINTEXT-TOKEN-XYZ"),
            "context.json must NOT contain plaintext secret: {on_disk}"
        );
        // The resolved_* fields are #[serde(skip)] so their names must not appear.
        assert!(
            !on_disk.contains("resolved_env"),
            "context.json should not include resolved_env field: {on_disk}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// On Unix, `WriteContextHook` must chmod `context.json` to `0600`.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_write_context_is_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = std::env::temp_dir().join(format!(
            "staging-write-perm-{}-{}",
            std::process::id(),
            uuid_like()
        ));
        let ctx = test_context(&tmp);
        let ctx = CreateRunDirectoryHook
            .stage(&test_job(), ctx)
            .await
            .unwrap();

        // Verify run-dir root is 0700.
        let root_mode = std::fs::metadata(&ctx.run_dir.root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            root_mode, 0o700,
            "run dir root should be 0700, got {root_mode:o}"
        );

        let ctx = WriteContextHook.stage(&test_job(), ctx).await.unwrap();
        let ctx_mode = std::fs::metadata(&ctx.run_dir.context_file)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            ctx_mode, 0o600,
            "context.json should be 0600, got {ctx_mode:o}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Round-tripping a context with populated `resolved_*` fields through
    /// JSON must drop those fields (defense in depth).
    #[tokio::test]
    async fn test_resolved_config_does_not_round_trip() {
        let tmp = PathBuf::from("/tmp/staging-rt-test");
        let mut ctx = test_context(&tmp);
        ctx.resolved_env
            .insert("API_KEY".into(), "ROUNDTRIP-PLAINTEXT".into());
        ctx.resolved_config = Some(serde_json::json!({"token": "ROUNDTRIP-PLAINTEXT"}));
        ctx.resolved_input_storage
            .insert("i1".into(), serde_json::json!({"k": "ROUNDTRIP-PLAINTEXT"}));
        ctx.resolved_output_storage
            .insert("o1".into(), serde_json::json!({"k": "ROUNDTRIP-PLAINTEXT"}));

        let json = serde_json::to_string(&ctx).unwrap();
        assert!(
            !json.contains("ROUNDTRIP-PLAINTEXT"),
            "resolved_* plaintext leaked to JSON: {json}"
        );

        let back: RunContext = serde_json::from_str(&json).unwrap();
        assert!(back.resolved_env.is_empty());
        assert!(back.resolved_config.is_none());
        assert!(back.resolved_input_storage.is_empty());
        assert!(back.resolved_output_storage.is_empty());
    }

    /// Cheap unique suffix without a uuid dependency from tests.
    fn uuid_like() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{n:x}")
    }

    /// Missing-key resolution still fails closed — same contract as the old
    /// `InjectSecretsHook`, just routed through `resolved_env`.
    #[tokio::test]
    async fn test_plan_secrets_missing_key_returns_error() {
        let store = Arc::new(MockSecretStore(HashMap::new())); // empty — all lookups fail
        let hook = PlanSecretsHook {
            store,
            vault_addr: None,
        };

        let tmp = PathBuf::from("/tmp/staging-plan-fail-test");
        let mut ctx = test_context(&tmp);
        ctx.env.insert("TOKEN".into(), "{{secret:MISSING}}".into());

        let result = hook.stage(&test_job(), ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ExecutorError::SecretResolutionFailed(msg) => {
                assert!(
                    msg.contains("MISSING"),
                    "Error should mention the key: {msg}"
                );
            }
            other => panic!("Expected SecretResolutionFailed, got {:?}", other),
        }
    }

    /// `resolved_config` stays `None` when nothing in `spec.config` references
    /// a secret. The HTTP backend relies on this fallback path.
    #[tokio::test]
    async fn test_plan_secrets_no_pattern_leaves_resolved_config_none() {
        let store = Arc::new(MockSecretStore(HashMap::from([(
            "UNUSED".into(),
            "value".into(),
        )])));
        let hook = PlanSecretsHook {
            store,
            vault_addr: None,
        };

        let tmp = PathBuf::from("/tmp/staging-plan-no-pattern");
        let mut ctx = test_context(&tmp);
        ctx.spec.config = serde_json::json!({"url": "https://api.example.com"});

        let result = hook.stage(&test_job(), ctx).await.unwrap();
        assert!(
            result.resolved_config.is_none(),
            "no secret template → resolved_config should remain None to preserve HTTP fallback path"
        );
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
