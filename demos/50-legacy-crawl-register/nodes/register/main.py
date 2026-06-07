"""Register each crawled file into file_inventory by reference (docs/32 §4).

Reads the flattened listing from the `fold` step and the Start parameters, then
POSTs a batched by-reference register request to mekhan's own
`/api/v1/inventory/register` endpoint — one `{file_server_id, path, status:
"indexed"}` row per file. No bytes move. Crawl carries no content hash, so no
catalogue rows are inserted here (those appear once a later `probe` populates
content_hash and re-registers with it). The registered rows then show up in the
app's Inventory view.

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

entries = [
    {"file_server_id": server_id, "path": f["path"], "status": "indexed"}
    for f in files
    if isinstance(f, dict) and f.get("path")
]

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
    f"registered {len(entries)} files into '{server_id}' via {url} → "
    f"inventory_upserted={inv}, catalogue_inserted={cat}",
    files_seen=len(entries),
    inventory_upserted=inv,
    catalogue_inserted=cat,
)

set_output("files_seen", len(entries))
set_output("inventory_upserted", inv)
set_output("catalogue_inserted", cat)
