"""Set named output values: durable file write + best-effort IPC stream."""

import json
import os

from aithericon._client import get_stub


def set_output(name, value):
    """Set a named output **value** — the by-value result the next node consumes.

    Writes ``{AITHERICON_OUTPUTS_DIR}/{name}.json`` — the durable
    contract the runner template's required-output check reads. If a
    sidecar stub is available, the value is *also* streamed over IPC so
    the executor can surface it in real time. The two paths used to be
    mutually exclusive, which silently failed scripts that called
    ``set_output(...)`` instead of bare-globals (the implicit-sweep
    path) — the required-output check then exited the runner with
    ``missing required output(s): ['name']`` even though the IPC call
    succeeded.

    Outputs are for **values** (scalars, small JSON), not files. The value
    is parked by-value in the workflow token and rides the status update
    over NATS, so the executor **hard-errors** an output that serializes
    beyond the inline limit (~1 MiB; ``EXECUTOR_MAX_OUTPUT_INLINE_BYTES``).
    For a file, declare the node's output port as a **file** and return the
    file's path — the executor uploads it to the object store and passes a
    small ``{"key": …}`` handle that the downstream step materializes
    automatically. For large/binary side-products you want catalogued, use
    :func:`log_artifact` instead.
    """
    outputs_dir = os.environ.get("AITHERICON_OUTPUTS_DIR")
    if outputs_dir:
        os.makedirs(outputs_dir, exist_ok=True)
        path = os.path.join(outputs_dir, f"{name}.json")
        with open(path, "w") as f:
            json.dump(value, f)

    stub = get_stub()
    if stub:
        from aithericon._proto import executor_sidecar_pb2

        stub.SetOutput(
            executor_sidecar_pb2.SetOutputRequest(
                name=name, value_json=json.dumps(value)
            )
        )
