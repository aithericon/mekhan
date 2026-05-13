use aithericon_executor_ipc::proto;
use aithericon_executor_ipc::ExecutorSidecarClient;

use crate::error::CliError;
use crate::output::check_response;

pub async fn shutdown(
    client: &mut ExecutorSidecarClient<tonic::transport::Channel>,
    exit_code: i32,
) -> Result<(), CliError> {
    let resp = client
        .shutdown_ack(proto::ShutdownAckRequest { exit_code })
        .await?
        .into_inner();

    check_response(resp)
}
