"""gRPC channel singleton for IPC sidecar communication."""

import os

import grpc

from aithericon._proto import executor_sidecar_pb2_grpc

_channel = None
_stub = None


def init(socket_path=None):
    """Connect to IPC sidecar.

    Auto-discovers from AITHERICON_IPC_SOCKET env var if no path given.
    If no socket path is available, SDK functions become silent no-ops.
    """
    global _channel, _stub
    socket_path = socket_path or os.environ.get("AITHERICON_IPC_SOCKET")
    if not socket_path:
        return
    # Set grpc.default_authority to "localhost" so that the :authority
    # HTTP/2 pseudo-header is valid. Without this, Python grpcio sends
    # the socket path as the authority, which the h2 crate (used by the
    # Rust sidecar via hyper/tonic) rejects with RST_STREAM PROTOCOL_ERROR.
    # See: https://github.com/hyperium/h2/pull/487
    _channel = grpc.insecure_channel(
        f"unix://{socket_path}",
        options=[("grpc.default_authority", "localhost")],
    )
    _stub = executor_sidecar_pb2_grpc.ExecutorSidecarStub(_channel)


def shutdown(exit_code=0):
    """Send ShutdownAck to the sidecar and close the channel."""
    from aithericon._proto import executor_sidecar_pb2

    if _stub:
        try:
            _stub.ShutdownAck(
                executor_sidecar_pb2.ShutdownAckRequest(exit_code=exit_code)
            )
        except grpc.RpcError:
            pass  # Sidecar may already be gone
    if _channel:
        _channel.close()


def get_stub():
    """Return the active gRPC stub, or None if not connected."""
    return _stub


def is_connected():
    """Return True if connected to the IPC sidecar."""
    return _stub is not None
