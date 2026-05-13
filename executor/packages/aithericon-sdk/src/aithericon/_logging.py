"""Structured logging via IPC."""

from aithericon._client import get_stub


def _log(level, message, **fields):
    stub = get_stub()
    if stub:
        from aithericon._proto import executor_sidecar_pb2

        stub.LogMessage(
            executor_sidecar_pb2.LogMessageRequest(
                level=level,
                message=message,
                fields={k: str(v) for k, v in fields.items()},
            )
        )


def log_info(message, **fields):
    """Log an info-level message."""
    from aithericon._proto import executor_sidecar_pb2

    _log(executor_sidecar_pb2.LOG_LEVEL_INFO, message, **fields)


def log_warn(message, **fields):
    """Log a warn-level message."""
    from aithericon._proto import executor_sidecar_pb2

    _log(executor_sidecar_pb2.LOG_LEVEL_WARN, message, **fields)


def log_error(message, **fields):
    """Log an error-level message."""
    from aithericon._proto import executor_sidecar_pb2

    _log(executor_sidecar_pb2.LOG_LEVEL_ERROR, message, **fields)


def log_debug(message, **fields):
    """Log a debug-level message."""
    from aithericon._proto import executor_sidecar_pb2

    _log(executor_sidecar_pb2.LOG_LEVEL_DEBUG, message, **fields)
