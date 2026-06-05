"""Shared channel-manifest resolution for both planes (docs/25).

The compiler bakes a slim **channel manifest** into the job: a JSON array of
``{name, plane, element_kind}`` entries (the serialized ``ExecutionJob.channels``
the executor injects in ``InjectEnvironmentHook``), exposed to the child via the
``AITHERICON_CHANNELS`` env var. There is **no** producer-side ``contract`` field
any more — a producer always emits one uniform bracketed episode (open/item/close)
and the *consumer* edge's ``join`` decides how it folds (docs/25). This module is
the single place the SDK parses that manifest, so both planes validate the same
way: a friendly, early check when ``AITHERICON_CHANNELS`` is exposed, and a skip
(the sidecar enforces authoritatively) when it is not.

The manifest entry shape is direction-agnostic: a producer's ``out`` channel and a
consumer's ``in`` channel are both the same ``plane``. The SDK therefore validates
the plane only; the executor/engine enforce direction.
"""

import json
import os


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
            f"not a control channel — out() targets control channels only"
        )
    return entry


def resolve_data_channel(name):
    """Validate ``name`` is a declared ``data``-plane channel.

    Returns the matched manifest entry, or ``None`` when the manifest is not
    exposed to this process (local validation skipped — sidecar enforces).
    Raises :class:`ValueError` when the manifest *is* present and ``name`` is
    unknown or names a non-``data`` channel.
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
    if entry.get("plane") != "data":
        raise ValueError(
            f"channel '{name}' is a '{entry.get('plane')}' channel, "
            f"not a data channel — open_output()/stream() target data "
            f"channels only"
        )
    return entry


def element_kind_for(name, default="any"):
    """Return the declared ``element_kind`` for ``name`` (``default`` if absent).

    ``"json"`` / ``"binary"`` / ``"any"`` — drives how :func:`aithericon.stream`
    decodes each envelope's payload. When the manifest is not exposed, returns
    ``default`` (``"any"`` → JSON-decode, the safe generic).
    """
    entry = resolve_data_channel(name)
    if entry is None:
        return default
    return entry.get("element_kind", default)
