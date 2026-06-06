# Collect — gather the streamed transcript segments into one transcript (docs/25).
#
# The transcribe step's Control channel `parts` is consumed by THIS edge with
# `join: gather`, so the compiler's gather barrier re-orders the emitted segments
# by their `item_idx` and parks the whole collection as the envelope
# `{ output: [<segment>, ...] }` on the channel's gathered place. The graph wires
# that place straight into this node, so the gathered envelope IS this step's
# input token — read the list off `input.output` (the runner exposes the inbound
# token as the `input` global).
#
# This is the consumer-side fold: the producer just emitted a uniform stream
# (open → item* → close); the join discipline lives on the edge, not in the
# producer. Each segment is the `{index, text, start, end}` dict transcribe
# emitted. They arrive in stream order, so concatenating their `text` in order
# reconstructs the full utterance.

segments = input.output or []  # noqa: F821 — runner-injected gathered collection

texts = [str(s.get("text", "")).strip() for s in segments if isinstance(s, dict)]
transcript = " ".join(t for t in texts if t)

seconds = round(max((s.get("end", 0) for s in segments if isinstance(s, dict)), default=0), 2)

set_output("transcript", transcript)  # noqa: F821 — runner-injected SDK global
set_output("segment_count", len(segments))  # noqa: F821
set_output("seconds", seconds)  # noqa: F821
