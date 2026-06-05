"""Consumer: drain the `blocks` data channel with the ORDINARY stream() API.

Identical to what it would be over JetStream or nats-latest — it never names a
transport. The consumer's executor lifts `transport: s3` from the producer's
`open` descriptor and dispatches the matching object-store subscribe adapter,
which polls the chunk objects in `seq` order until the EOF object.

Because S3 is durable + ordered + replayable (NOT lossy like nats-latest), the
consumer is guaranteed every block from `c0` no matter when it starts — so
`received` MUST equal the producer's `sent` and `total_bytes` its `bytes_sent`.
That equality is the durable-transport contract, asserted in the test.
"""

import aithericon

received = 0
total_bytes = 0

for block in aithericon.stream("blocks"):
    received += 1
    if isinstance(block, (bytes, bytearray)):
        total_bytes += len(block)

aithericon.set_output("received", received)
aithericon.set_output("total_bytes", total_bytes)
