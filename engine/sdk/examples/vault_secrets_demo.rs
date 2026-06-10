//! Example: Vault secrets demo — end-to-end secret delivery via response wrapping.
//!
//! Uses the [`executor_lifecycle`] component to wire the full executor lifecycle
//! (submission, status tracking, retry, dead-letter, cancellation, events) in a
//! single function call, then seeds a job whose `spec.config.env` contains
//! `{{secret:KEY}}` references.
//!
//! The secret lifecycle:
//! 1. Secrets are stored in Vault KV (seeded externally before engine starts)
//! 2. The seed job's `spec.config.env` contains `{{secret:KEY}}` refs
//! 3. Petri-lab resolves refs, wraps into a single-use Vault wrapping token
//! 4. Only the wrapping token travels on NATS (not plaintext secrets)
//! 5. The executor unwraps the token, builds an InMemorySecretStore
//! 6. InjectSecretsHook resolves `{{secret:KEY}}` refs in spec.config.env
//! 7. The process runs with secrets available as environment variables
//!
//! The process command echoes the resolved secrets so the demo can verify
//! they arrived correctly by inspecting the completed token's detail output.
//!
//! ## Environment variables
//!
//! ```bash
//! EXECUTOR_ENABLED=true
//! EXECUTOR_SIGNAL_ROUTES=accepted:sig_accepted,running:sig_running,completed:sig_completed,failed:sig_failed,timed_out:sig_timed_out
//! EXECUTOR_NAMESPACE=executor_jobs
//! NATS_URL=nats://localhost:4333
//! VAULT_ADDR=http://localhost:8200
//! VAULT_TOKEN=demo-root-token
//! ```
//!
//! ## Running
//!
//! ```bash
//! just vault-secrets-demo
//! ```

use aithericon_sdk::prelude::*;

// ---------------------------------------------------------------------------
// Net definition
// ---------------------------------------------------------------------------

fn definition(ctx: &mut Context) {
    // -- Inbox place -----------------------------------------------------------

    let exec_queue = ctx.state::<ExecutorSubmitInput>("exec_queue", "Execution Queue");

    // -- Seed data (with secret refs in env) ----------------------------------
    // Seed before passing exec_queue to executor_lifecycle (which takes ownership).

    ctx.seed(
        &exec_queue,
        vec![ExecutorSubmitInput {
            job_id: "vault-secret-test".into(),
            run: 0,
            retries: 0,
            max_retries: 1,
            execution_id: "exec-vault-secret-test".into(),
            spec: ExecutionSpec {
                backend: "process".into(),
                config: serde_json::json!({
                    "command": "sh",
                    "args": ["-c", "echo SECRETS_RESOLVED: API_KEY=$API_KEY DB_PASS=$DB_PASS"],
                    "env": {
                        "API_KEY": "{{secret:demo/api#key}}",
                        "DB_PASS": "{{secret:demo/db#password}}"
                    }
                }),
                inputs: vec![],
                outputs: vec![],
                config_ref: None,
            },
        }],
    );

    // -- Executor lifecycle (submission, status, retry, DLQ, cancel, events) ---

    let _handles = executor_lifecycle(
        ctx,
        ExecutorBridges {
            inbox: exec_queue,
            result_out: None,
            failure_out: None,
            process_id: None,
            process_step: None,
            catalogue: true,
            process: false,
            control_in: None,
        },
    );

    // _handles.completed  — terminal place for successful executions
    // _handles.dead_letter — terminal place for dead-lettered executions
    // _handles.effect_errors — place where effect handler errors land
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    aithericon_sdk::run(
        "vault-secrets-demo",
        "Vault secrets demo: end-to-end secret delivery via response wrapping",
        definition,
    );
}
