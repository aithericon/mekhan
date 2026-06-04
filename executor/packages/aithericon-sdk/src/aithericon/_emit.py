"""Emit a control-plane ``signal`` token into a statically-declared channel.

This is the Python side of dynamic control-token emission (docs/25). A step
declares output **channels** in its node config; the compiler bakes a slim
**channel manifest** (name + plane + contract + element kind) into the job and
synthesizes one petri place per channel. At runtime the child calls
:func:`emit` to fire one control token into a named ``control``-plane channel,
which the engine ingests (fire-and-forget) to deposit a token into
``p_{node}_{channel}`` and trigger the downstream work wired off that handle::

    import aithericon

    if found_anomaly:
        aithericon.emit("anomaly", {"sensor": "s3", "value": 91.2})

Silent no-op when the SDK is not connected to a sidecar (no
``AITHERICON_IPC_SOCKET``) — same contract as :func:`aithericon.set_output` —
so the same script runs unchanged outside an execution context.

The manifest reaches Python via the ``AITHERICON_CHANNELS`` env var (a JSON
array of ``{name, plane, contract, element_kind}`` objects, the serialized
``ExecutionJob.channels`` the executor injects in ``InjectEnvironmentHook``).
It is used only for an *early, friendly* validation: when the env var is
present we reject an unknown channel name or a ``data``-plane channel before the
RPC; when it is absent we skip the local check and let the sidecar — which holds
the authoritative manifest and rejects the same cases with
``RESPONSE_STATUS_INVALID_ARGUMENT`` — be the source of truth.
"""

import json
import os

from aithericon._client import get_stub


def _load_manifest():
    """Return the job's channel manifest, or ``None`` when not exposed.

    Parses the ``AITHERICON_CHANNELS`` env var (JSON array of channel entries).
    Returns ``None`` (validation is skipped) when the var is unset or malformed
    — the sidecar enforces the manifest authoritatively regardless.
    """
    raw = os.environ.get("AITHERICON_CHANNELS")
    if not raw:
        return None
    try:
        entries = json.loads(raw)
    except (json.JSONDecodeError, ValueError):
        return None
    if not isinstance(entries, list):
        return None
    return entries


def _resolve_control_channel(name):
    """Validate ``name`` against the manifest, raising on a category error.

    Returns the matched manifest entry, or ``None`` when the manifest is not
    exposed to this process (local validation skipped — sidecar enforces).
    Raises :class:`ValueError` when the manifest *is* present and ``name`` is
    unknown or names a non-``control`` channel.
    """
    if not name or not isinstance(name, str):
        raise ValueError("channel name must be a non-empty string")

    manifest = _load_manifest()
    if manifest is None:
        return None

    entry = next((c for c in manifest if c.get("name") == name), None)
    if entry is None:
        declared = sorted(c.get("name", "") for c in manifest)
        raise ValueError(
            f"channel '{name}' is not declared in this job's channel "
            f"manifest (declared: {declared})"
        )
    if entry.get("plane") != "control":
        raise ValueError(
            f"channel '{name}' is a '{entry.get('plane')}' channel, "
            f"not a control channel — emit() targets control channels only"
        )
    return entry


def emit(name, payload):
    """Fire one ``signal`` control token into the named control channel.

    ``name`` is a control-plane channel declared in this step's node config.
    ``payload`` is the control-token value (JSON-serializable). Silent no-op
    when not connected to a sidecar.

    Raises :class:`ValueError` when the channel manifest is exposed
    (``AITHERICON_CHANNELS``) and ``name`` is unknown or is a data-plane channel.
    """
    _resolve_control_channel(name)

    stub = get_stub()
    if stub is None:
        return

    from aithericon._proto import executor_sidecar_pb2

    resp = stub.EmitControl(
        executor_sidecar_pb2.EmitControlRequest(
            channel=name,
            kind=executor_sidecar_pb2.CONTROL_KIND_SIGNAL,
            payload_json=json.dumps(payload),
            scatter_uid="",
        )
    )
    if resp.status != executor_sidecar_pb2.RESPONSE_STATUS_OK:
        raise RuntimeError(resp.error_message)
