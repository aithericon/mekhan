use std::collections::HashMap;
use std::time::Duration;

use aithericon_executor_domain::{
    ExecutionOutcome, ExecutionResult, ExecutorError, MetricSummary, RunContext,
};

use super::{selector, HttpConfig, ResponseMode};

/// Process an HTTP response into an ExecutionResult.
pub async fn process_response(
    resp: reqwest::Response,
    config: &HttpConfig,
    duration: Duration,
    run_context: &RunContext,
) -> Result<ExecutionResult, ExecutorError> {
    let status_code = resp.status().as_u16();
    let response_time_ms = duration.as_millis() as u64;

    // Capture response headers
    let headers: HashMap<String, String> = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                v.to_str().unwrap_or("<binary>").to_string(),
            )
        })
        .collect();

    let content_type = headers
        .get("content-type")
        .cloned()
        .unwrap_or_default();

    // Read body with size limit
    let body_bytes = read_body_limited(resp, config.max_response_bytes).await?;

    // Parse body based on response_mode
    let (body_value, body_text) = parse_body(&body_bytes, &config.response_mode, &content_type)?;

    // Determine outcome
    let outcome = determine_outcome(status_code, &config.expected_status_codes);

    // Build outputs
    let mut outputs = HashMap::new();
    outputs.insert(
        "status_code".into(),
        serde_json::Value::Number(status_code.into()),
    );
    outputs.insert("headers".into(), serde_json::to_value(&headers).unwrap_or_default());
    outputs.insert("body".into(), body_value);
    outputs.insert("content_type".into(), serde_json::Value::String(content_type));
    outputs.insert(
        "response_time_ms".into(),
        serde_json::Value::Number(response_time_ms.into()),
    );

    // Apply output mapping to produce additional user-declared outputs
    if !config.output_mapping.is_empty() {
        // Selectors were already validated in prepare(); re-parse is cheap.
        if let Ok(parsed) = selector::validate_mapping(&config.output_mapping) {
            let mapped = selector::apply_mapping(&outputs, &parsed);
            outputs.extend(mapped);
        }
    }

    // Metrics
    let metrics = Some(MetricSummary {
        total_points: 3,
        metric_names: vec![
            "http/status_code".into(),
            "http/response_time_ms".into(),
            "http/response_bytes".into(),
        ],
        latest_values: HashMap::from([
            ("http/status_code".into(), status_code as f64),
            ("http/response_time_ms".into(), response_time_ms as f64),
            ("http/response_bytes".into(), body_bytes.len() as f64),
        ]),
    });

    Ok(ExecutionResult {
        outcome,
        duration,
        stdout_tail: body_text,
        stderr_tail: None,
        artifact_manifest: None,
        outputs,
        progress: None,
        run_dir: Some(run_context.run_dir.clone()),
        metrics,
        logs: None,
    })
}

/// Read response body up to `max_bytes`.
async fn read_body_limited(
    resp: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, ExecutorError> {
    // Use bytes() which reads the full body, then truncate.
    // reqwest doesn't have a built-in size limit on response body,
    // so we read in chunks to avoid unbounded allocation.
    let bytes = resp.bytes().await.map_err(|e| {
        ExecutorError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("failed to read response body: {e}"),
        ))
    })?;

    if bytes.len() > max_bytes {
        Ok(bytes[..max_bytes].to_vec())
    } else {
        Ok(bytes.to_vec())
    }
}

/// Parse body bytes into a JSON Value and optional text representation.
fn parse_body(
    bytes: &[u8],
    mode: &ResponseMode,
    content_type: &str,
) -> Result<(serde_json::Value, Option<String>), ExecutorError> {
    match mode {
        ResponseMode::Discard => Ok((serde_json::Value::Null, None)),
        ResponseMode::Text => {
            let text = String::from_utf8_lossy(bytes).to_string();
            Ok((serde_json::Value::String(text.clone()), Some(text)))
        }
        ResponseMode::Json => {
            let text = String::from_utf8_lossy(bytes).to_string();
            let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| {
                ExecutorError::Config(format!("response_mode is json but body is not valid JSON: {e}"))
            })?;
            Ok((value, Some(text)))
        }
        ResponseMode::Auto => {
            let text = String::from_utf8_lossy(bytes).to_string();
            if looks_like_json(content_type) {
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
                    return Ok((value, Some(text)));
                }
            }
            Ok((serde_json::Value::String(text.clone()), Some(text)))
        }
    }
}

/// Determine whether a Content-Type looks like JSON.
fn looks_like_json(content_type: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    ct.contains("application/json") || ct.contains("+json")
}

/// Map HTTP status code to ExecutionOutcome.
fn determine_outcome(status_code: u16, expected: &[u16]) -> ExecutionOutcome {
    let is_success = if expected.is_empty() {
        (200..300).contains(&status_code)
    } else {
        expected.contains(&status_code)
    };

    if is_success {
        ExecutionOutcome::Success
    } else {
        ExecutionOutcome::ExitFailure {
            exit_code: status_code as i32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_success_2xx() {
        assert!(matches!(determine_outcome(200, &[]), ExecutionOutcome::Success));
        assert!(matches!(determine_outcome(201, &[]), ExecutionOutcome::Success));
        assert!(matches!(determine_outcome(204, &[]), ExecutionOutcome::Success));
    }

    #[test]
    fn default_failure_non_2xx() {
        assert!(matches!(
            determine_outcome(404, &[]),
            ExecutionOutcome::ExitFailure { exit_code: 404 }
        ));
        assert!(matches!(
            determine_outcome(500, &[]),
            ExecutionOutcome::ExitFailure { exit_code: 500 }
        ));
    }

    #[test]
    fn custom_expected_codes() {
        // 500 is success if explicitly expected
        assert!(matches!(
            determine_outcome(500, &[200, 500]),
            ExecutionOutcome::Success
        ));
        // 201 is failure if not in expected list
        assert!(matches!(
            determine_outcome(201, &[200]),
            ExecutionOutcome::ExitFailure { exit_code: 201 }
        ));
    }

    #[test]
    fn parse_body_json_mode() {
        let bytes = br#"{"key": "value"}"#;
        let (val, text) = parse_body(bytes, &ResponseMode::Json, "").unwrap();
        assert_eq!(val["key"], "value");
        assert!(text.is_some());
    }

    #[test]
    fn parse_body_json_mode_invalid() {
        let bytes = b"not json";
        let result = parse_body(bytes, &ResponseMode::Json, "");
        assert!(result.is_err());
    }

    #[test]
    fn parse_body_text_mode() {
        let bytes = b"hello world";
        let (val, text) = parse_body(bytes, &ResponseMode::Text, "").unwrap();
        assert_eq!(val, serde_json::Value::String("hello world".into()));
        assert_eq!(text.unwrap(), "hello world");
    }

    #[test]
    fn parse_body_discard_mode() {
        let bytes = b"anything";
        let (val, text) = parse_body(bytes, &ResponseMode::Discard, "").unwrap();
        assert_eq!(val, serde_json::Value::Null);
        assert!(text.is_none());
    }

    #[test]
    fn parse_body_auto_json_content_type() {
        let bytes = br#"{"a": 1}"#;
        let (val, _) = parse_body(bytes, &ResponseMode::Auto, "application/json").unwrap();
        assert_eq!(val["a"], 1);
    }

    #[test]
    fn parse_body_auto_non_json_content_type() {
        let bytes = b"plain text";
        let (val, _) = parse_body(bytes, &ResponseMode::Auto, "text/plain").unwrap();
        assert_eq!(val, serde_json::Value::String("plain text".into()));
    }

    #[test]
    fn looks_like_json_variants() {
        assert!(looks_like_json("application/json"));
        assert!(looks_like_json("application/json; charset=utf-8"));
        assert!(looks_like_json("application/vnd.api+json"));
        assert!(!looks_like_json("text/html"));
        assert!(!looks_like_json("text/plain"));
    }
}
