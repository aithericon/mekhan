use std::fmt;
use std::process::ExitCode;

use aithericon_executor_ipc::proto;

/// All errors the CLI can encounter.
#[derive(Debug)]
pub enum CliError {
    /// Socket path not provided via --socket or AITHERICON_IPC_SOCKET.
    NoSocket,
    /// Failed to connect to the sidecar Unix socket.
    Connection(String),
    /// Sidecar returned a non-OK response.
    Sidecar {
        status: proto::ResponseStatus,
        message: String,
    },
    /// gRPC transport error.
    Grpc(Box<tonic::Status>),
    /// Invalid CLI arguments.
    InvalidArgument(String),
    /// I/O error (reading files, stdin).
    Io(std::io::Error),
    /// JSON parsing error.
    Json(serde_json::Error),
}

impl CliError {
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::NoSocket | Self::Connection(_) => ExitCode::from(2),
            Self::Sidecar { .. } | Self::Grpc(_) => ExitCode::from(1),
            Self::InvalidArgument(_) => ExitCode::from(3),
            Self::Io(_) | Self::Json(_) => ExitCode::from(4),
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSocket => write!(
                f,
                "no socket path: set AITHERICON_IPC_SOCKET or use --socket"
            ),
            Self::Connection(msg) => write!(f, "connection failed: {msg}"),
            Self::Sidecar { status, message } => {
                write!(f, "sidecar error ({status:?})")?;
                if !message.is_empty() {
                    write!(f, ": {message}")?;
                }
                Ok(())
            }
            Self::Grpc(status) => write!(f, "grpc error: {status}"),
            Self::InvalidArgument(msg) => write!(f, "invalid argument: {msg}"),
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::Json(e) => write!(f, "json error: {e}"),
        }
    }
}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for CliError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<tonic::Status> for CliError {
    fn from(s: tonic::Status) -> Self {
        Self::Grpc(Box::new(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- exit_code tests --

    #[test]
    fn exit_code_no_socket() {
        assert_eq!(CliError::NoSocket.exit_code(), ExitCode::from(2));
    }

    #[test]
    fn exit_code_connection() {
        let e = CliError::Connection("refused".into());
        assert_eq!(e.exit_code(), ExitCode::from(2));
    }

    #[test]
    fn exit_code_sidecar() {
        let e = CliError::Sidecar {
            status: proto::ResponseStatus::Error,
            message: "boom".into(),
        };
        assert_eq!(e.exit_code(), ExitCode::from(1));
    }

    #[test]
    fn exit_code_grpc() {
        let e = CliError::Grpc(Box::new(tonic::Status::internal("fail")));
        assert_eq!(e.exit_code(), ExitCode::from(1));
    }

    #[test]
    fn exit_code_invalid_argument() {
        let e = CliError::InvalidArgument("bad".into());
        assert_eq!(e.exit_code(), ExitCode::from(3));
    }

    #[test]
    fn exit_code_io() {
        let e = CliError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"));
        assert_eq!(e.exit_code(), ExitCode::from(4));
    }

    #[test]
    fn exit_code_json() {
        let e: CliError = serde_json::from_str::<serde_json::Value>("{{bad}}")
            .unwrap_err()
            .into();
        assert_eq!(e.exit_code(), ExitCode::from(4));
    }

    // -- Display tests --

    #[test]
    fn display_no_socket() {
        let msg = CliError::NoSocket.to_string();
        assert!(msg.contains("no socket path"));
        assert!(msg.contains("AITHERICON_IPC_SOCKET"));
    }

    #[test]
    fn display_connection() {
        let msg = CliError::Connection("refused".into()).to_string();
        assert!(msg.contains("connection failed"));
        assert!(msg.contains("refused"));
    }

    #[test]
    fn display_sidecar_with_message() {
        let e = CliError::Sidecar {
            status: proto::ResponseStatus::NotFound,
            message: "phase not found".into(),
        };
        let msg = e.to_string();
        assert!(msg.contains("sidecar error"));
        assert!(msg.contains("phase not found"));
    }

    #[test]
    fn display_sidecar_empty_message() {
        let e = CliError::Sidecar {
            status: proto::ResponseStatus::Error,
            message: String::new(),
        };
        let msg = e.to_string();
        assert!(msg.contains("sidecar error"));
        // Should NOT contain a trailing ": " when message is empty.
        assert!(!msg.ends_with(": "));
    }

    #[test]
    fn display_grpc() {
        let e = CliError::Grpc(Box::new(tonic::Status::unavailable("server down")));
        let msg = e.to_string();
        assert!(msg.contains("grpc error"));
    }

    #[test]
    fn display_invalid_argument() {
        let msg = CliError::InvalidArgument("bad value".into()).to_string();
        assert!(msg.contains("invalid argument"));
        assert!(msg.contains("bad value"));
    }

    #[test]
    fn display_io() {
        let e = CliError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"));
        let msg = e.to_string();
        assert!(msg.contains("i/o error"));
    }

    #[test]
    fn display_json() {
        let e: CliError = serde_json::from_str::<serde_json::Value>("{{bad}}")
            .unwrap_err()
            .into();
        let msg = e.to_string();
        assert!(msg.contains("json error"));
    }

    // -- From impl tests --

    #[test]
    fn from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nope");
        let cli_err: CliError = io_err.into();
        matches!(cli_err, CliError::Io(_));
    }

    #[test]
    fn from_serde_json_error() {
        let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let cli_err: CliError = json_err.into();
        matches!(cli_err, CliError::Json(_));
    }

    #[test]
    fn from_tonic_status() {
        let status = tonic::Status::cancelled("cancelled");
        let cli_err: CliError = status.into();
        matches!(cli_err, CliError::Grpc(_));
    }
}
