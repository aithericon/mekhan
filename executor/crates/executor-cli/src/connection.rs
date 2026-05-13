use std::io;

use tokio::net::UnixStream;
use tonic::transport::{Endpoint, Uri};
use tower::service_fn;

use aithericon_executor_ipc::ExecutorSidecarClient;

use crate::error::CliError;

/// Connect to the IPC sidecar over a Unix domain socket.
pub async fn connect(
    socket_path: &str,
) -> Result<ExecutorSidecarClient<tonic::transport::Channel>, CliError> {
    let path = socket_path.to_string();
    let channel = Endpoint::try_from("http://[::]:50051")
        .map_err(|e| CliError::Connection(e.to_string()))?
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = path.clone();
            async move {
                let stream = UnixStream::connect(path).await?;
                Ok::<_, io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await
        .map_err(|e| CliError::Connection(e.to_string()))?;
    Ok(ExecutorSidecarClient::new(channel))
}
