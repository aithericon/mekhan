"""Consumer: drain the `frames` data channel with the ORDINARY stream() API.

This code is identical to what it would be over JetStream — it never names a
transport. The consumer's executor lifts `transport` from the producer's `open`
descriptor and dispatches the matching (lossy core-NATS) subscribe adapter. That
indifference is the whole point: one consumer contract, any transport.

`received` may be LESS than the producer's `sent` — lossy-latest drops anything
published before this job subscribed. We assert only that the stream delivered
(received >= 1), which proves the dispatched adapter actually carried bytes.
"""

import aithericon

received = 0
total_bytes = 0

for frame in aithericon.stream("frames"):
    received += 1
    if isinstance(frame, (bytes, bytearray)):
        total_bytes += len(frame)

aithericon.set_output("received", received)
aithericon.set_output("total_bytes", total_bytes)
