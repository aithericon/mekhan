"""Producer: write 8 durable 8 KiB binary blocks over the `s3` transport.

This code is byte-for-byte the ordinary data-channel producer API — it never
names a transport. The channel `blocks` declares `transport: s3` in the graph;
the compiler bakes that into the manifest, the SDK stamps it into the `open`
descriptor, and the producer's executor dispatches the object-store PUT adapter
off it. Each `write` becomes one durable object; `close` stamps the count.

We use 8 KiB blocks (64 KiB total) to make the "load-bearing for large blobs"
point concrete: the bytes ride out-of-band object storage, never the petri net.
"""

import aithericon

BLOCK_SIZE = 8 * 1024  # 8 KiB per block
NUM_BLOCKS = 8

bytes_sent = 0
with aithericon.open_output("blocks") as out:
    for i in range(NUM_BLOCKS):
        block = bytes([i & 0xFF]) * BLOCK_SIZE
        out.write(block, content_type="application/octet-stream")
        bytes_sent += len(block)

aithericon.set_output("sent", NUM_BLOCKS)
aithericon.set_output("bytes_sent", bytes_sent)
