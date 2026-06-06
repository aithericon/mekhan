"""Summary — gather the streamed detections into an aggregate report (docs/25).

The detector step's Control channel `detections` is consumed by THIS edge with
`join: gather`, so the compiler's gather barrier re-orders the emitted detection
tokens by their `item_idx` and parks the whole collection as the envelope
`{ output: [<detection>, ...] }` on the channel's gathered place — which the
graph wires straight in as this step's input token (the runner exposes the
inbound token as the `input` global).

This is the consumer-side fold: the detector just emitted a uniform stream
(open → item* → close); the join discipline lives on the edge, not in the
producer. Each item is the `{frame, t, label, confidence, bbox, track_id}` dict
the detector emitted. We tally totals, a per-class histogram, the set of
distinct tracked objects, and the peak per-frame object count.
"""

import json

from aithericon import log_info

detections = input.output or []  # noqa: F821 — runner-injected gathered collection
detections = [d for d in detections if isinstance(d, dict)]

# NOTE: this edge consumes `detections` with `join: gather`, a BARRIER — so the
# whole stream has already landed by the time this node runs; the log below is
# the folded batch, not a live tick. The live per-frame feed is logged on the
# emit side (the `detector` step), which streams to the execution log AS the
# clip is processed. To fold live instead, switch this edge to `join: each`.
total = len(detections)
log_info(f"gathered {total} detections from the `detections` control stream")

class_counts = {}
for d in detections:
    label = str(d.get("label", "?"))
    class_counts[label] = class_counts.get(label, 0) + 1

# Distinct tracked objects (ByteTrack ids); fall back to label-only if untracked.
track_ids = {d["track_id"] for d in detections if d.get("track_id") is not None}

per_frame = {}
for d in detections:
    f = d.get("frame", -1)
    per_frame[f] = per_frame.get(f, 0) + 1
peak_per_frame = max(per_frame.values(), default=0)

# A compact human-readable line for the End summary, e.g. "person×27, car×5".
ranked = sorted(class_counts.items(), key=lambda kv: (-kv[1], kv[0]))
labels_line = ", ".join(f"{name}×{n}" for name, n in ranked)

log_info(
    f"detection report — {total} objects, {len(class_counts)} classes "
    f"({labels_line}); {len(track_ids)} distinct tracked, peak {peak_per_frame}/frame",
    total_objects=total,
    distinct_tracked=len(track_ids),
    peak_per_frame=peak_per_frame,
    **{f"n_{k}": v for k, v in class_counts.items()},
)

set_output("total_objects", total)  # noqa: F821 — runner-injected SDK global
set_output("distinct_classes", len(class_counts))  # noqa: F821
set_output("distinct_tracked", len(track_ids))  # noqa: F821
set_output("peak_per_frame", peak_per_frame)  # noqa: F821
set_output("class_counts", json.dumps(class_counts))  # noqa: F821
set_output("labels", labels_line)  # noqa: F821
