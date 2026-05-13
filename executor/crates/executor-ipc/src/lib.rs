pub mod proto {
    tonic::include_proto!("aithericon.executor.ipc");
}

pub use proto::executor_sidecar_client::ExecutorSidecarClient;
pub use proto::executor_sidecar_server::{ExecutorSidecar, ExecutorSidecarServer};
pub use proto::*;
