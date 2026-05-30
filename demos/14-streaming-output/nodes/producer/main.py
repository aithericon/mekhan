# Producer — streaming-output prototype.
#
# This AutomatedStep has `streamOutput: true` in graph.json, so the compiler
# synthesizes a Signal place `p_producer_stream` and stamps the job metadata
# `petri_event_log = p_producer_stream`. The engine then routes every
# EventCategory::Log event this step emits (the log_info() calls below) into
# that place as ONE token per event. An edge from this node's "stream" handle
# taps those tokens into the downstream `consumer` step.
#
# The control path is unchanged: `produced` / `count` are parked write-once on
# the normal output port and ride the control token to End. Streaming is a
# purely additive side-channel — completion is governed by the control "out"
# token, NOT by how many stream tokens are produced/consumed.

import time

from aithericon import log_info, set_output

# Default to 8 chunks; `start.chunks` is an optional Start field (kind:number,
# required:false) injected as a producer-namespaced global that rides the
# control token — same access form as demo 12b's `start.a/d/z`. Keep the count
# small — this is a prototype with no stream backpressure or end-of-stream
# sentinel.
n = int(start.chunks) if start.chunks is not None else 8

for i in range(n):
    # Each log_info() is an EventCategory::Log event → one token in
    # p_producer_stream → one consumer firing.
    log_info(f"chunk {i}", idx=str(i))
    time.sleep(0.05)

log_info("producer done", count=str(n))

# Final parked output on the control path — governs termination via End.
set_output("produced", True)
set_output("count", n)
