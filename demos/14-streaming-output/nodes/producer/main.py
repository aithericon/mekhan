# Producer — streaming-output prototype (OUTPUT channel, Phase B: per-call emit).
#
# This AutomatedStep has `streamOutput: true` in graph.json, so the compiler
# synthesizes a Signal place `p_producer_stream` and the executor lifecycle's
# `log_output` transition grows a second arc onto it. Every `set_output(name,
# value)` this step makes becomes a structured `OutputSet { name, value }`
# event on the executor's OUTPUT channel (EventCategory::Output), routed into
# `p_producer_stream` as ONE token per output. An edge from this node's
# "stream" handle taps those tokens into the downstream `consumer`, which reads
# each `{ name, value }` as real DATA (not a log string).
#
# Each token's wire shape (built by the engine watcher) is:
#   { execution_id, category: "output",
#     detail: { event_type: "output_set", name: "chunk_0", value: {...} }, ... }
# so the consumer reads `input.detail.value`.
#
# Phase B: `set_output` now emits its OutputSet event PER CALL during execution
# (gated on the `output` stream-events category the compiler adds for
# stream_output steps), so each chunk reaches the consumer WHILE this step is
# still running — not only at job end (which raced net completion). The sleeps
# below space the chunks out so the consumer's downstream jobs run well before
# the producer finishes and the net reaches End.
#
# DISTINCT names matter: the stream token's dedup id is content-addressable per
# output name (`{exec}-output-{name}`), so re-using a name would be deduped.
#
# The control path is unchanged: `produced` / `count` are the node's declared
# output fields and ride the control token to End. Streaming is purely additive.

import time

from aithericon import set_output

# Structured per-chunk data on the stream side-channel — distinct names, spaced
# out so each emits mid-execution and the consumer drains before End.
chunks = [
    {"idx": 0, "text": "the"},
    {"idx": 1, "text": "quick"},
    {"idx": 2, "text": "brown"},
    {"idx": 3, "text": "fox"},
]
for c in chunks:
    set_output(f"chunk_{c['idx']}", c)
    time.sleep(1.0)

# Give the side-channel consumer jobs a head start before the control token
# reaches End and the net completes.
time.sleep(2.0)

# Final declared outputs on the control path — govern termination via End.
set_output("produced", True)
set_output("count", len(chunks))
