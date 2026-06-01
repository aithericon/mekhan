# Render — rasterize the uploaded document to per-page PNG images so the
# downstream VISION steps (classify, extract-generic) get a raster page image
# instead of a raw PDF. Ollama rejects a PDF fed as `images: [{path: <.pdf>}]`
# with HTTP 500 `image: unknown format`; vision models need real raster pages.
# This is the generic "rasterize document to page images" stage — it carries
# no clinic-domain vocabulary.
#
# Mechanism: PyMuPDF (`pymupdf` / `fitz`) opens the document and renders every
# page to a PNG at 150 dpi. PDFs rasterize page-by-page; already-raster inputs
# (PNG/JPG/WebP) open as a single-page document and re-emit one PNG so the
# contract is uniform regardless of input kind.
#
# ── Acquiring the document BYTES (the hard part — see report) ───────────────
# Python automated_step borrows are Envelope-only: the borrow stages the
# producer's JSON envelope as `<slug>.json`, NOT the raw binary. So referencing
# `start.document_file` below gives us the file-ref METADATA
# (`{key, url, filename, content_type, ...}`), not the bytes. (Contrast the
# LLM `images[].path` and surya `file:` borrows, which are File path-sites that
# download the binary into the run dir — Python has no such path-site borrow.)
#
# We therefore resolve the bytes by trying, in order:
#   1. a raw binary staged in AITHERICON_INPUTS_DIR whose name MATCHES the
#      file-ref `filename` (works for free if mekhan ever gains a Python File
#      path-site borrow that downloads it). Matching by name is essential: the
#      runner ALSO stages this script (`main.py`) + the JSON envelopes here, so
#      reading "the first non-JSON file" would feed main.py to fitz.
#   2. downloading from the file-ref `url`, which is host-relative
#      (`/api/v1/files/...`) and absolutized against the file-service origin in
#      DOCUMENT_STORAGE_BASE_URL.
# Step 2 is the live path today. *** This couples the render step to an HTTP
# fetch of the upload; it is the one open integration question flagged in the
# handover. The clean platform fix is a Python File path-site borrow (which
# would make step 1 the live path), tracked separately. ***
#
# ── Output contract — the per-field File borrow constraint ──────────────────
# The LLM `images[].path` borrow (BackendFieldStage File arm) requires each
# referenced producer field to be `kind: file` and stages exactly ONE file per
# `(slug, attr)` reference (the static `images[]` length is fixed at compile
# time; there is no array fan-out at one `images[].path` site). Phase-1 vision
# steps therefore reference a single lead-page field, `render.page_1`
# (kind: file) — the vision model classifies / extracts from the first page's
# layout. Every rendered page is still surfaced in `pages` + as `log_artifact`
# for telemetry/debug; multi-page vision fan-in needs engine support for
# array-valued image borrows.
#
# The page_1 output VALUE is a file-ref dict `{ "key": <path>, ... }`; the
# borrow arm reads `<producer>.detail.outputs.page_1.key`. (Same storage-key
# caveat as above: a Python `file` output is not auto-uploaded, so for the live
# LLM step to read the PNG bytes the `key` must resolve to a downloadable
# object key.)
#
# Upstream borrow (Envelope — file-ref metadata, not bytes):
#   start.document_file   # the uploaded PDF / PNG / JPG / WebP (file-ref)
#
# Outputs:
#   page_1       # lead-page image file-ref (required; the vision borrow target)
#   pages        # full array of page file-refs (telemetry / debug)
#   page_count   # number of pages rendered

import os
import urllib.request

import fitz  # PyMuPDF — the `pymupdf` wheel imports as `fitz`.

define_phases(["Acquire document", "Rasterize pages"])

update_phase("Acquire document", "running")

_inputs_dir = os.environ.get("AITHERICON_INPUTS_DIR", "")
_outputs_dir = os.environ.get("AITHERICON_OUTPUTS_DIR", "")

# The file-service ORIGIN (scheme://host[:port]) used to absolutize the
# host-relative file-ref `url`. The platform keeps file-ref URLs host-relative
# (`/api/v1/files/...`) so stored tokens stay environment-agnostic; the
# consumer supplies the origin out-of-band. In the dev stack the executor is
# launched with `DOCUMENT_STORAGE_BASE_URL=http://localhost:3100` (the
# mekhan-service file API) and the Python step inherits it (inherit_env=true).
_FILE_BASE_URL = os.environ.get("DOCUMENT_STORAGE_BASE_URL", "").rstrip("/")

# Reference the upstream file-ref. The bare `start.document_file` access is
# what the Python ref_scanner lifts into an Envelope borrow (staging
# `start.json`); `start` is exposed as an AccessibleDict global by the runner.
_file_ref = start.document_file if isinstance(start.document_file, dict) else {}


def _read_local_binary():
    # Return the raw document bytes IFF a binary whose name matches the
    # file-ref `filename` was staged in the inputs dir. Python borrows are
    # Envelope-only today — the runner stages JSON envelopes (`start.json`,
    # `input.json`) and THIS script (`main.py`) into the inputs dir, NOT the
    # upload bytes — so this normally finds nothing. It MUST match on the
    # document's own filename: blindly reading the first non-JSON file grabs
    # the staged runner script and feeds it to fitz as if it were a PDF
    # (`FzErrorFormat: no objects found`), which is exactly the bug this guards.
    if not _inputs_dir or not os.path.isdir(_inputs_dir):
        return None
    fname = _file_ref.get("filename")
    if not isinstance(fname, str) or not fname:
        return None
    candidate = os.path.join(_inputs_dir, os.path.basename(fname))
    if not os.path.isfile(candidate):
        return None
    try:
        with open(candidate, "rb") as f:
            return f.read()
    except OSError:
        return None


def _absolutize(u):
    # The file-ref `url` is host-relative (`/api/v1/files/...`); prepend the
    # configured file-service origin so urllib can fetch it. Already-absolute
    # URLs pass through unchanged.
    if not isinstance(u, str) or not u:
        return None
    if u.startswith(("http://", "https://")):
        return u
    if _FILE_BASE_URL and u.startswith("/"):
        return _FILE_BASE_URL + u
    return None


def _download(url):
    if not url or not isinstance(url, str):
        return None
    try:
        with urllib.request.urlopen(url) as resp:  # noqa: S310 — trusted upload URL
            return resp.read()
    except Exception as _e:  # noqa: BLE001
        log_warn("document download failed", url=url, error=str(_e))
        return None


# Acquisition order: a name-matched staged binary (free, zero-network) → the
# file-ref `url` absolutized against the file-service origin.
doc_bytes = _read_local_binary()
source = "inputs_dir" if doc_bytes else None

if not doc_bytes:
    doc_bytes = _download(_absolutize(_file_ref.get("url")))
    if doc_bytes:
        source = "url"

if not doc_bytes:
    log_error(
        "could not acquire document bytes",
        have_url=bool(_file_ref.get("url")),
        have_base_url=bool(_FILE_BASE_URL),
        filename=_file_ref.get("filename"),
        inputs_dir=_inputs_dir,
    )
    raise RuntimeError(
        "render: could not acquire document bytes — no staged binary matching "
        "the file-ref filename, and the file-ref url could not be fetched "
        "(set DOCUMENT_STORAGE_BASE_URL to the file-service origin)"
    )

log_info("acquired document", source=source, bytes=len(doc_bytes))
update_phase("Acquire document", "completed")

update_phase("Rasterize pages", "running")
os.makedirs(_outputs_dir, exist_ok=True)

# 150 dpi balances vision-model legibility against payload size. PyMuPDF's
# default render is 72 dpi; the zoom matrix scales it to the requested dpi.
_DPI = 150
_zoom = _DPI / 72.0
_matrix = fitz.Matrix(_zoom, _zoom)

# Open from bytes — PyMuPDF sniffs the format, so PDF and raster inputs both
# work. `filetype` hint from the file-ref extension helps non-PDF inputs.
_ext = ""
_fname = _file_ref.get("filename")
if isinstance(_fname, str) and "." in _fname:
    _ext = _fname.rsplit(".", 1)[-1].lower()

pages = []
doc = fitz.open(stream=doc_bytes, filetype=_ext or None)
try:
    page_count = doc.page_count
    for page_index in range(page_count):
        page = doc.load_page(page_index)
        pixmap = page.get_pixmap(matrix=_matrix)
        page_no = page_index + 1
        filename = f"page_{page_no:04d}.png"
        out_path = os.path.join(_outputs_dir, filename)
        pixmap.save(out_path)
        try:
            log_artifact(out_path, name=filename, category="other", mime_type="image/png")
        except Exception as _e:  # noqa: BLE001 — artifact logging is best-effort
            log_debug("log_artifact failed (non-fatal)", error=str(_e))
        # File-ref shape consumed by the LLM `images[].path` borrow
        # (BackendFieldStage File arm reads `.key`).
        pages.append(
            {
                "key": out_path,
                "page": page_no,
                "filename": filename,
                "media_type": "image/png",
            }
        )
        update_progress(
            (page_index + 1) / max(page_count, 1),
            f"rendered page {page_no}/{page_count}",
        )
finally:
    doc.close()

if not pages:
    log_error("document produced zero pages")
    raise RuntimeError("render: document produced zero pages")

log_metric("pages_rendered", float(len(pages)))
# page_1 is the borrow-addressable vision input (kind: file). Always present —
# a zero-page document already raised above.
set_output("page_1", pages[0])
set_output("pages", pages)
set_output("page_count", len(pages))
update_phase("Rasterize pages", "completed")
