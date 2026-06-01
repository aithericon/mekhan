# Reducer — demo 15 (AutomatedStep with streamInput=true).
#
# A streamInput AutomatedStep IS the reducer: it is seeded at net entry (starts
# immediately), receives the upstream producer's chunks via `aithericon.chunks()`
# over the IPC sidecar, does its own reduction (uppercase + concatenate), and
# emits the final result. No container node, no Petri-net gather barrier — this
# single step owns the reduction.
#
# `aithericon.chunks()` is a generator that yields chunk values as they arrive.
# It terminates when the producer's stream ends (the EOF sentinel, sent when the
# producer's control token reaches this node's `in`).

from aithericon import chunks, set_output

acc = []
for chunk in chunks():
    acc.append(str(chunk).upper())

transcript = " ".join(acc)
set_output("transcript", transcript)
