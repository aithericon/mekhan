//! Shared input resolution for `{{input:NAME}}` and `{{input_path:NAME}}`
//! patterns in backend configs.
//!
//! Follows the same convention as `{{secret:KEY}}` used elsewhere in the
//! executor. Staged inputs (populated by the `StageInputsHook` from
//! `spec.inputs` declarations) are referenced by name inside the JSON config.
//!
//! Two pattern families are supported:
//!
//! ## `{{input:NAME}}` — content resolution
//!
//! - **Full-value replacement**: when the entire JSON string value is
//!   `"{{input:NAME}}"`, the value is replaced with the parsed JSON content
//!   of the staged input file. This supports objects, arrays, numbers, etc.
//!
//! - **String interpolation**: when `{{input:NAME}}` appears within a larger
//!   string (e.g. `"prefix/{{input:subdir}}/file.csv"`), the input's string
//!   value is interpolated in place. The input must be a JSON string in this
//!   case.
//!
//! ## `{{input_path:NAME}}` — path resolution
//!
//! Resolves to the **file system path** of the staged input, without reading
//! its content. Useful when the backend needs a path to a binary file (e.g.
//! images for vision models) rather than its parsed JSON content.
//!
//! - **Full-value replacement**: `"{{input_path:NAME}}"` → `"/path/to/staged/file"`
//! - **String interpolation**: `"dir/{{input_path:NAME}}/suffix"` → `"dir//path/to/staged/file/suffix"`

use std::collections::HashMap;
use std::path::PathBuf;

/// Error type for input resolution failures.
#[derive(Debug, thiserror::Error)]
pub enum InputResolveError {
    /// An input reference (`{{input:NAME}}`) could not be resolved.
    #[error("input resolution failed: {0}")]
    Resolution(String),
}

/// Pattern prefix and suffix for input references.
const PATTERN_PREFIX: &str = "{{input:";
const PATH_PATTERN_PREFIX: &str = "{{input_path:";
const PATTERN_SUFFIX: &str = "}}";

/// Resolve `{{input:NAME}}` and `{{input_path:NAME}}` references in a JSON config tree.
///
/// Walks the tree recursively. String values are checked for both patterns:
/// - `{{input:NAME}}` — replaced with the parsed JSON content of the staged input file
/// - `{{input_path:NAME}}` — replaced with the file system path string of the staged input
///
/// If `staged_inputs` is empty, this is effectively a no-op.
pub fn resolve_inputs(
    config: &mut serde_json::Value,
    staged_inputs: &HashMap<String, PathBuf>,
) -> Result<(), InputResolveError> {
    if staged_inputs.is_empty() {
        return Ok(());
    }
    // Cache loaded inputs to avoid re-reading the same file
    let mut cache: HashMap<String, serde_json::Value> = HashMap::new();
    resolve_value(config, staged_inputs, &mut cache)
}

fn resolve_value(
    value: &mut serde_json::Value,
    staged_inputs: &HashMap<String, PathBuf>,
    cache: &mut HashMap<String, serde_json::Value>,
) -> Result<(), InputResolveError> {
    match value {
        serde_json::Value::String(s) => {
            let has_input = s.contains(PATTERN_PREFIX);
            let has_path = s.contains(PATH_PATTERN_PREFIX);

            if !has_input && !has_path {
                return Ok(());
            }

            // Check for exact match: entire string is "{{input:NAME}}" or "{{input_path:NAME}}"
            if let Some(name) = extract_exact_match(s, PATTERN_PREFIX) {
                let input_value = load_input(&name, staged_inputs, cache)?;
                *value = input_value;
            } else if let Some(name) = extract_exact_match(s, PATH_PATTERN_PREFIX) {
                let path = resolve_path(&name, staged_inputs)?;
                *value = serde_json::Value::String(path);
            } else {
                // Partial match: interpolate within the string
                let resolved = interpolate_string(s, staged_inputs, cache)?;
                *value = serde_json::Value::String(resolved);
            }
        }
        serde_json::Value::Object(map) => {
            for v in map.values_mut() {
                resolve_value(v, staged_inputs, cache)?;
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                resolve_value(v, staged_inputs, cache)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// If the string is exactly `{{prefix:NAME}}`, return the name.
fn extract_exact_match(s: &str, prefix: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.starts_with(prefix) && trimmed.ends_with(PATTERN_SUFFIX) {
        let inner = &trimmed[prefix.len()..trimmed.len() - PATTERN_SUFFIX.len()];
        // Only exact match if there's no other content and no nested patterns
        if !inner.contains("{{") && !inner.is_empty() {
            return Some(inner.to_string());
        }
    }
    None
}

/// Resolve a staged input name to its file system path string.
fn resolve_path(
    name: &str,
    staged_inputs: &HashMap<String, PathBuf>,
) -> Result<String, InputResolveError> {
    let path = staged_inputs.get(name).ok_or_else(|| {
        InputResolveError::Resolution(format!(
            "input '{name}' not found in staged_inputs (available: {})",
            staged_inputs
                .keys()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;
    Ok(path.display().to_string())
}

/// Interpolate all `{{input:NAME}}` and `{{input_path:NAME}}` occurrences within a string.
/// Content-resolved inputs must be JSON string values.
fn interpolate_string(
    s: &str,
    staged_inputs: &HashMap<String, PathBuf>,
    cache: &mut HashMap<String, serde_json::Value>,
) -> Result<String, InputResolveError> {
    let mut result = String::with_capacity(s.len());
    let mut remaining = s;

    while let Some(start) = remaining.find("{{") {
        // Copy text before the pattern
        result.push_str(&remaining[..start]);

        let from_pattern = &remaining[start..];

        if let Some(after_prefix) = from_pattern.strip_prefix(PATH_PATTERN_PREFIX) {
            // {{input_path:NAME}} — resolve to file path
            let end = after_prefix.find(PATTERN_SUFFIX).ok_or_else(|| {
                InputResolveError::Resolution(format!(
                    "unclosed input_path reference in: {s}"
                ))
            })?;
            let name = &after_prefix[..end];
            if name.is_empty() {
                return Err(InputResolveError::Resolution(
                    "empty input name in {{input_path:}}".into(),
                ));
            }
            let path = resolve_path(name, staged_inputs)?;
            result.push_str(&path);
            remaining = &after_prefix[end + PATTERN_SUFFIX.len()..];
        } else if let Some(after_prefix) = from_pattern.strip_prefix(PATTERN_PREFIX) {
            // {{input:NAME}} — resolve to file content
            let end = after_prefix.find(PATTERN_SUFFIX).ok_or_else(|| {
                InputResolveError::Resolution(format!(
                    "unclosed input reference in: {s}"
                ))
            })?;
            let name = &after_prefix[..end];
            if name.is_empty() {
                return Err(InputResolveError::Resolution(
                    "empty input name in {{input:}}".into(),
                ));
            }
            let input_value = load_input(name, staged_inputs, cache)?;
            match input_value.as_str() {
                Some(string_val) => result.push_str(string_val),
                None => {
                    return Err(InputResolveError::Resolution(format!(
                        "input '{name}' used in string interpolation must be a JSON string, \
                         got {}",
                        value_type_name(&input_value)
                    )));
                }
            }
            remaining = &after_prefix[end + PATTERN_SUFFIX.len()..];
        } else {
            // Unknown `{{...}}` — leave as-is (could be `{{secret:...}}`)
            result.push_str("{{");
            remaining = &from_pattern[2..];
        }
    }
    result.push_str(remaining);
    Ok(result)
}

/// Load a staged input file as a JSON value, with caching.
fn load_input(
    name: &str,
    staged_inputs: &HashMap<String, PathBuf>,
    cache: &mut HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value, InputResolveError> {
    if let Some(cached) = cache.get(name) {
        return Ok(cached.clone());
    }

    let path = staged_inputs.get(name).ok_or_else(|| {
        InputResolveError::Resolution(format!(
            "input '{name}' not found in staged_inputs (available: {})",
            staged_inputs
                .keys()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;

    let content = std::fs::read_to_string(path).map_err(|e| {
        InputResolveError::Resolution(format!(
            "failed to read staged input '{name}' from {}: {e}",
            path.display()
        ))
    })?;

    let value: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        InputResolveError::Resolution(format!(
            "failed to parse staged input '{name}' as JSON: {e}"
        ))
    })?;

    cache.insert(name.to_string(), value.clone());
    Ok(value)
}

fn value_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Helper: create a staged input as a temp file containing JSON.
    fn stage_input(
        name: &str,
        value: &serde_json::Value,
    ) -> (String, PathBuf, NamedTempFile) {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "{}", serde_json::to_string(value).unwrap()).unwrap();
        (name.to_string(), tmp.path().to_path_buf(), tmp)
    }

    #[test]
    fn resolve_full_value_replacement() {
        let (name, path, _tmp) = stage_input(
            "probe_result",
            &serde_json::json!({"format": "Csv", "num_rows": 1000}),
        );
        let staged = HashMap::from([(name, path)]);

        let mut config = serde_json::json!({
            "annotations": "{{input:probe_result}}",
            "path": "data.csv"
        });

        resolve_inputs(&mut config, &staged).unwrap();

        assert_eq!(
            config["annotations"],
            serde_json::json!({"format": "Csv", "num_rows": 1000})
        );
        assert_eq!(config["path"], serde_json::json!("data.csv"));
    }

    #[test]
    fn resolve_string_interpolation() {
        let (name, path, _tmp) =
            stage_input("subdir", &serde_json::json!("2026/feb"));
        let staged = HashMap::from([(name, path)]);

        let mut config = serde_json::json!({
            "path": "data/{{input:subdir}}/file.csv"
        });

        resolve_inputs(&mut config, &staged).unwrap();

        assert_eq!(
            config["path"],
            serde_json::json!("data/2026/feb/file.csv")
        );
    }

    #[test]
    fn resolve_nested_objects() {
        let (name, path, _tmp) =
            stage_input("target", &serde_json::json!("nested/path.csv"));
        let staged = HashMap::from([(name, path)]);

        let mut config = serde_json::json!({
            "source": "{{input:target}}",
            "nested": {
                "deep": {
                    "value": "prefix/{{input:target}}"
                }
            }
        });

        resolve_inputs(&mut config, &staged).unwrap();

        assert_eq!(config["source"], serde_json::json!("nested/path.csv"));
        assert_eq!(
            config["nested"]["deep"]["value"],
            serde_json::json!("prefix/nested/path.csv")
        );
    }

    #[test]
    fn resolve_missing_input_errors() {
        let (name, path, _tmp) =
            stage_input("other", &serde_json::json!("irrelevant"));
        let staged = HashMap::from([(name, path)]);

        let mut config = serde_json::json!({
            "path": "{{input:nonexistent}}"
        });

        let err = resolve_inputs(&mut config, &staged).unwrap_err();
        assert!(matches!(err, InputResolveError::Resolution(_)));
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn resolve_no_patterns_noop() {
        let (name, path, _tmp) =
            stage_input("unused", &serde_json::json!("value"));
        let staged = HashMap::from([(name, path)]);

        let original = serde_json::json!({
            "path": "data/file.csv"
        });
        let mut config = original.clone();

        resolve_inputs(&mut config, &staged).unwrap();

        assert_eq!(config, original);
    }

    #[test]
    fn resolve_multiple_references() {
        let (name1, path1, _tmp1) =
            stage_input("src", &serde_json::json!("source.csv"));
        let (name2, path2, _tmp2) =
            stage_input("dst", &serde_json::json!("dest.csv"));
        let staged = HashMap::from([(name1, path1), (name2, path2)]);

        let mut config = serde_json::json!({
            "source": "{{input:src}}",
            "destination": "{{input:dst}}"
        });

        resolve_inputs(&mut config, &staged).unwrap();

        assert_eq!(config["source"], serde_json::json!("source.csv"));
        assert_eq!(config["destination"], serde_json::json!("dest.csv"));
    }

    #[test]
    fn resolve_interpolation_non_string_input_errors() {
        let (name, path, _tmp) = stage_input(
            "obj",
            &serde_json::json!({"key": "value"}),
        );
        let staged = HashMap::from([(name, path)]);

        let mut config = serde_json::json!({
            "path": "prefix/{{input:obj}}/suffix"
        });

        let err = resolve_inputs(&mut config, &staged).unwrap_err();
        assert!(matches!(err, InputResolveError::Resolution(_)));
        assert!(err.to_string().contains("must be a JSON string"));
    }

    #[test]
    fn resolve_empty_staged_inputs_is_noop() {
        let staged: HashMap<String, PathBuf> = HashMap::new();
        let original = serde_json::json!({
            "path": "{{input:something}}"
        });
        let mut config = original.clone();

        resolve_inputs(&mut config, &staged).unwrap();

        assert_eq!(config, original);
    }

    #[test]
    fn resolve_array_elements() {
        let (name, path, _tmp) =
            stage_input("tag", &serde_json::json!("important"));
        let staged = HashMap::from([(name, path)]);

        let mut config = serde_json::json!({
            "tags": ["static", "{{input:tag}}"]
        });

        resolve_inputs(&mut config, &staged).unwrap();

        assert_eq!(
            config["tags"],
            serde_json::json!(["static", "important"])
        );
    }

    #[test]
    fn resolve_same_input_referenced_twice() {
        let (name, path, _tmp) =
            stage_input("path", &serde_json::json!("shared.csv"));
        let staged = HashMap::from([(name, path)]);

        let mut config = serde_json::json!({
            "source": "{{input:path}}",
            "destination": "backup/{{input:path}}"
        });

        resolve_inputs(&mut config, &staged).unwrap();

        assert_eq!(config["source"], serde_json::json!("shared.csv"));
        assert_eq!(
            config["destination"],
            serde_json::json!("backup/shared.csv")
        );
    }

    // -- {{input_path:NAME}} tests --

    #[test]
    fn resolve_path_full_value_replacement() {
        let (name, path, _tmp) =
            stage_input("image", &serde_json::json!("unused content"));
        let expected_path = path.display().to_string();
        let staged = HashMap::from([(name, path)]);

        let mut config = serde_json::json!({
            "image_file": "{{input_path:image}}"
        });

        resolve_inputs(&mut config, &staged).unwrap();

        assert_eq!(config["image_file"], serde_json::json!(expected_path));
    }

    #[test]
    fn resolve_path_string_interpolation() {
        let (name, path, _tmp) =
            stage_input("doc", &serde_json::json!("unused"));
        let expected_path = path.display().to_string();
        let staged = HashMap::from([(name, path)]);

        let mut config = serde_json::json!({
            "command": "process --file={{input_path:doc}}"
        });

        resolve_inputs(&mut config, &staged).unwrap();

        assert_eq!(
            config["command"],
            serde_json::json!(format!("process --file={expected_path}"))
        );
    }

    #[test]
    fn resolve_path_missing_input_errors() {
        let staged: HashMap<String, PathBuf> =
            HashMap::from([("other".into(), PathBuf::from("/tmp/other"))]);

        let mut config = serde_json::json!({
            "path": "{{input_path:nonexistent}}"
        });

        let err = resolve_inputs(&mut config, &staged).unwrap_err();
        assert!(matches!(err, InputResolveError::Resolution(_)));
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn resolve_path_and_content_mixed() {
        let (name1, path1, _tmp1) =
            stage_input("image", &serde_json::json!("binary data"));
        let (name2, path2, _tmp2) =
            stage_input("config", &serde_json::json!({"key": "value"}));
        let expected_path = path1.display().to_string();
        let staged = HashMap::from([(name1, path1), (name2, path2)]);

        let mut config = serde_json::json!({
            "image_path": "{{input_path:image}}",
            "settings": "{{input:config}}"
        });

        resolve_inputs(&mut config, &staged).unwrap();

        assert_eq!(config["image_path"], serde_json::json!(expected_path));
        assert_eq!(config["settings"], serde_json::json!({"key": "value"}));
    }

    #[test]
    fn resolve_path_in_array() {
        let (name, path, _tmp) =
            stage_input("photo", &serde_json::json!("unused"));
        let expected_path = path.display().to_string();
        let staged = HashMap::from([(name, path)]);

        let mut config = serde_json::json!({
            "images": [
                {"path": "{{input_path:photo}}"}
            ]
        });

        resolve_inputs(&mut config, &staged).unwrap();

        assert_eq!(
            config["images"][0]["path"],
            serde_json::json!(expected_path)
        );
    }

    #[test]
    fn resolve_path_does_not_read_file_content() {
        // The file doesn't need to contain valid JSON for input_path
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "this is not JSON at all! \x00\x01\x02").unwrap();
        let staged = HashMap::from([
            ("binary".to_string(), tmp.path().to_path_buf()),
        ]);
        let expected_path = tmp.path().display().to_string();

        let mut config = serde_json::json!({
            "file": "{{input_path:binary}}"
        });

        resolve_inputs(&mut config, &staged).unwrap();
        assert_eq!(config["file"], serde_json::json!(expected_path));
    }
}
