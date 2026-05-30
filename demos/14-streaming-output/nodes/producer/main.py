# Producer — streaming-output demo.
#
# `streamOutput: true` (graph.json) makes every `set_output(name, value)` call
# emit an `OutputSet { name, value }` event PER CALL, mid-execution, onto this
# node's stream side-channel (the `p_producer_stream` Signal place). A downstream
# StreamConsumer wired from the "stream" handle drains each chunk as it arrives.
#
# We emit one DISTINCT-named chunk per word, each value a plain string (distinct
# names matter: the stream token dedup id is content-addressable per output
# name). The consumer's Concat reduce joins them in stream order into the full
# sentence "the quick brown fox".
#
# IMPORTANT: a streaming producer must emit ONLY stream chunks — every
# `set_output` becomes a stream token and is counted into `stream_count` (the
# end-of-stream N). Do NOT also set a `produced`/`count` control output here, or
# it inflates N and pollutes the consumer's reduction.
#
# At job end the executor stamps `stream_count` (= 4 here) on the terminal
# Completed detail; it rides the producer's control token to the consumer's
# "control" handle, where it sizes the end-of-stream gather barrier. The sleeps
# space the chunks out so they stream over time.

import time

from aithericon import set_output

words = ["the", "quick", "brown", "fox"]
for i, w in enumerate(words):
    set_output(f"chunk_{i}", w)
    time.sleep(1.0)
