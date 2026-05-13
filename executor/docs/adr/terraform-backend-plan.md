# Terraform/OpenTofu Backend Implementation Plan

**Status**: Proposed
**Date**: 2026-02-09

## Overview

Add a new `terraform` backend to aithericon-executor that supports both Terraform and OpenTofu CLIs, full lifecycle operations, flexible plan/apply workflows, and remote state management.

## Requirements

- **Operations**: Full lifecycle (init, plan, apply, destroy, output, validate)
- **Workflow**: Both single-shot (separate jobs) and auto-apply (plan+apply in one job) modes
- **CLI**: Support both `terraform` and `tofu` binaries via config
- **State**: Full remote state backend support with locking

---

## Implementation

### 1. Core Config Types

**File**: `crates/executor-backend/src/terraform/config.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerraformConfig {
    /// CLI binary: "terraform" (default) or "tofu"
    #[serde(default = "default_terraform")]
    pub binary: String,

    /// Action to perform
    pub action: TerraformAction,

    /// Working directory containing .tf files (relative to run_dir)
    #[serde(default)]
    pub working_dir: Option<String>,

    /// State backend configuration
    #[serde(default)]
    pub state_backend: Option<StateBackend>,

    /// Backend config overrides (-backend-config)
    #[serde(default)]
    pub backend_config: HashMap<String, String>,

    /// Variable values (-var)
    #[serde(default)]
    pub vars: HashMap<String, String>,

    /// Variable file paths (-var-file), resolved from inputs
    #[serde(default)]
    pub var_files: Vec<String>,

    /// For apply: use saved plan file from inputs
    #[serde(default)]
    pub plan_file: Option<String>,

    /// For plan/apply: auto-approve without human review
    #[serde(default)]
    pub auto_approve: bool,

    /// Force state reconfiguration on init (-reconfigure)
    #[serde(default)]
    pub reconfigure_backend: bool,

    /// Migrate state to new backend (-migrate-state)
    #[serde(default)]
    pub migrate_state: bool,

    /// State locking (-lock=true/false)
    #[serde(default = "default_true")]
    pub lock: bool,

    /// Lock timeout (-lock-timeout)
    #[serde(default)]
    pub lock_timeout: Option<String>,

    /// Additional CLI flags
    #[serde(default)]
    pub extra_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerraformAction {
    Init,
    Validate,
    Plan,
    Apply,
    Destroy,
    Output,
    /// Combined: init + plan + apply (requires auto_approve=true)
    PlanApply,
    /// Pull remote state to local file
    StatePull,
    /// Push local state to remote
    StatePush { #[serde(default)] force: bool },
    /// List resources in state
    StateList,
    /// Show specific resource in state
    StateShow { address: String },
    /// Remove resource from state
    StateRm { addresses: Vec<String> },
    /// Move resource in state
    StateMv { source: String, destination: String },
    /// Import existing resource
    Import { address: String, id: String },
    /// Force unlock state (emergency)
    ForceUnlock { lock_id: String },
}
```

### 2. State Backend Types

**File**: `crates/executor-backend/src/terraform/state.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StateBackend {
    /// Local state (default)
    Local {
        #[serde(default)]
        path: Option<String>,
    },

    /// S3-compatible backend
    S3 {
        bucket: String,
        key: String,
        region: String,
        #[serde(default)]
        endpoint: Option<String>,  // For MinIO, etc.
        #[serde(default)]
        dynamodb_table: Option<String>,  // For state locking
        #[serde(default)]
        encrypt: bool,
    },

    /// Google Cloud Storage
    Gcs {
        bucket: String,
        prefix: String,
    },

    /// Azure Blob Storage
    AzureRm {
        storage_account_name: String,
        container_name: String,
        key: String,
        #[serde(default)]
        use_msi: bool,
    },

    /// Terraform Cloud / Enterprise
    Remote {
        organization: String,
        #[serde(default)]
        hostname: Option<String>,
        workspaces: RemoteWorkspaceConfig,
    },

    /// HTTP backend
    Http {
        address: String,
        #[serde(default)]
        lock_address: Option<String>,
        #[serde(default)]
        unlock_address: Option<String>,
    },

    /// Consul
    Consul {
        address: String,
        path: String,
    },

    /// PostgreSQL
    Pg {
        conn_str: String,
        schema_name: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RemoteWorkspaceConfig {
    Name { name: String },
    Prefix { prefix: String },
}
```

### 3. Backend Implementation

**File**: `crates/executor-backend/src/terraform/backend.rs`

```rust
pub struct TerraformBackend {
    max_output_bytes: usize,
}

#[async_trait]
impl ExecutionBackend for TerraformBackend {
    fn name(&self) -> &'static str { "terraform" }

    fn supports(&self, spec: &ExecutionSpec) -> bool {
        spec.backend == "terraform" || spec.backend == "tofu"
    }

    async fn prepare(&self, _job: &ExecutionJob, mut ctx: RunContext)
        -> Result<RunContext, ExecutorError> {
        let config = TerraformConfig::from_spec(&ctx.spec)?;

        // Validate config
        if matches!(config.action, TerraformAction::PlanApply) && !config.auto_approve {
            return Err(ExecutorError::Config(
                "plan_apply action requires auto_approve=true".into()
            ));
        }

        // Resolve working directory
        let work_dir = match &config.working_dir {
            Some(d) => ctx.run_dir.base().join(d),
            None => ctx.run_dir.inputs_dir().to_path_buf(),
        };

        // Store resolved state
        ctx.backend_state = json!({
            "config": config,
            "work_dir": work_dir,
        });

        Ok(ctx)
    }

    async fn execute(&self, ctx: &RunContext, status_cb: StatusCallback,
                     cancel: CancellationToken) -> Result<ExecutionResult, ExecutorError> {
        let state: ResolvedState = serde_json::from_value(ctx.backend_state.clone())?;

        match state.config.action {
            TerraformAction::Init => self.run_init(&state, ctx, &status_cb, &cancel).await,
            TerraformAction::Plan => self.run_plan(&state, ctx, &status_cb, &cancel).await,
            TerraformAction::Apply => self.run_apply(&state, ctx, &status_cb, &cancel).await,
            TerraformAction::Destroy => self.run_destroy(&state, ctx, &status_cb, &cancel).await,
            TerraformAction::Validate => self.run_validate(&state, ctx, &status_cb, &cancel).await,
            TerraformAction::Output => self.run_output(&state, ctx, &status_cb, &cancel).await,
            TerraformAction::PlanApply => self.run_plan_apply(&state, ctx, &status_cb, &cancel).await,
            // State operations...
            _ => self.run_state_command(&state, ctx, &status_cb, &cancel).await,
        }
    }
}
```

### 4. Feature Flag & Registration

**`crates/executor-backend/Cargo.toml`**:
```toml
[features]
terraform = []  # No extra deps - uses process spawning
```

**`crates/executor-service/src/main.rs`**:
```rust
#[cfg(feature = "terraform")]
{
    registry = registry.register(
        TerraformBackend::new().with_max_output_bytes(config.max_output_bytes)
    );
    info!("terraform backend registered (supports terraform and tofu)");
}
```

---

## State Backend Credential Handling

Credentials for remote state backends are passed via environment variables, integrating with existing `InjectSecretsHook`:

| Backend | Required Env Vars |
|---------|-------------------|
| S3 | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN` (optional) |
| GCS | `GOOGLE_CREDENTIALS` or `GOOGLE_APPLICATION_CREDENTIALS` |
| AzureRM | `ARM_ACCESS_KEY` or `ARM_SAS_TOKEN` or MSI |
| Remote (TFC) | `TF_TOKEN_app_terraform_io` |
| Consul | `CONSUL_HTTP_TOKEN` |
| Pg | Connection string in config or `PGHOST`, `PGUSER`, etc. |

---

## Example Job Specs

### Init + Plan (separate jobs)
```json
{
  "type": "terraform",
  "inputs": [{"name": "config", "source": {"url": "s3://bucket/infra.tar.gz"}}],
  "outputs": [{"name": "plan", "path": "plan.tfplan"}],
  "config": {
    "binary": "tofu",
    "action": "plan",
    "vars": {"environment": "staging"},
    "backend_config": {"bucket": "my-state-bucket"}
  }
}
```

### Auto Plan+Apply
```json
{
  "type": "terraform",
  "config": {
    "action": "plan_apply",
    "auto_approve": true,
    "vars": {"instance_count": "3"}
  }
}
```

### S3 Remote State with Vault Secrets
```json
{
  "type": "terraform",
  "config": {
    "action": "apply",
    "auto_approve": true,
    "state_backend": {
      "type": "s3",
      "bucket": "my-tf-state",
      "key": "prod/network.tfstate",
      "region": "us-east-1",
      "dynamodb_table": "tf-locks"
    }
  },
  "secrets": {
    "vault_path": "secret/data/aws/terraform",
    "env_mapping": {
      "access_key": "AWS_ACCESS_KEY_ID",
      "secret_key": "AWS_SECRET_ACCESS_KEY"
    }
  }
}
```

---

## Files to Create/Modify

| File | Action |
|------|--------|
| `crates/executor-backend/src/terraform/mod.rs` | Create |
| `crates/executor-backend/src/terraform/config.rs` | Create |
| `crates/executor-backend/src/terraform/state.rs` | Create |
| `crates/executor-backend/src/terraform/backend.rs` | Create |
| `crates/executor-backend/src/terraform/tests.rs` | Create |
| `crates/executor-backend/src/lib.rs` | Modify (add feature export) |
| `crates/executor-backend/Cargo.toml` | Modify (add feature) |
| `crates/executor-service/Cargo.toml` | Modify (add feature) |
| `crates/executor-service/src/main.rs` | Modify (register backend) |
| `crates/executor-service/tests/terraform.rs` | Create |

---

## Estimated Effort

| Component | LOC |
|-----------|-----|
| Config types + state backends | ~200 |
| Backend implementation | ~300 |
| Action implementations | ~400 |
| Tests | ~400 |
| **Total** | **~1300** |

Difficulty: **Medium** - The executor's backend architecture is clean and extensible. Most complexity is in Terraform-specific semantics (state, plan/apply workflow) rather than integration.

---

## Future Enhancements (out of scope)

- State file backup/versioning via OpenDAL
- Workspace support (`terraform workspace`)
- Provider mirror configuration
- Plan diff parsing for structured output
- Cost estimation integration
- Drift detection mode
- State encryption at rest
