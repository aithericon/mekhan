use std::io::Read;

use aithericon_executor_ipc::proto;
use aithericon_executor_ipc::ExecutorSidecarClient;

use crate::error::CliError;
use crate::output::check_response;

pub async fn set_output(
    client: &mut ExecutorSidecarClient<tonic::transport::Channel>,
    name: String,
    value: Option<String>,
    raw: bool,
    from_stdin: bool,
) -> Result<(), CliError> {
    let raw_value = if from_stdin {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        value.ok_or_else(|| CliError::InvalidArgument("value required (or use --stdin)".into()))?
    };

    let value_json = if raw {
        serde_json::to_string(&raw_value)?
    } else {
        // Validate it's valid JSON, then pass through as-is.
        let _: serde_json::Value = serde_json::from_str(&raw_value).map_err(|e| {
            CliError::InvalidArgument(format!("invalid JSON value: {e} (use --raw for strings)"))
        })?;
        raw_value
    };

    let resp = client
        .set_output(proto::SetOutputRequest { name, value_json })
        .await?
        .into_inner();

    check_response(resp)
}

/// File-based fallback when no socket is available.
pub fn set_output_fallback(name: &str, value: &str, raw: bool) -> Result<(), CliError> {
    let outputs_dir = std::env::var("AITHERICON_OUTPUTS_DIR").map_err(|_| CliError::NoSocket)?;

    let value_json = if raw {
        serde_json::to_string(value)?
    } else {
        let _: serde_json::Value = serde_json::from_str(value).map_err(|e| {
            CliError::InvalidArgument(format!("invalid JSON value: {e} (use --raw for strings)"))
        })?;
        value.to_string()
    };

    std::fs::create_dir_all(&outputs_dir)?;
    let path = std::path::Path::new(&outputs_dir).join(format!("{name}.json"));
    std::fs::write(path, value_json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Global lock to serialize tests that modify AITHERICON_OUTPUTS_DIR.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_outputs_dir<F: FnOnce(&std::path::Path)>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("AITHERICON_OUTPUTS_DIR", dir.path()) };
        f(dir.path());
        unsafe { std::env::remove_var("AITHERICON_OUTPUTS_DIR") };
    }

    fn without_outputs_dir<F: FnOnce()>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("AITHERICON_OUTPUTS_DIR") };
        f();
    }

    // -- set_output_fallback tests --

    #[test]
    fn fallback_writes_json_value() {
        with_outputs_dir(|dir| {
            set_output_fallback("result", r#"{"score":42}"#, false).unwrap();
            let content = std::fs::read_to_string(dir.join("result.json")).unwrap();
            assert_eq!(content, r#"{"score":42}"#);
        });
    }

    #[test]
    fn fallback_raw_wraps_as_json_string() {
        with_outputs_dir(|dir| {
            set_output_fallback("greeting", "hello world", true).unwrap();
            let content = std::fs::read_to_string(dir.join("greeting.json")).unwrap();
            assert_eq!(content, r#""hello world""#);
        });
    }

    #[test]
    fn fallback_invalid_json_without_raw() {
        with_outputs_dir(|_| {
            let err = set_output_fallback("key", "not valid json", false).unwrap_err();
            assert!(matches!(err, CliError::InvalidArgument(_)));
            assert!(err.to_string().contains("invalid JSON"));
        });
    }

    #[test]
    fn fallback_no_outputs_dir_env() {
        without_outputs_dir(|| {
            let err = set_output_fallback("key", "42", false).unwrap_err();
            assert!(matches!(err, CliError::NoSocket));
        });
    }

    #[test]
    fn fallback_creates_nested_dirs() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested").join("outputs");
        unsafe { std::env::set_var("AITHERICON_OUTPUTS_DIR", &nested) };
        set_output_fallback("key", "42", false).unwrap();
        assert!(nested.join("key.json").exists());
        unsafe { std::env::remove_var("AITHERICON_OUTPUTS_DIR") };
    }

    #[test]
    fn fallback_overwrites_existing() {
        with_outputs_dir(|dir| {
            set_output_fallback("key", "1", false).unwrap();
            set_output_fallback("key", "2", false).unwrap();
            let content = std::fs::read_to_string(dir.join("key.json")).unwrap();
            assert_eq!(content, "2");
        });
    }

    #[test]
    fn fallback_raw_escapes_special_chars() {
        with_outputs_dir(|dir| {
            set_output_fallback("msg", r#"hello "world""#, true).unwrap();
            let content = std::fs::read_to_string(dir.join("msg.json")).unwrap();
            assert_eq!(content, r#""hello \"world\"""#);
        });
    }
}
