"""Producer: pace small binary frames over a LOSSY-LATEST data channel.

The channel `frames` declares `transport: nats-latest` in the graph, so its
bytes ride plain core NATS (no JetStream, no replay, no back-pressure) instead of
the durable default. Nothing here is transport-aware — this is the exact same
`open_output(...).write(...)` API every data demo uses. The transport choice
lives in the manifest the compiler baked; the SDK stamps it into the `open`
descriptor; the executor dispatches the lossy publish adapter off it.

Because core NATS has no replay, a consumer only receives frames published AFTER
it subscribed. We emit `open` (on context entry), wait a brief lead-in so the
downstream consumer can connect, THEN start writing — so most frames land while
the consumer is listening. A slow consumer still legitimately misses some: that
is the lossy-latest contract.
"""

import time

import aithericon

FRAMES = 20
LEAD_IN_S = 0.5  # after `open`, before the first write — lets the consumer connect
PACE_S = 0.15  # ~3 s of streaming, a wide window for the consumer to catch up
FRAME_BYTES = 256

with aithericon.open_output("frames") as out:
    # `open` has fired; give the consumer time to receive it and subscribe.
    time.sleep(LEAD_IN_S)
    for i in range(FRAMES):
        # A distinct, fixed-size binary frame per write.
        out.write(bytes([i & 0xFF]) * FRAME_BYTES, content_type="application/octet-stream")
        time.sleep(PACE_S)

aithericon.set_output("sent", FRAMES)
