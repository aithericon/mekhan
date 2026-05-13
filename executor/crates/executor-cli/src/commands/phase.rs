use aithericon_executor_ipc::proto;
use aithericon_executor_ipc::ExecutorSidecarClient;

use crate::cli::PhaseStatusArg;
use crate::error::CliError;
use crate::output::check_response;

fn to_proto_status(s: &PhaseStatusArg) -> proto::PhaseStatus {
    match s {
        PhaseStatusArg::Pending => proto::PhaseStatus::Pending,
        PhaseStatusArg::Running => proto::PhaseStatus::Running,
        PhaseStatusArg::Completed => proto::PhaseStatus::Completed,
        PhaseStatusArg::Failed => proto::PhaseStatus::Failed,
        PhaseStatusArg::Skipped => proto::PhaseStatus::Skipped,
    }
}

pub async fn define_phases(
    client: &mut ExecutorSidecarClient<tonic::transport::Channel>,
    names: Vec<String>,
) -> Result<(), CliError> {
    let resp = client
        .define_phases(proto::DefinePhasesRequest { phase_names: names })
        .await?
        .into_inner();

    check_response(resp)
}

pub async fn update_phase(
    client: &mut ExecutorSidecarClient<tonic::transport::Channel>,
    name: String,
    status: PhaseStatusArg,
    message: Option<String>,
) -> Result<(), CliError> {
    let resp = client
        .update_phase(proto::UpdatePhaseRequest {
            phase_name: name,
            status: to_proto_status(&status).into(),
            message: message.unwrap_or_default(),
        })
        .await?
        .into_inner();

    check_response(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_pending() {
        assert_eq!(
            to_proto_status(&PhaseStatusArg::Pending) as i32,
            proto::PhaseStatus::Pending as i32
        );
    }

    #[test]
    fn status_running() {
        assert_eq!(
            to_proto_status(&PhaseStatusArg::Running) as i32,
            proto::PhaseStatus::Running as i32
        );
    }

    #[test]
    fn status_completed() {
        assert_eq!(
            to_proto_status(&PhaseStatusArg::Completed) as i32,
            proto::PhaseStatus::Completed as i32
        );
    }

    #[test]
    fn status_failed() {
        assert_eq!(
            to_proto_status(&PhaseStatusArg::Failed) as i32,
            proto::PhaseStatus::Failed as i32
        );
    }

    #[test]
    fn status_skipped() {
        assert_eq!(
            to_proto_status(&PhaseStatusArg::Skipped) as i32,
            proto::PhaseStatus::Skipped as i32
        );
    }
}
