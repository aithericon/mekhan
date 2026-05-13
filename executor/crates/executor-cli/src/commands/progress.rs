use aithericon_executor_ipc::proto;
use aithericon_executor_ipc::ExecutorSidecarClient;

use crate::error::CliError;
use crate::output::check_response;

pub async fn update_progress(
    client: &mut ExecutorSidecarClient<tonic::transport::Channel>,
    fraction: f32,
    message: Option<String>,
    step: Option<u64>,
    total_steps: Option<u64>,
) -> Result<(), CliError> {
    if !(0.0..=1.0).contains(&fraction) {
        return Err(CliError::InvalidArgument(format!(
            "fraction must be 0.0–1.0, got {fraction}"
        )));
    }

    let resp = client
        .update_progress(proto::UpdateProgressRequest {
            fraction,
            message: message.unwrap_or_default(),
            current_step: step.unwrap_or(0),
            total_steps: total_steps.unwrap_or(0),
        })
        .await?
        .into_inner();

    check_response(resp)
}
