"""Write a data-plane byte stream: ``with aithericon.open_output(name) as out:``.

This is the producer side of the data plane (docs/25). A data channel is
out-of-band bytes **bracketed by two control emissions** — an ``open`` (carrying
the transport DESCRIPTOR) fired the moment ``open_output(name)`` is called, and a
``close`` (stamping the element count + terminal status) fired when the writer
context exits. Between them the bulk bytes flow over the transport subject named
in the descriptor — they NEVER ride a control token, so the petri net sees the
stream's lifecycle only (two firings total, regardless of element count)::

    import aithericon

    with aithericon.open_output("thumbnails") as out:
        for frame in frames:
            out.write(make_jpeg(frame), content_type="image/jpeg")
    # close (count = frames written) fires here, on block exit

The ``open`` is emitted **early** (on ``__enter__``, mid-job) so a consumer wired
off this handle can connect to the transport and start draining while this
producer is still producing — the consumer does not wait for this job to finish.

The SDK holds no NATS/transport credentials: each ``write`` hands the executor
one framed binary envelope over the ``PublishChunk`` RPC and the executor (which
owns the transport connection + any unwrapped credential) publishes it onto the
channel's datastream subject. The publish is unary + awaited, so the transport's
ack window can back-pressure a too-fast producer (docs/25 §5).

Silent no-op when the SDK is not connected to a sidecar (no
``AITHERICON_IPC_SOCKET``) — same contract as :func:`aithericon.emit` /
:func:`aithericon.set_output` — so the same script runs unchanged outside an
execution context. The channel name is validated against the job's channel
manifest exactly as :func:`aithericon.emit` does (early friendly check when
``AITHERICON_CHANNELS`` is exposed; the sidecar enforces authoritatively
otherwise).
"""

import json
import os

from aithericon._channels import resolve_data_channel
from aithericon._client import get_stub

#: The transport tag a v1 descriptor advertises. Only JetStream ships in v1
#: (docs/25 §6); the field exists so an S3 / live adapter is additive.
_TRANSPORT_JETSTREAM = "jetstream"


def _datastream_subject(name):
    """Mint this producer's transport subject for channel ``name``.

    ``executor.datastream.{execution_id}.{channel}`` — the same scheme the
    executor's ``datastream_subject()`` mints (the SDK names it for the
    descriptor; the executor is the source of truth at publish time). Falls back
    to a bare ``executor.datastream..{channel}`` when the execution id is not in
    the environment (offline / no-sidecar), which is never actually published.
    """
    exec_id = os.environ.get("AITHERICON_EXECUTION_ID", "")
    return f"executor.datastream.{exec_id}.{name}"


class _Writer:
    """The byte-stream writer yielded by :func:`open_output`.

    Tracks the running element count so ``__exit__`` can stamp it on the
    ``close`` token. Each :meth:`write` publishes one binary envelope with a
    monotonically-increasing ``seq``; a JSON value is framed as
    ``utf8(json)`` + ``application/json`` while raw ``bytes``/``bytearray`` pass
    through with the caller's ``content_type``.
    """

    def __init__(self, name, content_type):
        self._name = name
        self._default_content_type = content_type
        self._seq = 0

    @property
    def count(self):
        """Number of elements written so far (the eventual close count)."""
        return self._seq

    def write(self, value, content_type=None):
        """Publish one element onto the data channel.

        ``value`` is either raw ``bytes``/``bytearray`` (published verbatim with
        ``content_type``, defaulting to the channel's open content-type or
        ``application/octet-stream``) or a JSON-serializable value (framed as
        ``utf8(json.dumps(value))`` with ``application/json``). The element
        ``seq`` advances on every call so the consumer can re-order/dedup, even
        when not connected (the close count then still reflects intent).
        """
        seq = self._seq
        self._seq += 1

        if isinstance(value, (bytes, bytearray)):
            payload = bytes(value)
            ct = content_type or self._default_content_type or "application/octet-stream"
        else:
            payload = json.dumps(value).encode("utf-8")
            ct = "application/json"

        self._publish(seq=seq, content_type=ct, payload=payload, is_eof=False)

    def _publish(self, seq, content_type, payload, is_eof):
        """Hand one binary envelope to the executor over ``PublishChunk``.

        Silent no-op when not connected to a sidecar. Raises on a non-OK
        response (an invalid channel, transport failure) — same surfacing as
        :func:`aithericon.emit`.
        """
        stub = get_stub()
        if stub is None:
            return

        from aithericon._proto import executor_sidecar_pb2

        resp = stub.PublishChunk(
            executor_sidecar_pb2.PublishChunkRequest(
                channel=self._name,
                envelope=executor_sidecar_pb2.ChunkMessage(
                    seq=seq,
                    content_type=content_type,
                    payload=payload,
                    is_eof=is_eof,
                ),
            )
        )
        if resp.status != executor_sidecar_pb2.RESPONSE_STATUS_OK:
            raise RuntimeError(resp.error_message)

    def _close_transport(self):
        """Publish the in-band EOF envelope terminating the byte stream.

        Fired before the ``close`` control token so the consumer's transport
        reader sees the terminator and ends its loop; ``payload`` is empty and
        ``content_type`` carries the channel default for symmetry.
        """
        self._publish(
            seq=self._seq,
            content_type=self._default_content_type or "application/octet-stream",
            payload=b"",
            is_eof=True,
        )


class open_output:
    """Context manager opening a data-plane byte stream on an ``out`` channel.

    ``__enter__`` validates the channel name against the manifest (must be a
    ``data``-plane channel), emits the ``open`` control token carrying the
    transport descriptor EARLY, and returns a :class:`_Writer`. ``__exit__``
    publishes the in-band EOF envelope and then emits the ``close`` control token
    stamping the final element count + ``ok`` status. Both brackets are
    suppressed if the body raised — a partial stream is not committed clean (the
    consumer drains until the transport ends; the missing ``close`` signals the
    producer aborted).
    """

    def __init__(self, name, content_type=None):
        self._entry = resolve_data_channel(name)
        self._name = name
        self._content_type = content_type
        self._writer = _Writer(name, content_type)

    def __enter__(self):
        self._emit_open()
        return self._writer

    def __exit__(self, exc_type, exc_val, exc_tb):
        if exc_type is None:
            self._writer._close_transport()
            self._emit_close()
        return False

    def _descriptor(self):
        """Build the JSON transport descriptor the ``open`` token carries.

        ``{transport, subject, content_type, credential?}`` (docs/25 §6). v1
        emits ``transport: "jetstream"`` and no ``credential`` (the dev NATS path
        is open; the engine wraps a scoped grant at submit when one is required —
        the consumer's executor unwraps it, never the SDK). ``content_type`` is
        the channel-level default hint; per-element content types override it on
        each envelope.
        """
        descriptor = {
            "transport": _TRANSPORT_JETSTREAM,
            "subject": _datastream_subject(self._name),
        }
        if self._content_type:
            descriptor["content_type"] = self._content_type
        return descriptor

    def _emit_open(self):
        """Fire the ``open`` control token with the transport descriptor."""
        stub = get_stub()
        if stub is None:
            return

        from aithericon._proto import executor_sidecar_pb2

        resp = stub.EmitControl(
            executor_sidecar_pb2.EmitControlRequest(
                channel=self._name,
                kind=executor_sidecar_pb2.CONTROL_KIND_OPEN,
                payload_json=json.dumps(self._descriptor()),
                scatter_uid="",
            )
        )
        if resp.status != executor_sidecar_pb2.RESPONSE_STATUS_OK:
            raise RuntimeError(resp.error_message)

    def _emit_close(self):
        """Fire the ``close`` control token stamping the count + status."""
        stub = get_stub()
        if stub is None:
            return

        from aithericon._proto import executor_sidecar_pb2

        resp = stub.EmitControl(
            executor_sidecar_pb2.EmitControlRequest(
                channel=self._name,
                kind=executor_sidecar_pb2.CONTROL_KIND_CLOSE,
                payload_json=json.dumps(
                    {"count": self._writer.count, "status": "ok"}
                ),
                scatter_uid="",
            )
        )
        if resp.status != executor_sidecar_pb2.RESPONSE_STATUS_OK:
            raise RuntimeError(resp.error_message)
