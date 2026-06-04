"""Scatter a fan-out of control tokens into a declared channel.

``with aithericon.scatter(name) as s:`` opens a fan-out on a ``control``-plane
channel whose contract is ``scatter``. Each ``s.emit(value)`` fires one
instance-colored item (``CONTROL_KIND_SCATTER_ITEM``, carrying a 0-based
``scatter_id``); leaving the block fires a ``CONTROL_KIND_SCATTER_CLOSE`` that
stamps the total item ``scatter_count`` so the net's gather barrier knows when
the fan-out is complete::

    import aithericon

    with aithericon.scatter("items") as s:
        for row in rows:
            s.emit(row)
    # close (count = len(rows)) fires here, on block exit

Silent no-op when not connected to a sidecar — the same script runs unchanged
outside an execution context (``emit`` does nothing; the close is also skipped).

The channel name is validated against the job's channel manifest exactly as
:func:`aithericon.emit` does (early friendly check when ``AITHERICON_CHANNELS``
is exposed; the sidecar enforces authoritatively otherwise).
"""

import json
import uuid

from aithericon._client import get_stub
from aithericon._emit import _resolve_control_channel


class _Scatter:
    """The fan-out handle yielded by :func:`scatter`.

    Tracks the running item count so ``__enter__``'s ``emit`` stamps an
    incrementing ``scatter_id`` and ``__exit__``'s close stamps the final
    ``scatter_count``. A single ``scatter_uid`` is minted per handle and stamped
    on every item + the close so the engine can correlate one fan-out.
    """

    def __init__(self, name):
        self._name = name
        self._count = 0
        self._scatter_uid = uuid.uuid4().hex

    def emit(self, value):
        """Fire one scatter item, advancing the 0-based item index.

        ``value`` is the item's control-token payload (JSON-serializable).
        Silent no-op when not connected — but the index still advances so the
        close count reflects the items the author intended.
        """
        index = self._count
        self._count += 1

        stub = get_stub()
        if stub is None:
            return

        from aithericon._proto import executor_sidecar_pb2

        resp = stub.EmitControl(
            executor_sidecar_pb2.EmitControlRequest(
                channel=self._name,
                kind=executor_sidecar_pb2.CONTROL_KIND_SCATTER_ITEM,
                payload_json=json.dumps(value),
                scatter_id=index,
                scatter_uid=self._scatter_uid,
            )
        )
        if resp.status != executor_sidecar_pb2.RESPONSE_STATUS_OK:
            raise RuntimeError(resp.error_message)

    def _close(self):
        """Fire the scatter-close, stamping the total item count."""
        stub = get_stub()
        if stub is None:
            return

        from aithericon._proto import executor_sidecar_pb2

        resp = stub.EmitControl(
            executor_sidecar_pb2.EmitControlRequest(
                channel=self._name,
                kind=executor_sidecar_pb2.CONTROL_KIND_SCATTER_CLOSE,
                scatter_count=self._count,
                scatter_uid=self._scatter_uid,
            )
        )
        if resp.status != executor_sidecar_pb2.RESPONSE_STATUS_OK:
            raise RuntimeError(resp.error_message)


class scatter:
    """Context manager opening a scatter fan-out on a control channel.

    ``__enter__`` validates the channel name against the manifest and returns a
    :class:`_Scatter` handle (``.emit(value)``). ``__exit__`` fires the
    scatter-close stamping the final count. The close is suppressed if the body
    raised — a partial fan-out is not committed.
    """

    def __init__(self, name):
        _resolve_control_channel(name)
        self._handle = _Scatter(name)

    def __enter__(self):
        return self._handle

    def __exit__(self, exc_type, exc_val, exc_tb):
        if exc_type is None:
            self._handle._close()
        return False
