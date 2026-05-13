"""Log artifact files via IPC."""

import os

from aithericon._client import get_stub

_CATEGORY_MAP = None


def _get_category_map():
    global _CATEGORY_MAP
    if _CATEGORY_MAP is None:
        from aithericon._proto import executor_sidecar_pb2

        _CATEGORY_MAP = {
            "other": executor_sidecar_pb2.ARTIFACT_CATEGORY_OTHER,
            "model": executor_sidecar_pb2.ARTIFACT_CATEGORY_MODEL,
            "dataset": executor_sidecar_pb2.ARTIFACT_CATEGORY_DATASET,
            "plot": executor_sidecar_pb2.ARTIFACT_CATEGORY_PLOT,
            "log": executor_sidecar_pb2.ARTIFACT_CATEGORY_LOG,
            "checkpoint": executor_sidecar_pb2.ARTIFACT_CATEGORY_CHECKPOINT,
            "config": executor_sidecar_pb2.ARTIFACT_CATEGORY_CONFIG,
            "metric": executor_sidecar_pb2.ARTIFACT_CATEGORY_METRIC,
        }
    return _CATEGORY_MAP


def log_artifact(
    path,
    name=None,
    category="other",
    mime_type="",
    metadata=None,
    extract_metadata=False,
    blocking=False,
):
    """Log an artifact file via IPC.

    By default the sidecar accepts the request immediately and uploads in the
    background. Set ``blocking=True`` to wait for the upload to complete before
    returning (useful when the file might be deleted afterwards or when you need
    confirmation that the artifact was stored).

    Args:
        path: Path to the artifact file.
        name: Display name (defaults to filename).
        category: One of "other", "model", "dataset", "plot", "log",
                  "checkpoint", "config", "metric".
        mime_type: MIME type of the file.
        metadata: Optional dict of string key-value metadata.
        extract_metadata: Whether the sidecar should extract file metadata.
        blocking: If True, wait for the upload to finish before returning.
    """
    stub = get_stub()
    if not stub:
        return

    from aithericon._proto import executor_sidecar_pb2

    artifact_name = name or os.path.basename(path)
    cat_map = _get_category_map()

    stub.LogArtifact(
        executor_sidecar_pb2.LogArtifactRequest(
            artifact_id=artifact_name,
            path=os.path.abspath(path),
            name=artifact_name,
            category=cat_map.get(category, executor_sidecar_pb2.ARTIFACT_CATEGORY_OTHER),
            mime_type=mime_type,
            metadata=metadata or {},
            extract_file_metadata=extract_metadata,
            blocking=blocking,
        )
    )
