"""Fold the gathered crawl batches into one flat {path,size} listing (docs/25).

The `crawl → fold` edge carries `join: gather`, so the gather barrier re-orders
the emitted batches by index and parks the whole stream as the envelope
`{ output: [<batch>, …] }` — the runner exposes that inbound token as the
`input` global. Each batch is a `{ items: [{path,size,mtime}, …] }` dict the
crawl op emitted per `batch_size`. Flatten them into a single `{path,size}`
list; the next step turns that into inventory rows.

This is the consumer-side fold: the producer just emitted a uniform stream
(open → item* → close); the join discipline lives on the edge, not the producer.
"""

from aithericon import log_info, set_output

batches = input.output or []  # noqa: F821 — runner-injected gathered collection

files = []
for batch in batches:
    if not isinstance(batch, dict):
        continue
    for it in batch.get("items", []):
        if isinstance(it, dict) and it.get("path"):
            files.append({"path": it["path"], "size": it.get("size")})

log_info(f"folded {len(batches)} crawl batch(es) → {len(files)} files")

set_output("files", files)
set_output("count", len(files))
