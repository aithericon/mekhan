"""Aithericon Python SDK — IPC client for the execution platform.

Provides functions to interact with the execution sidecar: set outputs,
log artifacts, report progress, emit structured logs, and record metrics.

Basic usage (auto-imported by the runner template)::

    import aithericon
    aithericon.init()
    aithericon.set_output("result", {"accuracy": 0.95})
    aithericon.shutdown()

Context manager usage::

    from aithericon import ExecutionContext

    with ExecutionContext() as ctx:
        data = ctx.inputs.get("config.json", {})
        ctx.set_output("result", 42)
"""

# Lifecycle
from aithericon._client import init, shutdown, is_connected

# Inputs
from aithericon._inputs import load_inputs, token, Token

# Outputs
from aithericon._outputs import set_output

# Artifacts
from aithericon._artifacts import log_artifact

# Progress
from aithericon._progress import update_progress, define_phases, update_phase

# Logging
from aithericon._logging import log_info, log_warn, log_error, log_debug

# Metrics
from aithericon._metrics import log_metric, log_metrics

# High-level context manager
from aithericon._context import ExecutionContext

__all__ = [
    "init",
    "shutdown",
    "is_connected",
    "load_inputs",
    "token",
    "Token",
    "set_output",
    "log_artifact",
    "update_progress",
    "define_phases",
    "update_phase",
    "log_info",
    "log_warn",
    "log_error",
    "log_debug",
    "log_metric",
    "log_metrics",
    "ExecutionContext",
]
