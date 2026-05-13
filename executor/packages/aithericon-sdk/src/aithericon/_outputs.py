"""Set named output values via IPC or file-based fallback."""

import json
import os

from aithericon._client import get_stub


def set_output(name, value):
    """Set a named output value.

    Uses IPC if connected to the sidecar, otherwise writes a JSON file
    to the outputs directory as a fallback.
    """
    stub = get_stub()
    if stub:
        from aithericon._proto import executor_sidecar_pb2

        stub.SetOutput(
            executor_sidecar_pb2.SetOutputRequest(
                name=name, value_json=json.dumps(value)
            )
        )
    else:
        outputs_dir = os.environ.get("AITHERICON_OUTPUTS_DIR")
        if outputs_dir:
            os.makedirs(outputs_dir, exist_ok=True)
            path = os.path.join(outputs_dir, f"{name}.json")
            with open(path, "w") as f:
                json.dump(value, f)
