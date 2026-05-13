use std::path::Path;

use crate::error::CliError;

fn inputs_dir() -> Result<String, CliError> {
    std::env::var("AITHERICON_INPUTS_DIR")
        .map_err(|_| CliError::InvalidArgument("AITHERICON_INPUTS_DIR not set".into()))
}

pub fn list_inputs(json_mode: bool) -> Result<(), CliError> {
    let dir = inputs_dir()?;
    let path = Path::new(&dir);

    if !path.exists() {
        if json_mode {
            println!("[]");
        }
        return Ok(());
    }

    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }
    names.sort();

    if json_mode {
        println!("{}", serde_json::to_string(&names)?);
    } else {
        for name in &names {
            println!("{name}");
        }
    }
    Ok(())
}

pub fn get_input(name: &str, json_mode: bool) -> Result<(), CliError> {
    let dir = inputs_dir()?;
    let path = Path::new(&dir).join(name);

    if !path.exists() {
        return Err(CliError::InvalidArgument(format!(
            "input file not found: {name}"
        )));
    }

    let content = std::fs::read_to_string(&path)?;

    if json_mode {
        // Try to parse as JSON for structured output; fall back to string.
        let value: serde_json::Value = serde_json::from_str(&content)
            .unwrap_or_else(|_| serde_json::Value::String(content.clone()));
        let wrapper = serde_json::json!({ "name": name, "value": value });
        println!("{}", serde_json::to_string(&wrapper)?);
    } else {
        print!("{content}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Global lock to serialize tests that modify AITHERICON_INPUTS_DIR.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_inputs_dir<F: FnOnce(&std::path::Path)>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("AITHERICON_INPUTS_DIR", dir.path()) };
        f(dir.path());
        unsafe { std::env::remove_var("AITHERICON_INPUTS_DIR") };
    }

    fn without_inputs_dir<F: FnOnce()>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("AITHERICON_INPUTS_DIR") };
        f();
    }

    // -- inputs_dir tests --

    #[test]
    fn inputs_dir_not_set() {
        without_inputs_dir(|| {
            let err = inputs_dir().unwrap_err();
            assert!(matches!(err, CliError::InvalidArgument(_)));
            assert!(err.to_string().contains("AITHERICON_INPUTS_DIR"));
        });
    }

    // -- list_inputs tests --

    #[test]
    fn list_inputs_env_not_set() {
        without_inputs_dir(|| {
            let err = list_inputs(false).unwrap_err();
            assert!(matches!(err, CliError::InvalidArgument(_)));
        });
    }

    #[test]
    fn list_inputs_empty_dir() {
        with_inputs_dir(|_| {
            list_inputs(false).unwrap();
        });
    }

    #[test]
    fn list_inputs_with_files() {
        with_inputs_dir(|dir| {
            std::fs::write(dir.join("alpha.json"), "{}").unwrap();
            std::fs::write(dir.join("beta.txt"), "hello").unwrap();
            std::fs::create_dir(dir.join("subdir")).unwrap();
            list_inputs(false).unwrap();
        });
    }

    #[test]
    fn list_inputs_nonexistent_dir() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("AITHERICON_INPUTS_DIR", "/tmp/no-such-dir-ever-12345");
        }
        list_inputs(false).unwrap();
        unsafe { std::env::remove_var("AITHERICON_INPUTS_DIR") };
    }

    #[test]
    fn list_inputs_ignores_subdirs() {
        with_inputs_dir(|dir| {
            std::fs::write(dir.join("file.txt"), "data").unwrap();
            std::fs::create_dir(dir.join("nested")).unwrap();
            std::fs::write(dir.join("nested").join("inner.txt"), "nested").unwrap();
            list_inputs(false).unwrap();
        });
    }

    // -- get_input tests --

    #[test]
    fn get_input_existing_json_file() {
        with_inputs_dir(|dir| {
            std::fs::write(dir.join("config.json"), r#"{"key":"val"}"#).unwrap();
            get_input("config.json", false).unwrap();
        });
    }

    #[test]
    fn get_input_existing_text_file() {
        with_inputs_dir(|dir| {
            std::fs::write(dir.join("readme.txt"), "hello world").unwrap();
            get_input("readme.txt", false).unwrap();
        });
    }

    #[test]
    fn get_input_json_mode_wraps_non_json() {
        with_inputs_dir(|dir| {
            std::fs::write(dir.join("plain.txt"), "not json").unwrap();
            get_input("plain.txt", true).unwrap();
        });
    }

    #[test]
    fn get_input_file_not_found() {
        with_inputs_dir(|_| {
            let err = get_input("nonexistent.txt", false).unwrap_err();
            assert!(matches!(err, CliError::InvalidArgument(_)));
            assert!(err.to_string().contains("not found"));
        });
    }

    #[test]
    fn get_input_env_not_set() {
        without_inputs_dir(|| {
            let err = get_input("anything", false).unwrap_err();
            assert!(matches!(err, CliError::InvalidArgument(_)));
        });
    }
}
