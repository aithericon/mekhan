# Producer — streaming-source Map demo (demo 16).
#
# `streamOutput: true` (graph.json) makes every `set_output(name, value)` call
# emit an `OutputSet { name, value }` event PER CALL, mid-execution, onto this
# node's stream side-channel (the `p_producer_stream` Signal place). The
# downstream streaming Map ingests each chunk as it arrives and dispatches a
# per-chunk Python body CONCURRENTLY (one ephemeral job per chunk).
#
# We emit one DISTINCT-named chunk per word, each value a plain string. The
# map body uppercases each chunk; the gather re-orders by stream sequence and
# the trailing joiner concatenates into "THE QUICK BROWN FOX".
#
# IMPORTANT: a streaming producer must emit ONLY stream chunks — every
# `set_output` is counted into `stream_count` (the end-of-stream N that sizes
# the Map's gather barrier). Do NOT also set a control output here.

import time

from aithericon import set_output

words = ["the", "quick", "brown", "fox"]
for i, w in enumerate(words):
    set_output(f"chunk_{i}", w)
    time.sleep(1.0)
