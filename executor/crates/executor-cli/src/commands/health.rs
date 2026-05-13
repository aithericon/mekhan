use aithericon_executor_ipc::proto;
use aithericon_executor_ipc::ExecutorSidecarClient;

use crate::error::CliError;
use crate::output::check_response;

pub async fn health_check(
    client: &mut ExecutorSidecarClient<tonic::transport::Channel>,
) -> Result<(), CliError> {
    let resp = client
        .health_check(proto::HealthCheckRequest { sequence: 1 })
        .await?
        .into_inner();

    check_response(resp)
}
