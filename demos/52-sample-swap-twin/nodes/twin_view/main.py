"""Consumer for the `scene` data channel — the net-side anchor the live
planning-scene twin renders ON.

The twin in the UI taps this SAME channel out-of-band (via `?follow=1`) and
animates the REAL move_group planning scene (arm + world collision objects +
the grasped sample riding the gripper); this node is the data-plane consumer
that gives the channel a `producer → consumer` edge to render on (mirrors
demo 49's twin_view). It drains the NDJSON planning-scene frames with the
ordinary transport-unaware `for chunk in stream('scene')` API, reframes on
newlines, validates each frame parses as a planning-scene object, and reports
the count.

The frames carry the REAL move_group planning scene streamed live from the
lift action — NOT a re-computed scene; the twin reflects move_group's true
world + attached collision objects and the arm's actual joint states. The
robot model is carried in the channel content_type as `;model=xarm6` so the
twin renderer knows which URDF (`robot_description` asset, ref_key xarm6) to
load. The bulk stream rides the out-of-band JetStream datastream transport —
the net sees only open + close.
"""

import json

import aithericon
from aithericon import set_output

buf = b""
received = 0
malformed = 0
for chunk in aithericon.stream("scene"):
    if not isinstance(chunk, (bytes, bytearray)):
        continue
    buf += chunk
    while b"\n" in buf:
        line, buf = buf.split(b"\n", 1)
        line = line.strip()
        if not line:
            continue
        try:
            frame = json.loads(line)
            if isinstance(frame, dict):
                received += 1
            else:
                malformed += 1
        except (ValueError, AttributeError):
            malformed += 1

set_output("received_frames", received)
set_output("malformed_frames", malformed)
