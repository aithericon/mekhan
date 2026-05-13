"""Progress and phase tracking via IPC."""

from aithericon._client import get_stub

_PHASE_STATUS_MAP = None


def _get_phase_status_map():
    global _PHASE_STATUS_MAP
    if _PHASE_STATUS_MAP is None:
        from aithericon._proto import executor_sidecar_pb2

        _PHASE_STATUS_MAP = {
            "pending": executor_sidecar_pb2.PHASE_STATUS_PENDING,
            "running": executor_sidecar_pb2.PHASE_STATUS_RUNNING,
            "completed": executor_sidecar_pb2.PHASE_STATUS_COMPLETED,
            "failed": executor_sidecar_pb2.PHASE_STATUS_FAILED,
            "skipped": executor_sidecar_pb2.PHASE_STATUS_SKIPPED,
        }
    return _PHASE_STATUS_MAP


def update_progress(fraction, message=None, current_step=0, total_steps=0):
    """Report execution progress.

    Args:
        fraction: Progress fraction between 0.0 and 1.0.
        message: Optional human-readable progress message.
        current_step: Current step number.
        total_steps: Total number of steps.
    """
    stub = get_stub()
    if stub:
        from aithericon._proto import executor_sidecar_pb2

        stub.UpdateProgress(
            executor_sidecar_pb2.UpdateProgressRequest(
                fraction=fraction,
                message=message or "",
                current_step=current_step,
                total_steps=total_steps,
            )
        )


def define_phases(phase_names):
    """Define the execution phases upfront.

    Args:
        phase_names: List of phase name strings.
    """
    stub = get_stub()
    if stub:
        from aithericon._proto import executor_sidecar_pb2

        stub.DefinePhases(
            executor_sidecar_pb2.DefinePhasesRequest(phase_names=phase_names)
        )


def update_phase(phase_name, status, message=None):
    """Update the status of a named phase.

    Args:
        phase_name: Name of the phase (must match one from define_phases).
        status: One of "pending", "running", "completed", "failed", "skipped".
        message: Optional status message.
    """
    stub = get_stub()
    if stub:
        from aithericon._proto import executor_sidecar_pb2

        status_map = _get_phase_status_map()
        stub.UpdatePhase(
            executor_sidecar_pb2.UpdatePhaseRequest(
                phase_name=phase_name,
                status=status_map.get(status, executor_sidecar_pb2.PHASE_STATUS_PENDING),
                message=message or "",
            )
        )
