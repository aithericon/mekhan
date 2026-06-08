use std::collections::HashMap;
use std::path::Path;

use aithericon_executor_ipc::proto;
use aithericon_executor_ipc::ExecutorSidecarClient;

use crate::cli::ArtifactCategoryArg;
use crate::error::CliError;
use crate::output::check_response;

fn to_proto_category(cat: &ArtifactCategoryArg) -> proto::ArtifactCategory {
    match cat {
        ArtifactCategoryArg::Other => proto::ArtifactCategory::Other,
        ArtifactCategoryArg::Model => proto::ArtifactCategory::Model,
        ArtifactCategoryArg::Dataset => proto::ArtifactCategory::Dataset,
        ArtifactCategoryArg::Plot => proto::ArtifactCategory::Plot,
        ArtifactCategoryArg::Log => proto::ArtifactCategory::Log,
        ArtifactCategoryArg::Checkpoint => proto::ArtifactCategory::Checkpoint,
        ArtifactCategoryArg::Config => proto::ArtifactCategory::Config,
        ArtifactCategoryArg::Metric => proto::ArtifactCategory::Metric,
    }
}

pub fn parse_key_value_pairs(pairs: &[String]) -> Result<HashMap<String, String>, CliError> {
    let mut map = HashMap::new();
    for pair in pairs {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| CliError::InvalidArgument(format!("expected KEY=VALUE, got: {pair}")))?;
        map.insert(k.to_string(), v.to_string());
    }
    Ok(map)
}

pub async fn log_artifact(
    client: &mut ExecutorSidecarClient<tonic::transport::Channel>,
    path: String,
    name: Option<String>,
    category: ArtifactCategoryArg,
    mime_type: Option<String>,
    metadata_pairs: Vec<String>,
    extract_metadata: bool,
) -> Result<(), CliError> {
    let metadata = parse_key_value_pairs(&metadata_pairs)?;

    let display_name = name.unwrap_or_else(|| {
        Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });

    let execution_id = std::env::var("AITHERICON_EXECUTION_ID").unwrap_or_default();
    let artifact_id = if execution_id.is_empty() {
        display_name.clone()
    } else {
        format!("{execution_id}/{display_name}")
    };

    let resp = client
        .log_artifact(proto::LogArtifactRequest {
            artifact_id,
            path,
            name: display_name,
            category: to_proto_category(&category).into(),
            mime_type: mime_type.unwrap_or_default(),
            metadata,
            extract_file_metadata: extract_metadata,
            blocking: true,
            storage_config_json: String::new(),
            // CLI logs artifacts the normal way (upload). By-reference
            // registration is the SDK `log_artifact(upload=False)` path.
            no_upload: false,
            file_server_id: String::new(),
            reference_path: String::new(),
        })
        .await?
        .into_inner();

    check_response(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_key_value_pairs tests --

    #[test]
    fn parse_single_pair() {
        let pairs = vec!["key=value".into()];
        let map = parse_key_value_pairs(&pairs).unwrap();
        assert_eq!(map.get("key").unwrap(), "value");
    }

    #[test]
    fn parse_multiple_pairs() {
        let pairs = vec!["a=1".into(), "b=2".into(), "c=3".into()];
        let map = parse_key_value_pairs(&pairs).unwrap();
        assert_eq!(map.len(), 3);
        assert_eq!(map["a"], "1");
        assert_eq!(map["b"], "2");
        assert_eq!(map["c"], "3");
    }

    #[test]
    fn parse_empty_value() {
        let pairs = vec!["key=".into()];
        let map = parse_key_value_pairs(&pairs).unwrap();
        assert_eq!(map["key"], "");
    }

    #[test]
    fn parse_value_containing_equals() {
        let pairs = vec!["key=val=ue=more".into()];
        let map = parse_key_value_pairs(&pairs).unwrap();
        assert_eq!(map["key"], "val=ue=more");
    }

    #[test]
    fn parse_empty_input() {
        let pairs: Vec<String> = vec![];
        let map = parse_key_value_pairs(&pairs).unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn parse_missing_equals_returns_error() {
        let pairs = vec!["noequals".into()];
        let err = parse_key_value_pairs(&pairs).unwrap_err();
        assert!(matches!(err, CliError::InvalidArgument(_)));
        assert!(err.to_string().contains("KEY=VALUE"));
    }

    #[test]
    fn parse_duplicate_keys_last_wins() {
        let pairs = vec!["key=first".into(), "key=second".into()];
        let map = parse_key_value_pairs(&pairs).unwrap();
        assert_eq!(map["key"], "second");
    }

    // -- to_proto_category tests --

    #[test]
    fn category_other() {
        assert_eq!(
            to_proto_category(&ArtifactCategoryArg::Other) as i32,
            proto::ArtifactCategory::Other as i32
        );
    }

    #[test]
    fn category_model() {
        assert_eq!(
            to_proto_category(&ArtifactCategoryArg::Model) as i32,
            proto::ArtifactCategory::Model as i32
        );
    }

    #[test]
    fn category_dataset() {
        assert_eq!(
            to_proto_category(&ArtifactCategoryArg::Dataset) as i32,
            proto::ArtifactCategory::Dataset as i32
        );
    }

    #[test]
    fn category_plot() {
        assert_eq!(
            to_proto_category(&ArtifactCategoryArg::Plot) as i32,
            proto::ArtifactCategory::Plot as i32
        );
    }

    #[test]
    fn category_log() {
        assert_eq!(
            to_proto_category(&ArtifactCategoryArg::Log) as i32,
            proto::ArtifactCategory::Log as i32
        );
    }

    #[test]
    fn category_checkpoint() {
        assert_eq!(
            to_proto_category(&ArtifactCategoryArg::Checkpoint) as i32,
            proto::ArtifactCategory::Checkpoint as i32
        );
    }

    #[test]
    fn category_config() {
        assert_eq!(
            to_proto_category(&ArtifactCategoryArg::Config) as i32,
            proto::ArtifactCategory::Config as i32
        );
    }

    #[test]
    fn category_metric() {
        assert_eq!(
            to_proto_category(&ArtifactCategoryArg::Metric) as i32,
            proto::ArtifactCategory::Metric as i32
        );
    }
}
