"""Load staged input files."""

import json
import os


def load_inputs(inputs_dir=None):
    """Load staged input files as a dict.

    Tries JSON parse for each file, falls back to raw string content.
    Returns an empty dict if the inputs directory doesn't exist.
    """
    inputs_dir = inputs_dir or os.environ.get("AITHERICON_INPUTS_DIR")
    if not inputs_dir or not os.path.isdir(inputs_dir):
        return {}
    result = {}
    for entry in os.listdir(inputs_dir):
        path = os.path.join(inputs_dir, entry)
        if not os.path.isfile(path):
            continue
        try:
            with open(path, encoding="utf-8") as f:
                content = f.read()
        except (UnicodeDecodeError, OSError):
            # Binary reference file (e.g. *.npy). Leave on disk; user code
            # can open it by path under AITHERICON_INPUTS_DIR.
            continue
        try:
            result[entry] = json.loads(content)
        except (json.JSONDecodeError, ValueError):
            result[entry] = content
    return result
