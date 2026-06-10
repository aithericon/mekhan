# Shout — workflow-as-streaming-endpoint echo step (docs/25 §9 Phase 3).
#
# This step is the in-net half of a streaming ENDPOINT: the bytes it drains
# were never produced by another job — an external client POSTed them to the
# mekhan ingress endpoint, which framed them into binary envelopes on the
# StreamSource's 'feed' data channel and published the open/close brackets
# into the node's control inbox. From this side the API is byte-for-byte the
# ordinary consumer/producer pair (demo 14): `stream("feed")` starts draining
# as soon as the open descriptor token triggers the job, and `open_output`
# re-emits on the 'shouted' channel the downstream StreamSink terminates at
# the egress endpoint.
#
# The transform is deliberately trivial (uppercase each chunk) so the demo
# proves the PLUMBING: external bytes in → net-mediated job → external bytes
# out, with the bulk payload riding out-of-band JetStream subjects on both
# legs, never the petri-net marking.

from aithericon import open_output, set_output, stream

chunks = 0
with open_output("shouted") as out:
    for chunk in stream("feed"):
        data = chunk if isinstance(chunk, (bytes, bytearray)) else str(chunk).encode()
        out.write(bytes(data).upper(), content_type="text/plain")
        chunks += 1

set_output("chunks", chunks)
