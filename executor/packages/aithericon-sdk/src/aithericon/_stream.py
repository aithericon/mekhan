"""Read a data-plane byte stream: ``for elem in aithericon.stream(name)``.

This is the consumer side of the data plane (docs/25). A producer elsewhere
called ``open_output(name)`` and is writing out-of-band bytes over a transport
subject; the engine delivered that producer's ``open`` DESCRIPTOR into THIS
(consumer) job's input the moment the producer opened — EARLY, independent of the
producer finishing. This generator drains the producer's byte stream and yields
one decoded element per binary envelope until end-of-stream::

    import aithericon

    total = 0
    for frame in aithericon.stream("frames"):
        total += score(frame)            # frame is bytes (Binary) or dict (Json)
    aithericon.set_output("total", total)

How each element decodes is fixed by the channel's declared ``element_kind``
(baked into the manifest the compiler emits): ``binary`` → raw ``bytes``;
``json`` / ``any`` → ``json.loads`` of the payload. The generator returns when
the in-band EOF sentinel arrives (or the stream otherwise ends).

The SDK holds no NATS/transport credentials: it lifts the PRODUCER's transport
``subject`` out of the ``open`` descriptor (delivered as this job's input) and
asks the executor to drain it via the ``StreamChunks`` server-stream (the
executor owns the transport connection + any unwrapped credential, and relays
each binary envelope back). Silent no-op (empty iterator) when not connected to a
sidecar (no ``AITHERICON_IPC_SOCKET``) or when no producer descriptor is present
(the producer hasn't opened, or this script runs outside an execution context).

**Sync over async core.** The public :func:`stream` is a *sync* generator, but it
is implemented by stepping a private *async* generator (:func:`_astream`) one
element at a time on a dedicated event loop. The async core is the real reader;
the sync face just pumps it. That keeps ``async for`` / ``select()`` / multi-input
muxing additive surface later (expose ``_astream`` directly) rather than a rewrite.
"""

import asyncio
import json

from aithericon._channels import element_kind_for, resolve_data_channel
from aithericon._client import get_stub
from aithericon._inputs import token


def _coords_from_input(name):
    """Lift the producer's transport COORDINATES from this consumer's input.

    The engine deposits the producer's ``open`` control token —
    ``{channel, kind:"open", payload:<descriptor>}`` — into the data channel's
    place, which flows into this consumer job as input. The descriptor payload is
    ``{transport, subject, content_type?, credential?}`` (docs/25 §6). We scan the
    workflow token for the matching ``open`` token and return its
    ``(subject, transport)``: the PRODUCER's datastream subject
    (``executor.datastream.{producer_execution_id}.{channel}``) the executor must
    subscribe to, and the transport tag (``jetstream`` | ``nats-latest``) the
    executor dispatches its subscribe adapter off — so the consumer reads the
    stream the way the producer wrote it. Returns ``None`` when no matching
    descriptor is present (offline, or the producer has not opened yet), in which
    case the read is a silent no-op.
    """
    tok = token()
    if not isinstance(tok, dict):
        return None

    def _descriptor(value):
        if (
            isinstance(value, dict)
            and value.get("kind") == "open"
            and value.get("channel") == name
        ):
            return value.get("descriptor")
        return None

    # The open token may sit at the top level or namespaced under a field;
    # check the token itself, then one level of its values.
    descriptor = _descriptor(tok)
    if descriptor is None:
        for value in tok.values():
            descriptor = _descriptor(value)
            if descriptor is not None:
                break
    if not isinstance(descriptor, dict):
        return None
    subject = descriptor.get("subject")
    if not (isinstance(subject, str) and subject):
        return None
    # Transport tag the producer stamped; absent/blank → executor defaults to
    # jetstream (older descriptors that predate the field).
    transport = descriptor.get("transport")
    transport = transport if isinstance(transport, str) else ""
    return subject, transport


def _decode(envelope, element_kind):
    """Decode one binary envelope's payload per the channel's element kind.

    ``binary`` → raw ``bytes`` (the opaque blob, verbatim). ``json`` / ``any`` →
    ``json.loads`` of the UTF-8 payload. A malformed JSON payload under ``any``
    falls back to the raw bytes rather than raising (``any`` is the untyped
    escape hatch); under ``json`` the decode error surfaces.
    """
    payload = envelope.payload
    if element_kind == "binary":
        return payload
    text = payload.decode("utf-8")
    if element_kind == "any":
        try:
            return json.loads(text)
        except (json.JSONDecodeError, ValueError):
            return payload
    return json.loads(text)


async def _astream(name):
    """Async core: yield each decoded element of the producer's byte stream.

    The real reader. Lifts the producer's transport subject from the ``open``
    descriptor in this job's input, opens the ``StreamChunks`` server-stream
    passing that subject (so the executor subscribes to the *producer's* stream),
    and yields one decoded element per envelope until the in-band EOF sentinel.
    Silent no-op (yields nothing) when not connected or when no producer
    descriptor is present. Exposed for a future async/select surface; the sync
    :func:`stream` pumps it today.
    """
    import grpc

    stub = get_stub()
    if stub is None:
        return

    # Resolve up front: validates the name is a declared data channel and gives
    # us the decode kind.
    resolve_data_channel(name)
    element_kind = element_kind_for(name)

    # The producer's coordinates are authoritative — without them the executor
    # has nothing to subscribe to (the consumer's own execution id is NOT the
    # producer's). No descriptor → no producer opened yet → empty read.
    coords = _coords_from_input(name)
    if not coords:
        return
    subject, transport = coords

    from aithericon._proto import executor_sidecar_pb2

    try:
        stream = stub.StreamChunks(
            executor_sidecar_pb2.StreamChunksRequest(
                channel=name, subject=subject, transport=transport
            )
        )
        for envelope in stream:
            if envelope.is_eof:
                return
            yield _decode(envelope, element_kind)
    except grpc.RpcError:
        # Sidecar gone or stream aborted — end the loop cleanly rather than
        # surfacing a transport error into user consumer code.
        return


def stream(name):
    """Yield each element of the data channel ``name`` until end-of-stream.

    A *sync* generator. ``name`` is a ``data``-plane channel declared in this
    step's node config (wired off an upstream producer's ``open_output`` handle).
    Each yielded value is the decoded element: raw ``bytes`` for a ``Binary``
    channel, a ``dict``/value for ``Json``/``Any``. Silent no-op (empty iterator)
    when not connected to a sidecar.

    Raises :class:`ValueError` when the channel manifest is exposed
    (``AITHERICON_CHANNELS``) and ``name`` is unknown or is a control-plane
    channel.

    Sync over an async core: a dedicated event loop drives :func:`_astream` one
    element at a time, so the blocking ``for`` face ships now while ``async for``
    / multi-input ``select`` stay additive (read ``_astream`` directly) later.
    """
    # Validate eagerly so an undeclared/control channel raises at the call site,
    # before any iteration begins (parity with out()/open_output()).
    resolve_data_channel(name)

    if get_stub() is None:
        return

    loop = asyncio.new_event_loop()
    agen = _astream(name)
    try:
        while True:
            try:
                yield loop.run_until_complete(agen.__anext__())
            except StopAsyncIteration:
                return
    finally:
        try:
            loop.run_until_complete(agen.aclose())
        except Exception:
            pass
        loop.close()
