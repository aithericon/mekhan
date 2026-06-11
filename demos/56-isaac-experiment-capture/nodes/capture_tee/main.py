"""Stream tee — persist the `recording` data channel as a catalogue artifact.

The `recorder` step streams every captured ROS message as one NDJSON line
`{t_ms, topic, msg}` onto the `recording` channel (out-of-band JetStream
datastream transport). This consumer drains it with the ordinary
transport-unaware `for chunk in stream('recording')` API, writes the full byte
stream to `recording.ndjson`, counts messages per topic, and registers the
file via `log_artifact` with the EXPERIMENT METADATA from the Start form —
turning an ephemeral stream into a durable, content-addressed, provenance-
linked catalogue entry that a containment query
(`GET /api/v1/catalogue?metadata={"experiment_name": …}`) finds across runs.

COMPILER CONTRACT: this source is SCANNED (not executed) for `<slug>.<field>`
references. The literal `start.experiment_name` / `start.trial_id` reads below
MUST stay verbatim so the compiler stages the Start envelope as a Python
global. Outputs are implicit: the runner sweeps globals matching this step's
declared output port (received_messages / recorded_bytes / artifact_logged).

An EMPTY recording is legitimate (the fake xarm backend never publishes the
Isaac seam topics) — the artifact is still logged so the metadata trail shows
the trial ran; empty Start fields (a blank UI form) fall back to defaults
rather than failing the run.
"""

import json
import os

import aithericon
from aithericon import log_artifact, set_output

# Experiment identity from the Start form — staged by the compiler as the
# parked `start` envelope. Blank-form tolerant: "" falls back to defaults.
experiment_name = (start.experiment_name or "").strip() or "isaac-capture"
trial_id = (start.trial_id or "").strip() or "t1"

path = os.path.abspath("recording.ndjson")
buf = b""
received = 0
malformed = 0
recorded_bytes = 0
per_topic = {}

with open(path, "wb") as f:
    for chunk in aithericon.stream("recording"):
        if not isinstance(chunk, (bytes, bytearray)):
            continue
        f.write(chunk)
        recorded_bytes += len(chunk)
        buf += chunk
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
                topic = rec.get("topic", "?") if isinstance(rec, dict) else "?"
                per_topic[topic] = per_topic.get(topic, 0) + 1
                received += 1
            except ValueError:
                malformed += 1

# Flat string key-values — the catalogue stores these in the GIN-indexed
# user_metadata JSONB, so containment queries find captures by any key.
metadata = {
    "experiment_name": experiment_name,
    "trial_id": trial_id,
    "capture": "ros-record",
    "topics": ",".join(sorted(per_topic)) or "(none)",
    "messages": str(received),
}

# blocking=True: the file is a run-dir temp — make sure the upload completed
# before the step (and its cleanup) finishes.
log_artifact(
    path,
    name=f"{experiment_name}-{trial_id}-recording.ndjson",
    category="dataset",
    mime_type="application/x-ndjson",
    metadata=metadata,
    blocking=True,
)

aithericon.log_info(
    f"capture_tee: persisted {received} messages ({recorded_bytes} bytes, "
    f"per-topic {per_topic}, malformed {malformed}) as "
    f"{experiment_name}-{trial_id}-recording.ndjson"
)

set_output("received_messages", received)
set_output("recorded_bytes", recorded_bytes)
set_output("artifact_logged", True)
