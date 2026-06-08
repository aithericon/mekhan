"""Register each crawled file BY REFERENCE — log_artifact does both halves.

The runner is co-located with the files, so for each crawled file we call
log_artifact(..., upload=False): the SDK/executor hashes the file locally,
uploads NOTHING (the bytes stay on the file server), and the platform's
causality projector fills BOTH the content-addressed catalogue row AND the
physical file_inventory copy at (file_server_id, reference_path) — atomically,
server-side. No HTTP, no urllib: log_artifact is the whole registration.

The literal `fold.files`, `seed.root`, and `start.file_server_id` reads below
MUST stay verbatim — the compiler scans this source for those <slug>.<field>
references to synthesize the read-arcs that borrow the upstream fields.
"""

import os

from aithericon import log_artifact, log_info, set_output

files = fold.files or []  # noqa: F821 — borrowed from the `fold` step
server_id = start.file_server_id or "demo-nas"  # noqa: F821 — Start borrow

# Crawl emitted user-facing paths relative to the storage root (/tmp); seed.root
# is /tmp/mekhan-crawl-demo, so the storage root is its parent.
seed_root = seed.root or "/tmp/mekhan-crawl-demo"  # noqa: F821 — `seed` borrow
storage_root = os.path.dirname(seed_root.rstrip("/"))

registered = 0
for f in files:
    if not isinstance(f, dict) or not f.get("path"):
        continue
    rel = f["path"]
    abs_path = os.path.join(storage_root, rel)
    # upload=False => register by reference: hash locally, move no bytes, record
    # the physical location as (server_id, rel). blocking=True so the catalogue +
    # inventory rows are durable before this node returns.
    log_artifact(
        abs_path,
        name=os.path.basename(rel),
        category="dataset",
        upload=False,
        file_server_id=server_id,
        reference_path=rel,
        blocking=True,
    )
    registered += 1

log_info(
    f"registered {registered} files by reference into '{server_id}' "
    f"(no bytes moved; log_artifact filled catalogue + inventory)",
    files_registered=registered,
)
set_output("files_registered", registered)
