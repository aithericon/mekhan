"""Emit a control-plane episode into a statically-declared channel (docs/25).

This is the Python side of dynamic control-token emission. A producer always
emits ONE uniform bracketed episode — ``open`` → zero or more ``item`` → ``close``
— into a named ``control``-plane channel. The producer does NOT decide how the
episode folds; the **consumer** edge's ``join`` (``each`` | ``gather``) does that
in the net. A single ``item`` is just an alert/signal; many ``item`` s are a
fan-out. The producer side is identical either way.

Context-manager form (zero or more items)::

    import aithericon

    with aithericon.out("items") as o:
        for row in rows:
            o.emit(row)
    # close (count = len(rows)) fires here, on clean block exit

One-shot sugar (exactly one item — the old "signal")::

    aithericon.out("anomaly").send({"sensor": "s3", "value": 91.2})

Each episode is correlated by a single ``episode_uid`` (minted per ``out``),
stamped on the ``open``, every ``item`` (alongside an incrementing ``item_idx``),
and the ``close`` (alongside the final item ``count``). The engine deposits
``open``/``item``/``close`` tokens into ``p_{node}_{channel}``; the consumer's
join sub-net folds them.

Silent no-op when the SDK is not connected to a sidecar (no
``AITHERICON_IPC_SOCKET``) — same contract as :func:`aithericon.set_output` — so
the same script runs unchanged outside an execution context. The channel name is
validated against the job's channel manifest (early friendly check when
``AITHERICON_CHANNELS`` is exposed; the sidecar enforces authoritatively
otherwise; must be a ``control``-plane channel).
"""

import json
import uuid

from aithericon._channels import _resolve_control_channel
from aithericon._client import get_stub


class _Episode:
    """The control-episode handle yielded by ``with aithericon.out(name)``.

    Tracks the running item count so :meth:`emit` stamps an incrementing 0-based
    ``item_idx`` and the close stamps the final ``count``. One ``episode_uid`` is
    minted per handle and stamped on the open, every item, and the close so the
    engine can correlate a single episode.
    """

    def __init__(self, name):
        self._name = name
        self._count = 0
        self._episode_uid = uuid.uuid4().hex

    def _send_control(self, kind, *, payload_json="", item_idx=0, count=0):
        """Issue one ``EmitControl`` RPC, or silently no-op when disconnected."""
        stub = get_stub()
        if stub is None:
            return

        from aithericon._proto import executor_sidecar_pb2

        resp = stub.EmitControl(
            executor_sidecar_pb2.EmitControlRequest(
                channel=self._name,
                kind=kind,
                payload_json=payload_json,
                item_idx=item_idx,
                count=count,
                episode_uid=self._episode_uid,
            )
        )
        if resp.status != executor_sidecar_pb2.RESPONSE_STATUS_OK:
            raise RuntimeError(resp.error_message)

    def _open(self):
        """Fire the episode ``open`` lifecycle marker."""
        from aithericon._proto import executor_sidecar_pb2

        self._send_control(executor_sidecar_pb2.CONTROL_KIND_OPEN)

    def emit(self, value):
        """Fire one ``item``, advancing the 0-based ``item_idx``.

        ``value`` is the item's control-token payload (JSON-serializable). Silent
        no-op when not connected — but the index still advances so the close count
        reflects the items the author intended.
        """
        from aithericon._proto import executor_sidecar_pb2

        index = self._count
        self._count += 1
        self._send_control(
            executor_sidecar_pb2.CONTROL_KIND_ITEM,
            payload_json=json.dumps(value),
            item_idx=index,
        )

    def _close(self):
        """Fire the episode ``close``, stamping the total item count."""
        from aithericon._proto import executor_sidecar_pb2

        self._send_control(
            executor_sidecar_pb2.CONTROL_KIND_CLOSE,
            count=self._count,
        )


class out:
    """Open a control-plane episode on the channel ``name``.

    As a context manager, ``__enter__`` validates the channel name against the
    manifest, mints an ``episode_uid``, fires the ``open`` lifecycle marker, and
    returns an :class:`_Episode` handle (``.emit(value)`` per item). ``__exit__``
    fires the ``close`` stamping the final count — suppressed if the body raised
    (a partial episode is not committed clean).

    For the common single-item case use the :meth:`send` sugar without a ``with``
    block: ``aithericon.out(name).send(value)`` fires open + one item + close.
    """

    def __init__(self, name):
        _resolve_control_channel(name)
        self._handle = _Episode(name)

    def __enter__(self):
        self._handle._open()
        return self._handle

    def __exit__(self, exc_type, exc_val, exc_tb):
        if exc_type is None:
            self._handle._close()
        return False

    def send(self, value):
        """One-shot: emit a single-item episode (open + item 0 + close count 1).

        The terse form of the old ``signal`` — a single alert with no ``with``
        block. ``value`` is the item payload (JSON-serializable). Silent no-op
        when not connected. (Single-event fusion is a deferred micro-opt; the
        three control emissions are issued for now.)
        """
        self._handle._open()
        self._handle.emit(value)
        self._handle._close()
