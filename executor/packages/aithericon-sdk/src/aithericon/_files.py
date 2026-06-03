"""Lazy retrieval of storage-path files (e.g. asset ``File``-field values).

An asset record's ``File`` field is stored as a **storage-path string** — the
bytes live in the platform's object store, which the child process can't reach
directly (it holds no storage credentials; the sidecar does). A :class:`File`
wraps that path and fetches it on demand through the sidecar, so you download
only the one file you actually selected — never the whole collection::

    mat = next(m for m in metals_db if m.name == "Copper C110")
    path = mat.datasheet.retrieve()       # -> local path, downloaded once
    data = mat.datasheet.read_bytes()     # or read straight into memory
    with open(mat.datasheet) as f:        # File is os.PathLike (retrieves)
        ...

The runner auto-wraps ``File``-typed fields of staged asset records as
:class:`File` objects, so ``record.<field>`` is a ``File`` you can call
``.retrieve()`` on. Use :func:`file` to wrap a bare path string yourself.
"""

from aithericon._client import get_stub


class File:
    """A reference to a stored file, fetched on demand via the sidecar.

    Holds the ``storage_path``; :meth:`retrieve` downloads it into the run
    directory (once — the local path is cached) and returns that local path.
    Implements ``os.PathLike`` so ``open(file)`` / ``pathlib.Path(file)``
    transparently trigger a retrieve.
    """

    __slots__ = ("storage_path", "_local_path")

    def __init__(self, storage_path):
        self.storage_path = storage_path
        self._local_path = None

    def retrieve(self):
        """Download the file into the run directory; return its local path.

        Idempotent + cached: repeated calls (and a repeat for the same storage
        path across :class:`File` instances) return a local path without
        re-downloading.
        """
        if self._local_path is not None:
            return self._local_path
        from aithericon._proto import executor_sidecar_pb2 as pb2

        resp = get_stub().RetrieveFile(
            pb2.RetrieveFileRequest(storage_path=self.storage_path)
        )
        if resp.status != pb2.RESPONSE_STATUS_OK:
            raise RuntimeError(
                f"file retrieve failed for {self.storage_path!r}: {resp.error_message}"
            )
        self._local_path = resp.local_path
        return self._local_path

    def read_bytes(self):
        """Retrieve (if needed) and return the file's full contents as bytes."""
        with open(self.retrieve(), "rb") as f:
            return f.read()

    def read_text(self, encoding="utf-8"):
        """Retrieve (if needed) and return the file decoded as text."""
        with open(self.retrieve(), "r", encoding=encoding) as f:
            return f.read()

    def __fspath__(self):
        # os.PathLike — open(file) / Path(file) retrieve transparently.
        return self.retrieve()

    def __repr__(self):
        return f"File({self.storage_path!r})"


def file(value):
    """Wrap a storage-path string (or an existing :class:`File`) as a ``File``.

    Returns ``None`` for ``None`` / empty (an optional File field left unset),
    so ``aithericon.file(record.datasheet)`` is safe on sparse records.
    """
    if value is None or value == "":
        return None
    if isinstance(value, File):
        return value
    return File(str(value))
