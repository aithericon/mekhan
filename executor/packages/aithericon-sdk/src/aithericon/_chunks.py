"""Live inbound chunk feed: ``for chunk in aithericon.chunks()``.

This is the Python side of the "live IPC reducer" capability. A reducer job
opts in (the executor sets ``feed_chunks`` on the job), and the engine feeds
data chunks INTO the still-running job over the ``EXECUTOR_CHUNKS`` NATS feed.
The executor's IPC sidecar surfaces them on the ``StreamChunks`` server-stream;
this generator drains that stream so the author can write the idiomatic::

    import aithericon

    acc = 0
    for chunk in aithericon.chunks():
        acc += chunk
    aithericon.set_output("result", acc)

Each yielded value is the JSON-decoded chunk value. The generator returns when
the in-band EOF sentinel is received (or the stream otherwise ends). If the SDK
is not connected to a sidecar (no ``AITHERICON_IPC_SOCKET``), or the job did not
opt into the feed, this is a silent no-op — it yields nothing and returns
immediately, so the same script is safe to run outside a reducer context.
"""

import json

import grpc

from aithericon._client import get_stub


def chunks():
    """Yield each inbound chunk value (JSON-decoded) until end-of-stream.

    A generator. Silent no-op (empty iterator) when not connected to a sidecar
    or when the job did not opt into the inbound feed.
    """
    stub = get_stub()
    if stub is None:
        return

    from aithericon._proto import executor_sidecar_pb2

    try:
        stream = stub.StreamChunks(executor_sidecar_pb2.StreamChunksRequest())
        for msg in stream:
            if msg.is_eof:
                return
            yield json.loads(msg.value_json)
    except grpc.RpcError:
        # Sidecar gone or stream aborted — end the loop cleanly rather than
        # surfacing a transport error into user reducer code.
        return
