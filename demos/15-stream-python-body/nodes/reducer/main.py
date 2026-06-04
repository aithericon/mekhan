# Reducer — demo 15 (the AutomatedStep body IS the reducer, docs/25).
#
# This step declares a Data/In channel "words" wired from the producer's
# `open_output("words")` handle (graph.json). `aithericon.stream("words")` drains
# the producer's out-of-band byte stream as elements arrive — it starts the
# moment the producer's `open` descriptor reaches this node (EARLY, independent of
# the producer finishing) and yields one decoded element per envelope until the
# in-band EOF. The Python body does its own reduction (uppercase + concatenate)
# and emits the final result. No container node, no Petri-net gather barrier —
# this single step owns the reduction (docs/25 §2 retires net-native Fold).

from aithericon import set_output, stream

acc = [str(w).upper() for w in stream("words")]

transcript = " ".join(acc)
set_output("transcript", transcript)
