use aithericon_executor_ipc::proto;

use crate::error::CliError;

/// Check a sidecar response for errors, converting non-OK statuses into `CliError`.
pub fn check_response(resp: proto::SidecarResponse) -> Result<(), CliError> {
    let status =
        proto::ResponseStatus::try_from(resp.status).unwrap_or(proto::ResponseStatus::Error);
    if status == proto::ResponseStatus::Ok {
        Ok(())
    } else {
        Err(CliError::Sidecar {
            status,
            message: resp.error_message,
        })
    }
}

/// Format a successful result for output.
pub fn format_ok(json_mode: bool) -> String {
    if json_mode {
        r#"{"status":"ok"}"#.to_string()
    } else {
        String::new()
    }
}

/// Format an error result for output.
pub fn format_error(json_mode: bool, err: &CliError) -> String {
    if json_mode {
        let msg = err.to_string().replace('\\', "\\\\").replace('"', "\\\"");
        format!(r#"{{"status":"error","message":"{msg}"}}"#)
    } else {
        format!("error: {err}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- check_response tests --

    #[test]
    fn check_response_ok() {
        let resp = proto::SidecarResponse {
            status: proto::ResponseStatus::Ok.into(),
            error_message: String::new(),
        };
        assert!(check_response(resp).is_ok());
    }

    #[test]
    fn check_response_error() {
        let resp = proto::SidecarResponse {
            status: proto::ResponseStatus::Error.into(),
            error_message: "boom".into(),
        };
        let err = check_response(resp).unwrap_err();
        assert_eq!(err.exit_code(), std::process::ExitCode::from(1));
        assert!(err.to_string().contains("boom"));
    }

    #[test]
    fn check_response_not_found() {
        let resp = proto::SidecarResponse {
            status: proto::ResponseStatus::NotFound.into(),
            error_message: "missing".into(),
        };
        let err = check_response(resp).unwrap_err();
        assert_eq!(err.exit_code(), std::process::ExitCode::from(1));
    }

    #[test]
    fn check_response_invalid_argument() {
        let resp = proto::SidecarResponse {
            status: proto::ResponseStatus::InvalidArgument.into(),
            error_message: "bad".into(),
        };
        let err = check_response(resp).unwrap_err();
        assert_eq!(err.exit_code(), std::process::ExitCode::from(1));
    }

    #[test]
    fn check_response_unknown_status_treated_as_error() {
        let resp = proto::SidecarResponse {
            status: 999, // unknown
            error_message: "unknown".into(),
        };
        let err = check_response(resp).unwrap_err();
        assert_eq!(err.exit_code(), std::process::ExitCode::from(1));
    }

    // -- format_ok tests --

    #[test]
    fn format_ok_json_mode() {
        let out = format_ok(true);
        assert_eq!(out, r#"{"status":"ok"}"#);
    }

    #[test]
    fn format_ok_plain_mode() {
        let out = format_ok(false);
        assert!(out.is_empty());
    }

    // -- format_error tests --

    #[test]
    fn format_error_json_mode() {
        let err = CliError::InvalidArgument("bad value".into());
        let out = format_error(true, &err);
        assert!(out.starts_with('{'));
        assert!(out.contains(r#""status":"error""#));
        assert!(out.contains("bad value"));
    }

    #[test]
    fn format_error_plain_mode() {
        let err = CliError::InvalidArgument("bad value".into());
        let out = format_error(false, &err);
        assert!(out.starts_with("error:"));
        assert!(out.contains("bad value"));
    }

    #[test]
    fn format_error_json_escapes_quotes() {
        let err = CliError::InvalidArgument(r#"value "with" quotes"#.into());
        let out = format_error(true, &err);
        // The JSON output must have escaped quotes
        assert!(out.contains(r#"\"with\""#));
        // Must be valid-ish JSON structure
        assert!(out.starts_with('{'));
        assert!(out.ends_with('}'));
    }

    #[test]
    fn format_error_json_escapes_backslashes() {
        let err = CliError::InvalidArgument(r"path\to\file".into());
        let out = format_error(true, &err);
        assert!(out.contains(r"path\\to\\file"));
    }
}
