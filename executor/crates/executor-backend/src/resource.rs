//! Shared helper for loading workspace-resource envelopes at runtime.
//!
//! Backends bound to a workspace resource via `resource_alias` receive
//! the resource projection as `<alias>.json` staged in the run dir.
//! [`load_resource`] (typed) is the preferred entry point: it deserializes
//! the file into the backend's resource struct directly so callers never
//! see `serde_json::Value` or have to repeat the staged_inputs / inputs_dir
//! lookup. The untyped [`load_resource_envelope`] is still exposed for
//! backends that do field-by-field overlay (file-ops renames
//! `access_key_id` → `credentials.access_key`, etc.).

use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde_json::Value;

use aithericon_executor_domain::{ExecutorError, RunContext};

/// Locate `<alias>.json` and return its parsed JSON value, treating
/// "file not present" as a hard error. For the rare backend that has a
/// test-harness fallback when the file is missing, use
/// [`try_load_resource_envelope`] instead and decide per-call.
///
/// Lookup order:
/// 1. `run_context.staged_inputs[<alias>.json]` — the staging hook
///    populates this with the canonical path.
/// 2. `run_context.run_dir.inputs_dir.join(<alias>.json)` — fallback for
///    backends or tests that stage directly without going through the
///    hook.
///
/// Errors:
/// - [`ExecutorError::Config`] if the file isn't present (the compiler
///   should have emitted a `BorrowResolution::ResourceEnvelope` for the
///   alias; a missing file means the binding wasn't wired up).
/// - [`ExecutorError::Io`] (via `?`) if the read itself fails.
/// - [`ExecutorError::Config`] if the file content isn't valid JSON.
pub fn load_resource_envelope(
    run_context: &RunContext,
    alias: &str,
) -> Result<Value, ExecutorError> {
    match try_load_resource_envelope(run_context, alias)? {
        Some(v) => Ok(v),
        None => Err(ExecutorError::Config(format!(
            "resource '{alias}' not staged as <alias>.json — \
             compiler must emit a ResourceEnvelope borrow for this step"
        ))),
    }
}

/// Same lookup as [`load_resource_envelope`] but returns `Ok(None)` when
/// the file isn't present. Read/parse errors still surface as `Err`.
pub fn try_load_resource_envelope(
    run_context: &RunContext,
    alias: &str,
) -> Result<Option<Value>, ExecutorError> {
    let filename = format!("{alias}.json");
    let path: PathBuf = run_context
        .staged_inputs
        .get(&filename)
        .cloned()
        .unwrap_or_else(|| run_context.run_dir.inputs_dir.join(&filename));
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path)?;
    let value = serde_json::from_slice(&bytes).map_err(|e| {
        ExecutorError::Config(format!(
            "resource '{alias}' envelope invalid JSON: {e}"
        ))
    })?;
    Ok(Some(value))
}

/// Typed sibling of [`load_resource_envelope`]: deserialize the staged
/// `<alias>.json` directly into the backend's resource shape `T`.
///
/// Use this when the resource shape maps 1:1 onto the file content
/// (SMTP, LLM, Postgres, …). For backends that need field-level adaptation
/// — renames, partial overlays onto an existing config — call
/// [`load_resource_envelope`] and walk the `Value` manually.
pub fn load_resource<T: DeserializeOwned>(
    run_context: &RunContext,
    alias: &str,
) -> Result<T, ExecutorError> {
    match try_load_resource::<T>(run_context, alias)? {
        Some(v) => Ok(v),
        None => Err(ExecutorError::Config(format!(
            "resource '{alias}' not staged as <alias>.json — \
             compiler must emit a ResourceEnvelope borrow for this step"
        ))),
    }
}

/// Soft variant of [`load_resource`] — `Ok(None)` when the file is absent.
/// Parse errors and read errors still surface as `Err`.
pub fn try_load_resource<T: DeserializeOwned>(
    run_context: &RunContext,
    alias: &str,
) -> Result<Option<T>, ExecutorError> {
    let filename = format!("{alias}.json");
    let path: PathBuf = run_context
        .staged_inputs
        .get(&filename)
        .cloned()
        .unwrap_or_else(|| run_context.run_dir.inputs_dir.join(&filename));
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path)?;
    let value = serde_json::from_slice::<T>(&bytes).map_err(|e| {
        ExecutorError::Config(format!(
            "resource '{alias}' envelope invalid for expected shape: {e}"
        ))
    })?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aithericon_executor_domain::{ExecutionSpec, RunDirectory};
    use std::collections::HashMap;
    use std::fs;
    use std::time::Duration;
    use tempfile::TempDir;

    fn ctx_with_inputs_dir(td: &TempDir) -> RunContext {
        RunContext {
            execution_id: "test-exec".into(),
            spec: ExecutionSpec {
                backend: "process".into(),
                inputs: vec![],
                outputs: vec![],
                config: serde_json::json!({}),
                config_ref: None,
            },
            run_dir: RunDirectory::new(td.path(), "test-exec"),
            timeout: Duration::from_secs(60),
            env: HashMap::new(),
            resolved_env: HashMap::new(),
            resolved_config: None,
            resolved_input_storage: HashMap::new(),
            resolved_output_storage: HashMap::new(),
            resolved_inline_inputs: HashMap::new(),
            metadata: HashMap::new(),
            staged_inputs: HashMap::new(),
            expected_outputs: HashMap::new(),
            staged_events: vec![],
            backend_state: serde_json::Value::Null,
        }
    }

    #[test]
    fn missing_file_returns_config_error() {
        let td = TempDir::new().unwrap();
        let ctx = ctx_with_inputs_dir(&td);
        let err = load_resource_envelope(&ctx, "missing").unwrap_err();
        match err {
            ExecutorError::Config(msg) => assert!(msg.contains("missing")),
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[test]
    fn reads_from_inputs_dir_fallback() {
        let td = TempDir::new().unwrap();
        let ctx = ctx_with_inputs_dir(&td);
        fs::create_dir_all(&ctx.run_dir.inputs_dir).unwrap();
        fs::write(
            ctx.run_dir.inputs_dir.join("mail.json"),
            r#"{ "host": "smtp.example.com", "port": 587 }"#,
        )
        .unwrap();
        let envelope = load_resource_envelope(&ctx, "mail").unwrap();
        assert_eq!(envelope["host"], "smtp.example.com");
        assert_eq!(envelope["port"], 587);
    }

    #[test]
    fn prefers_staged_inputs_over_inputs_dir() {
        let td = TempDir::new().unwrap();
        let mut ctx = ctx_with_inputs_dir(&td);
        // Write a decoy to inputs_dir
        fs::create_dir_all(&ctx.run_dir.inputs_dir).unwrap();
        fs::write(
            ctx.run_dir.inputs_dir.join("svc.json"),
            r#"{ "host": "wrong" }"#,
        )
        .unwrap();
        // And the real value to a separate path registered in staged_inputs
        let real = td.path().join("staged/svc.json");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::write(&real, r#"{ "host": "right" }"#).unwrap();
        ctx.staged_inputs.insert("svc.json".into(), real);
        let envelope = load_resource_envelope(&ctx, "svc").unwrap();
        assert_eq!(envelope["host"], "right");
    }

    #[test]
    fn invalid_json_returns_config_error() {
        let td = TempDir::new().unwrap();
        let ctx = ctx_with_inputs_dir(&td);
        fs::create_dir_all(&ctx.run_dir.inputs_dir).unwrap();
        fs::write(
            ctx.run_dir.inputs_dir.join("broken.json"),
            "{ not json",
        )
        .unwrap();
        let err = load_resource_envelope(&ctx, "broken").unwrap_err();
        match err {
            ExecutorError::Config(msg) => assert!(msg.contains("invalid JSON")),
            other => panic!("expected Config, got {other:?}"),
        }
    }
}
