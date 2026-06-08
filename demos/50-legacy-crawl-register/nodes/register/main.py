"""Probe + register each indexed file — fills BOTH halves (docs/32 §4).

The coupling half of the lifecycle. The runner is co-located with the seeded
files, so it HASHES each one (stdlib hashlib SHA-256 over the storage root +
crawled path) — the migration model where the edge probes and the server
couples. It then POSTs a batched register request to mekhan's
`/api/v1/inventory/register` endpoint — one
`{file_server_id, path, content_hash, status: "registered", size_bytes, name}`
row per file. That endpoint fills BOTH halves of the equation atomically: a
content-addressed `catalogue_entries` row keyed on the hash AND the physical
`file_inventory` copy (promoting the same `(file_server_id, path)` row the
`index` step created from `indexed` to `registered`). Because every file now
carries a content hash, `catalogue_inserted` matches the file count — you can
no longer register just one side. No bytes move to mekhan; only the references.

The literal `fold.files`, `seed.root`, `start.file_server_id`, and
`start.mekhan_url` reads below MUST remain verbatim — the compiler scans this
source for those `<slug>.<field>` references to synthesize the read-arcs that
borrow the upstream fields onto this node.
"""

import hashlib
import json
import os
import urllib.request

from aithericon import log_info, set_output

files = fold.files or []  # noqa: F821 — borrowed from the `fold` step
server_id = start.file_server_id or "demo-nas"  # noqa: F821 — Start borrow

# The crawl emitted user-facing paths relative to the storage root (/tmp). The
# seed step's root is /tmp/mekhan-crawl-demo, so the storage root is its parent.
seed_root = seed.root or "/tmp/mekhan-crawl-demo"  # noqa: F821 — `seed` borrow
storage_root = os.path.dirname(seed_root.rstrip("/"))

# Target precedence: explicit Start field → executor env → localhost default.
base_url = (start.mekhan_url or "").rstrip("/")  # noqa: F821 — Start borrow
if not base_url:
    base_url = os.environ.get("MEKHAN_SERVICE_URL", "http://localhost:13100").rstrip("/")


def sha256_of(abs_path):
    h = hashlib.sha256()
    with open(abs_path, "rb") as fh:
        for chunk in iter(lambda: fh.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


entries = []
for f in files:
    if not isinstance(f, dict) or not f.get("path"):
        continue
    rel = f["path"]
    abs_path = os.path.join(storage_root, rel)
    digest = sha256_of(abs_path)
    size = f.get("size")
    if size is None:
        size = os.path.getsize(abs_path)
    entries.append(
        {
            "file_server_id": server_id,
            "path": rel,
            "content_hash": digest,
            "status": "registered",
            "size_bytes": int(size),
            "name": os.path.basename(rel),
        }
    )

url = f"{base_url}/api/v1/inventory/register"
body = json.dumps({"entries": entries}).encode()
req = urllib.request.Request(
    url, data=body, method="POST", headers={"Content-Type": "application/json"}
)
with urllib.request.urlopen(req, timeout=30) as resp:
    result = json.loads(resp.read().decode())

inv = int(result.get("inventory_upserted", 0))
cat = int(result.get("catalogue_inserted", 0))
log_info(
    f"registered {len(entries)} hashed files into '{server_id}' via {url} → "
    f"inventory_upserted={inv}, catalogue_inserted={cat} (both halves filled)",
    files_seen=len(entries),
    inventory_upserted=inv,
    catalogue_inserted=cat,
)

set_output("files_seen", len(entries))
set_output("inventory_upserted", inv)
set_output("catalogue_inserted", cat)
