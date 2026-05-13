use std::collections::HashMap;

use aithericon_executor_ipc::proto;
use aithericon_executor_ipc::ExecutorSidecarClient;

use crate::cli::LogLevelArg;
use crate::commands::artifact::parse_key_value_pairs;
use crate::error::CliError;
use crate::output::check_response;

fn to_proto_level(level: &LogLevelArg) -> proto::LogLevel {
    match level {
        LogLevelArg::Info => proto::LogLevel::Info,
        LogLevelArg::Trace => proto::LogLevel::Trace,
        LogLevelArg::Debug => proto::LogLevel::Debug,
        LogLevelArg::Warn => proto::LogLevel::Warn,
        LogLevelArg::Error => proto::LogLevel::Error,
    }
}

pub async fn log_message(
    client: &mut ExecutorSidecarClient<tonic::transport::Channel>,
    level: LogLevelArg,
    message: String,
    field_pairs: Vec<String>,
) -> Result<(), CliError> {
    let fields: HashMap<String, String> = if field_pairs.is_empty() {
        HashMap::new()
    } else {
        parse_key_value_pairs(&field_pairs)?
    };

    let resp = client
        .log_message(proto::LogMessageRequest {
            level: to_proto_level(&level).into(),
            message,
            fields,
        })
        .await?
        .into_inner();

    check_response(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_info() {
        assert_eq!(
            to_proto_level(&LogLevelArg::Info) as i32,
            proto::LogLevel::Info as i32
        );
    }

    #[test]
    fn level_trace() {
        assert_eq!(
            to_proto_level(&LogLevelArg::Trace) as i32,
            proto::LogLevel::Trace as i32
        );
    }

    #[test]
    fn level_debug() {
        assert_eq!(
            to_proto_level(&LogLevelArg::Debug) as i32,
            proto::LogLevel::Debug as i32
        );
    }

    #[test]
    fn level_warn() {
        assert_eq!(
            to_proto_level(&LogLevelArg::Warn) as i32,
            proto::LogLevel::Warn as i32
        );
    }

    #[test]
    fn level_error() {
        assert_eq!(
            to_proto_level(&LogLevelArg::Error) as i32,
            proto::LogLevel::Error as i32
        );
    }
}
