"""Shared channel-manifest resolution for the data plane (docs/25).

The control-plane resolver (:func:`aithericon._emit._resolve_control_channel`)
validates a channel name against the job's baked channel manifest and asserts the
``control`` plane. This is the symmetric helper for the ``data`` plane, reusing
:func:`aithericon._emit._load_manifest` so both planes parse the manifest the
same way — friendly, early validation when ``AITHERICON_CHANNELS`` is exposed,
skipped (the sidecar enforces authoritatively) when it is not.

The manifest entry shape is ``{name, plane, contract?, element_kind}`` and is
direction-agnostic: a producer's ``out`` data channel and a consumer's ``in``
data channel are both ``plane: "data"``. The SDK therefore validates the plane
only; the executor/engine enforce direction.
"""

from aithericon._emit import _load_manifest


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
