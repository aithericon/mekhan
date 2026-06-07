"""Consumer for the `joints` data channel — the net-side anchor the live 3D URDF
twin renders ON.

The 3D twin in the UI taps this SAME channel out-of-band (via `?follow=1`) and
animates the xArm 6; this node is the data-plane consumer that gives the channel
a `producer → consumer` edge to render on (mirrors demo 46's validator). It drains
the NDJSON joint-state frames with the ordinary transport-unaware
`for chunk in stream('joints')` API, reframes on newlines, validates each frame
carries 6 joint positions, and reports the count.
"""

import json

import aithericon
from aithericon import set_output

buf = b""
received = 0
malformed = 0
for chunk in aithericon.stream("joints"):
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
            if len(frame.get("positions", [])) == 6:
                received += 1
            else:
                malformed += 1
        except (ValueError, AttributeError):
            malformed += 1

set_output("received_frames", received)
set_output("malformed_frames", malformed)
