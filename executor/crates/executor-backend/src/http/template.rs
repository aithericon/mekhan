use std::collections::HashMap;
use std::path::PathBuf;

use aithericon_executor_domain::ExecutorError;

/// Resolve `{{variable}}` placeholders in a string.
///
/// Lookup order:
/// 1. `env` — environment variables from RunContext
/// 2. `staged_inputs` — file contents (read on demand) for inline/raw inputs
/// 3. `metadata` — job metadata
///
/// Returns `ExecutorError::Config` if any placeholder cannot be resolved.
pub fn resolve(
    template: &str,
    env: &HashMap<String, String>,
    staged_inputs: &HashMap<String, PathBuf>,
    metadata: &HashMap<String, String>,
) -> Result<String, ExecutorError> {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let end = after_open
            .find("}}")
            .ok_or_else(|| ExecutorError::Config(format!("unclosed template '{{{{' in: {template}")))?;

        let var_name = after_open[..end].trim();
        if var_name.is_empty() {
            return Err(ExecutorError::Config(
                "empty template variable name".into(),
            ));
        }

        let value = lookup(var_name, env, staged_inputs, metadata).ok_or_else(|| {
            ExecutorError::Config(format!("unresolved template variable: {var_name}"))
        })?;
        result.push_str(&value);
        rest = &after_open[end + 2..];
    }
    result.push_str(rest);
    Ok(result)
}

/// Resolve all values in a HashMap of templates.
pub fn resolve_map(
    map: &HashMap<String, String>,
    env: &HashMap<String, String>,
    staged_inputs: &HashMap<String, PathBuf>,
    metadata: &HashMap<String, String>,
) -> Result<HashMap<String, String>, ExecutorError> {
    map.iter()
        .map(|(k, v)| Ok((k.clone(), resolve(v, env, staged_inputs, metadata)?)))
        .collect()
}

fn lookup(
    name: &str,
    env: &HashMap<String, String>,
    staged_inputs: &HashMap<String, PathBuf>,
    metadata: &HashMap<String, String>,
) -> Option<String> {
    // 1. Environment variables
    if let Some(val) = env.get(name) {
        return Some(val.clone());
    }
    // 2. Staged inputs — read file contents for small inline/raw values
    if let Some(path) = staged_inputs.get(name) {
        if let Ok(contents) = std::fs::read_to_string(path) {
            return Some(contents.trim().to_string());
        }
    }
    // 3. Metadata
    if let Some(val) = metadata.get(name) {
        return Some(val.clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn no_templates() {
        let env = HashMap::new();
        let inputs = HashMap::new();
        let meta = HashMap::new();
        assert_eq!(
            resolve("https://example.com/api", &env, &inputs, &meta).unwrap(),
            "https://example.com/api"
        );
    }

    #[test]
    fn simple_substitution() {
        let env = HashMap::from([("host".into(), "api.example.com".into())]);
        let inputs = HashMap::new();
        let meta = HashMap::new();
        assert_eq!(
            resolve("https://{{host}}/v1", &env, &inputs, &meta).unwrap(),
            "https://api.example.com/v1"
        );
    }

    #[test]
    fn multiple_variables() {
        let env = HashMap::from([
            ("host".into(), "api.example.com".into()),
            ("version".into(), "v2".into()),
        ]);
        let inputs = HashMap::new();
        let meta = HashMap::from([("user_id".into(), "42".into())]);
        assert_eq!(
            resolve(
                "https://{{host}}/{{version}}/users/{{user_id}}",
                &env,
                &inputs,
                &meta,
            )
            .unwrap(),
            "https://api.example.com/v2/users/42"
        );
    }

    #[test]
    fn unresolved_variable_errors() {
        let env = HashMap::new();
        let inputs = HashMap::new();
        let meta = HashMap::new();
        let err = resolve("https://{{host}}/api", &env, &inputs, &meta).unwrap_err();
        assert!(err.to_string().contains("unresolved template variable: host"));
    }

    #[test]
    fn unclosed_template_errors() {
        let env = HashMap::new();
        let inputs = HashMap::new();
        let meta = HashMap::new();
        let err = resolve("https://{{host/api", &env, &inputs, &meta).unwrap_err();
        assert!(err.to_string().contains("unclosed template"));
    }

    #[test]
    fn whitespace_trimmed_in_variable_name() {
        let env = HashMap::from([("host".into(), "example.com".into())]);
        let inputs = HashMap::new();
        let meta = HashMap::new();
        assert_eq!(
            resolve("https://{{ host }}/api", &env, &inputs, &meta).unwrap(),
            "https://example.com/api"
        );
    }

    #[test]
    fn staged_input_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user_id");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "user-42").unwrap();

        let env = HashMap::new();
        let inputs = HashMap::from([("user_id".into(), path)]);
        let meta = HashMap::new();
        assert_eq!(
            resolve("https://api.example.com/users/{{user_id}}", &env, &inputs, &meta).unwrap(),
            "https://api.example.com/users/user-42"
        );
    }

    #[test]
    fn lookup_priority_env_first() {
        let env = HashMap::from([("key".into(), "from_env".into())]);
        let inputs = HashMap::new();
        let meta = HashMap::from([("key".into(), "from_meta".into())]);
        assert_eq!(
            resolve("{{key}}", &env, &inputs, &meta).unwrap(),
            "from_env"
        );
    }

    #[test]
    fn resolve_map_works() {
        let env = HashMap::from([("port".into(), "8080".into())]);
        let inputs = HashMap::new();
        let meta = HashMap::new();
        let map = HashMap::from([
            ("Host".into(), "{{port}}".into()),
            ("Static".into(), "value".into()),
        ]);
        let resolved = resolve_map(&map, &env, &inputs, &meta).unwrap();
        assert_eq!(resolved["Host"], "8080");
        assert_eq!(resolved["Static"], "value");
    }
}
