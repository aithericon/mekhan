//! Output selector parsing and resolution for the HTTP backend.
//!
//! Selectors map declared output names to specific parts of the HTTP response.
//! The syntax is dot-separated paths rooted at one of the 5 standard outputs:
//! `body`, `status_code`, `headers`, `content_type`, `response_time_ms`.
//!
//! Examples:
//! - `"body"` — full response body
//! - `"body.data.id"` — nested JSON field
//! - `"headers.x-request-id"` — specific header (case-insensitive)
//! - `"status_code"` — HTTP status code

use std::collections::HashMap;

use serde_json::Value;

use aithericon_executor_domain::ExecutorError;

/// Standard output names produced by the HTTP backend.
pub const STANDARD_OUTPUTS: &[&str] = &[
    "status_code",
    "headers",
    "body",
    "content_type",
    "response_time_ms",
];

/// A parsed output selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// A bare standard output name with no sub-path.
    Base(String),
    /// A standard output name followed by dot-separated path segments.
    DotPath { base: String, segments: Vec<String> },
}

impl Selector {
    /// Parse a selector string.
    ///
    /// Validates that the base (the part before the first `.`) is a known
    /// standard output name.
    pub fn parse(s: &str) -> Result<Self, ExecutorError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(ExecutorError::Config("empty output selector".into()));
        }

        let (base, rest) = match s.find('.') {
            Some(i) => (&s[..i], Some(&s[i + 1..])),
            None => (s, None),
        };

        if !STANDARD_OUTPUTS.contains(&base) {
            return Err(ExecutorError::Config(format!(
                "output selector base '{base}' is not a standard HTTP output; \
                 valid bases: {}",
                STANDARD_OUTPUTS.join(", ")
            )));
        }

        match rest {
            None => Ok(Selector::Base(base.to_string())),
            Some(path) => {
                let segments: Vec<String> =
                    path.split('.').map(|s| s.to_string()).collect();
                if segments.iter().any(|s| s.is_empty()) {
                    return Err(ExecutorError::Config(format!(
                        "output selector '{s}' contains an empty segment"
                    )));
                }
                Ok(Selector::DotPath {
                    base: base.to_string(),
                    segments,
                })
            }
        }
    }

    /// Resolve this selector against the standard outputs map.
    ///
    /// Returns `None` if the path cannot be traversed (non-JSON body,
    /// missing key, out-of-bounds index, etc.).
    pub fn resolve(&self, outputs: &HashMap<String, Value>) -> Option<Value> {
        match self {
            Selector::Base(name) => outputs.get(name).cloned(),
            Selector::DotPath { base, segments } => {
                let root = outputs.get(base)?;

                // Special case: headers are case-insensitive for single-segment paths
                if base == "headers" && segments.len() == 1 {
                    if let Value::Object(map) = root {
                        let target = segments[0].to_ascii_lowercase();
                        for (k, v) in map {
                            if k.to_ascii_lowercase() == target {
                                return Some(v.clone());
                            }
                        }
                        return None;
                    }
                }

                // General dot-path traversal
                let mut current = root;
                for segment in segments {
                    match current {
                        Value::Object(map) => {
                            current = map.get(segment.as_str())?;
                        }
                        Value::Array(arr) => {
                            let idx: usize = segment.parse().ok()?;
                            current = arr.get(idx)?;
                        }
                        _ => return None,
                    }
                }
                Some(current.clone())
            }
        }
    }
}

/// Parse and validate all selectors in an output mapping.
pub fn validate_mapping(
    mapping: &HashMap<String, String>,
) -> Result<HashMap<String, Selector>, ExecutorError> {
    mapping
        .iter()
        .map(|(name, selector_str)| {
            let selector = Selector::parse(selector_str)?;
            Ok((name.clone(), selector))
        })
        .collect()
}

/// Apply a parsed output mapping to produce additional outputs from the
/// standard outputs. Selectors that cannot resolve are silently skipped.
pub fn apply_mapping(
    standard_outputs: &HashMap<String, Value>,
    mapping: &HashMap<String, Selector>,
) -> HashMap<String, Value> {
    let mut extra = HashMap::new();
    for (output_name, selector) in mapping {
        if let Some(value) = selector.resolve(standard_outputs) {
            extra.insert(output_name.clone(), value);
        }
    }
    extra
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Parsing ----

    #[test]
    fn parse_base_selectors() {
        assert_eq!(
            Selector::parse("body").unwrap(),
            Selector::Base("body".into())
        );
        assert_eq!(
            Selector::parse("status_code").unwrap(),
            Selector::Base("status_code".into())
        );
        assert_eq!(
            Selector::parse("headers").unwrap(),
            Selector::Base("headers".into())
        );
        assert_eq!(
            Selector::parse("content_type").unwrap(),
            Selector::Base("content_type".into())
        );
        assert_eq!(
            Selector::parse("response_time_ms").unwrap(),
            Selector::Base("response_time_ms".into())
        );
    }

    #[test]
    fn parse_dot_path() {
        assert_eq!(
            Selector::parse("body.data.id").unwrap(),
            Selector::DotPath {
                base: "body".into(),
                segments: vec!["data".into(), "id".into()],
            }
        );
    }

    #[test]
    fn parse_header_path() {
        assert_eq!(
            Selector::parse("headers.x-request-id").unwrap(),
            Selector::DotPath {
                base: "headers".into(),
                segments: vec!["x-request-id".into()],
            }
        );
    }

    #[test]
    fn parse_trims_whitespace() {
        assert_eq!(
            Selector::parse("  body  ").unwrap(),
            Selector::Base("body".into())
        );
    }

    #[test]
    fn parse_empty_errors() {
        assert!(Selector::parse("").is_err());
        assert!(Selector::parse("  ").is_err());
    }

    #[test]
    fn parse_unknown_base_errors() {
        let err = Selector::parse("unknown.field").unwrap_err();
        assert!(err.to_string().contains("not a standard HTTP output"));
    }

    #[test]
    fn parse_empty_segment_errors() {
        assert!(Selector::parse("body..id").is_err());
        assert!(Selector::parse("body.").is_err());
    }

    // ---- Resolution ----

    fn sample_outputs() -> HashMap<String, Value> {
        HashMap::from([
            ("status_code".into(), serde_json::json!(200)),
            (
                "body".into(),
                serde_json::json!({
                    "data": {
                        "id": 42,
                        "name": "test",
                        "items": [10, 20, 30]
                    }
                }),
            ),
            (
                "headers".into(),
                serde_json::json!({
                    "x-request-id": "abc-123",
                    "Content-Type": "application/json"
                }),
            ),
            (
                "content_type".into(),
                serde_json::json!("application/json"),
            ),
            ("response_time_ms".into(), serde_json::json!(150)),
        ])
    }

    #[test]
    fn resolve_base_body() {
        let outputs = sample_outputs();
        let val = Selector::parse("body").unwrap().resolve(&outputs).unwrap();
        assert_eq!(val["data"]["id"], 42);
    }

    #[test]
    fn resolve_base_status_code() {
        let outputs = sample_outputs();
        let val = Selector::parse("status_code")
            .unwrap()
            .resolve(&outputs)
            .unwrap();
        assert_eq!(val, serde_json::json!(200));
    }

    #[test]
    fn resolve_dot_path_into_json() {
        let outputs = sample_outputs();
        let val = Selector::parse("body.data.id")
            .unwrap()
            .resolve(&outputs)
            .unwrap();
        assert_eq!(val, serde_json::json!(42));
    }

    #[test]
    fn resolve_nested_string() {
        let outputs = sample_outputs();
        let val = Selector::parse("body.data.name")
            .unwrap()
            .resolve(&outputs)
            .unwrap();
        assert_eq!(val, serde_json::json!("test"));
    }

    #[test]
    fn resolve_array_index() {
        let outputs = sample_outputs();
        let val = Selector::parse("body.data.items.1")
            .unwrap()
            .resolve(&outputs)
            .unwrap();
        assert_eq!(val, serde_json::json!(20));
    }

    #[test]
    fn resolve_header_case_insensitive() {
        let outputs = sample_outputs();
        // Lowercase query for mixed-case key
        let val = Selector::parse("headers.x-request-id")
            .unwrap()
            .resolve(&outputs)
            .unwrap();
        assert_eq!(val, serde_json::json!("abc-123"));

        // Different case
        let val = Selector::parse("headers.X-Request-Id")
            .unwrap()
            .resolve(&outputs)
            .unwrap();
        assert_eq!(val, serde_json::json!("abc-123"));
    }

    #[test]
    fn resolve_header_content_type() {
        let outputs = sample_outputs();
        let val = Selector::parse("headers.content-type")
            .unwrap()
            .resolve(&outputs)
            .unwrap();
        assert_eq!(val, serde_json::json!("application/json"));
    }

    #[test]
    fn resolve_missing_path_returns_none() {
        let outputs = sample_outputs();
        assert!(Selector::parse("body.nonexistent")
            .unwrap()
            .resolve(&outputs)
            .is_none());
    }

    #[test]
    fn resolve_deep_missing_returns_none() {
        let outputs = sample_outputs();
        assert!(Selector::parse("body.data.id.further")
            .unwrap()
            .resolve(&outputs)
            .is_none());
    }

    #[test]
    fn resolve_non_json_body_dot_path_returns_none() {
        let mut outputs = sample_outputs();
        outputs.insert("body".into(), serde_json::json!("plain text"));
        assert!(Selector::parse("body.data")
            .unwrap()
            .resolve(&outputs)
            .is_none());
    }

    #[test]
    fn resolve_array_out_of_bounds_returns_none() {
        let outputs = sample_outputs();
        assert!(Selector::parse("body.data.items.99")
            .unwrap()
            .resolve(&outputs)
            .is_none());
    }

    #[test]
    fn resolve_missing_header_returns_none() {
        let outputs = sample_outputs();
        assert!(Selector::parse("headers.x-nonexistent")
            .unwrap()
            .resolve(&outputs)
            .is_none());
    }

    // ---- Mapping ----

    #[test]
    fn validate_mapping_all_valid() {
        let mapping = HashMap::from([
            ("item_id".into(), "body.data.id".into()),
            ("req_id".into(), "headers.x-request-id".into()),
            ("full_body".into(), "body".into()),
        ]);
        let parsed = validate_mapping(&mapping).unwrap();
        assert_eq!(parsed.len(), 3);
    }

    #[test]
    fn validate_mapping_catches_bad_selector() {
        let mapping = HashMap::from([
            ("good".into(), "body.data".into()),
            ("bad".into(), "invalid_base.field".into()),
        ]);
        assert!(validate_mapping(&mapping).is_err());
    }

    #[test]
    fn apply_mapping_produces_extra_outputs() {
        let outputs = sample_outputs();
        let mapping = HashMap::from([
            (
                "item_id".into(),
                Selector::parse("body.data.id").unwrap(),
            ),
            (
                "req_id".into(),
                Selector::parse("headers.x-request-id").unwrap(),
            ),
        ]);
        let extra = apply_mapping(&outputs, &mapping);
        assert_eq!(extra["item_id"], serde_json::json!(42));
        assert_eq!(extra["req_id"], serde_json::json!("abc-123"));
    }

    #[test]
    fn apply_mapping_skips_unresolvable() {
        let outputs = sample_outputs();
        let mapping = HashMap::from([(
            "missing".into(),
            Selector::parse("body.nonexistent").unwrap(),
        )]);
        let extra = apply_mapping(&outputs, &mapping);
        assert!(!extra.contains_key("missing"));
    }

    #[test]
    fn apply_mapping_empty_produces_nothing() {
        let outputs = sample_outputs();
        let mapping = HashMap::new();
        let extra = apply_mapping(&outputs, &mapping);
        assert!(extra.is_empty());
    }
}
