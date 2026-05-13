//! Input resolution for `{{input:NAME}}` patterns in file-ops configs.
//!
//! Delegates to the shared [`aithericon_executor_backend::resolve`] module
//! and maps errors to [`FileOpsError::InputResolution`].

use std::collections::HashMap;
use std::path::PathBuf;

use crate::ops::FileOpsError;

/// Resolve `{{input:NAME}}` references in a JSON config tree.
///
/// Thin wrapper around the shared resolver that maps errors to
/// [`FileOpsError::InputResolution`].
pub fn resolve_inputs(
    config: &mut serde_json::Value,
    staged_inputs: &HashMap<String, PathBuf>,
) -> Result<(), FileOpsError> {
    aithericon_executor_backend::resolve::resolve_inputs(config, staged_inputs)
        .map_err(|e| FileOpsError::InputResolution(e.to_string()))
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
            "operation": "annotate",
            "annotations": "{{input:probe_result}}",
            "path": "data.csv"
        });

        resolve_inputs(&mut config, &staged).unwrap();

        assert_eq!(
            config["annotations"],
            serde_json::json!({"format": "Csv", "num_rows": 1000})
        );
        // Other fields untouched
        assert_eq!(config["path"], serde_json::json!("data.csv"));
    }

    #[test]
    fn resolve_string_interpolation() {
        let (name, path, _tmp) =
            stage_input("subdir", &serde_json::json!("2026/feb"));
        let staged = HashMap::from([(name, path)]);

        let mut config = serde_json::json!({
            "operation": "stat",
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
            "operation": "copy",
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
        // Need at least one staged input so the empty-check fast path is skipped
        let (name, path, _tmp) =
            stage_input("other", &serde_json::json!("irrelevant"));
        let staged = HashMap::from([(name, path)]);

        let mut config = serde_json::json!({
            "path": "{{input:nonexistent}}"
        });

        let err = resolve_inputs(&mut config, &staged).unwrap_err();
        assert!(matches!(err, FileOpsError::InputResolution(_)));
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn resolve_no_patterns_noop() {
        let (name, path, _tmp) =
            stage_input("unused", &serde_json::json!("value"));
        let staged = HashMap::from([(name, path)]);

        let original = serde_json::json!({
            "operation": "stat",
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
        assert!(matches!(err, FileOpsError::InputResolution(_)));
        assert!(err.to_string().contains("must be a JSON string"));
    }

    #[test]
    fn resolve_empty_staged_inputs_is_noop() {
        let staged: HashMap<String, PathBuf> = HashMap::new();
        let original = serde_json::json!({
            "path": "{{input:something}}"
        });
        let mut config = original.clone();

        // Empty staged_inputs → no resolution attempted, patterns pass through
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
}
