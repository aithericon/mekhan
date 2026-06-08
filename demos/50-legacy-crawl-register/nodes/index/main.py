"""Index each crawled file into file_inventory by observation (docs/32 §4).

The hashless OBSERVE half: reads the flattened listing from the `fold` step and
the Start parameters, then POSTs a batched request to mekhan's own
`/api/v1/inventory/index` endpoint — one `{path}` row per file under the given
`file_server_id`, status `indexed`. This writes `file_inventory` rows ONLY; it
never touches `catalogue_entries`, because an indexed file has a location but no
claimed content identity yet (crawl read no bytes). The `register` step later
hashes the bytes and promotes these same rows to a coupled catalogue entry.

The literal `fold.files`, `start.file_server_id`, and `start.mekhan_url` reads
below MUST remain verbatim — the compiler scans this source for those
`<slug>.<field>` references to synthesize the read-arcs that borrow the upstream
fields onto this node.
"""

import json
import os
import urllib.request

from aithericon import log_info, set_output

files = fold.files or []  # noqa: F821 — borrowed from the `fold` step
server_id = start.file_server_id or "demo-nas"  # noqa: F821 — Start borrow

# Target precedence: explicit Start field → executor env → localhost default.
base_url = (start.mekhan_url or "").rstrip("/")  # noqa: F821 — Start borrow
if not base_url:
    base_url = os.environ.get("MEKHAN_SERVICE_URL", "http://localhost:13100").rstrip("/")

items = [
    {"path": f["path"], "status": "indexed"}
    for f in files
    if isinstance(f, dict) and f.get("path")
]

url = f"{base_url}/api/v1/inventory/index"
body = json.dumps({"file_server_id": server_id, "items": items}).encode()
req = urllib.request.Request(
    url, data=body, method="POST", headers={"Content-Type": "application/json"}
)
with urllib.request.urlopen(req, timeout=30) as resp:
    result = json.loads(resp.read().decode())

inv = int(result.get("inventory_upserted", 0))
log_info(
    f"indexed {len(items)} files into '{server_id}' via {url} → "
    f"inventory_upserted={inv} (no catalogue rows — hashless observation)",
    files_seen=len(items),
    inventory_upserted=inv,
)

set_output("files_seen", len(items))
set_output("inventory_upserted", inv)
