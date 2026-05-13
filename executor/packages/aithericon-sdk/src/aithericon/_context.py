"""High-level ExecutionContext manager."""

from aithericon._client import init, shutdown
from aithericon._inputs import load_inputs
from aithericon._outputs import set_output
from aithericon._artifacts import log_artifact
from aithericon._progress import update_progress, define_phases, update_phase
from aithericon._logging import log_info, log_warn, log_error, log_debug
from aithericon._metrics import log_metric, log_metrics


class ExecutionContext:
    """Context manager that auto-connects to the IPC sidecar.

    Usage::

        with ExecutionContext() as ctx:
            data = ctx.inputs.get("config.json", {})
            ctx.set_output("result", 42)
            ctx.log_info("done")
    """

    def __init__(self, socket_path=None):
        self._socket_path = socket_path
        self.inputs = {}

    def __enter__(self):
        init(self._socket_path)
        self.inputs = load_inputs()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        exit_code = 0 if exc_type is None else 1
        shutdown(exit_code)
        return False

    set_output = staticmethod(set_output)
    log_artifact = staticmethod(log_artifact)
    update_progress = staticmethod(update_progress)
    define_phases = staticmethod(define_phases)
    update_phase = staticmethod(update_phase)
    log_info = staticmethod(log_info)
    log_warn = staticmethod(log_warn)
    log_error = staticmethod(log_error)
    log_debug = staticmethod(log_debug)
    log_metric = staticmethod(log_metric)
    log_metrics = staticmethod(log_metrics)
