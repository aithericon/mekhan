"""Load the workflow token and any staged input files.

The platform has exactly one input model: a single accumulating **workflow
token**. The compiler's prepare transition snapshots the upstream Petri token
and stages it as ``input.json``; ``token()`` returns it as a :class:`Token`.

Arbitrarily named files (config refs, binary ``*.npy``, catalogue artifacts)
are a separate, explicit channel — read those with :func:`load_inputs` or
straight off ``AITHERICON_INPUTS_DIR``. Day-to-day step code wants
:func:`token` (or the generated typed ``load_input()``).
"""

import json
import os


def _wrap(value):
    """Recursively wrap dicts as :class:`Token` so attribute access nests.

    Lists have their dict elements wrapped too; scalars pass through.
    """
    if isinstance(value, Token):
        return value
    if isinstance(value, dict):
        return Token(value)
    if isinstance(value, list):
        return [_wrap(v) for v in value]
    return value


class Token(dict):
    """The accumulating workflow token.

    A plain ``dict`` (item access, ``.get()``, ``in``, iteration, JSON
    serialisation all work as usual) that *additionally* exposes every field
    as an attribute::

        t = aithericon.token()
        t.vendor_name        # -> "ACME" (or None if absent)
        t["vendor_name"]     # KeyError if absent (standard dict)
        t.get("vendor_name") # None / default if absent

    A missing **attribute** returns ``None`` rather than raising, so the
    ``t.field or default`` idiom stays clean even when the graph shape drifts
    ahead of the code. Typos and out-of-scope fields are caught at *author*
    time instead: the generated ``_aithericon_io.pyi`` declares the exact
    per-node field set, so the editor / type-checker flags
    ``t.vendorr_name``. Item access keeps the standard dict contract (missing
    key → ``KeyError``) and is the escape hatch for fields whose name collides
    with a dict method (e.g. ``t["items"]``).

    Nested objects are wrapped consistently — via attribute, ``[]`` *and*
    ``get()`` — so ``t.address.city``, ``t["address"].city`` and
    ``t.get("items")[0].sku`` all navigate. Pulled-out containers are fresh
    copies (the token is read-only input), so don't mutate-in-place expecting
    it to stick.
    """

    __slots__ = ()

    def __getattr__(self, name):
        # Only invoked when normal attribute lookup fails. Never intercept
        # dunders — that would break copy/pickle/repr and friends.
        if name.startswith("__") and name.endswith("__"):
            raise AttributeError(name)
        return _wrap(dict.get(self, name))

    def __getitem__(self, key):
        return _wrap(dict.__getitem__(self, key))

    def get(self, key, default=None):
        return _wrap(dict.get(self, key, default))


def load_inputs(inputs_dir=None):
    """Load every staged input file as a ``{filename: value}`` dict.

    Tries JSON parse for each file, falls back to raw string content. Binary
    reference files (e.g. ``*.npy``) are skipped — open them by path under
    ``AITHERICON_INPUTS_DIR``. Returns ``{}`` if the directory is absent.

    This is the *named-file* escape hatch. For the workflow token, prefer
    :func:`token` — the file-map shape (including the ``input.json`` key) is
    an implementation detail.
    """
    inputs_dir = inputs_dir or os.environ.get("AITHERICON_INPUTS_DIR")
    if not inputs_dir or not os.path.isdir(inputs_dir):
        return {}
    result = {}
    for entry in os.listdir(inputs_dir):
        path = os.path.join(inputs_dir, entry)
        if not os.path.isfile(path):
            continue
        try:
            with open(path, encoding="utf-8") as f:
                content = f.read()
        except (UnicodeDecodeError, OSError):
            # Binary reference file (e.g. *.npy). Leave on disk; user code
            # can open it by path under AITHERICON_INPUTS_DIR.
            continue
        try:
            result[entry] = json.loads(content)
        except (json.JSONDecodeError, ValueError):
            result[entry] = content
    return result


def token(inputs_dir=None):
    """Return the workflow token (the staged ``input.json``) as a :class:`Token`.

    This is the single accumulating token the compiler's prepare transition
    snapshots for the step — the same ``input.<field>`` shape decision guards
    and typed ports see. Returns an empty :class:`Token` when none was staged.
    """
    return _wrap(load_inputs(inputs_dir).get("input.json", {}))
