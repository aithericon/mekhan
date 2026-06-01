# Transform — stage 2: a streaming map that is BOTH consumer and producer.
#
# This AutomatedStep has streamInput=true AND streamOutput=true:
#   - streamInput: it is seeded at net entry (starts immediately) and receives
#     the producer's chunks as they arrive over IPC via aithericon.chunks().
#   - streamOutput: each set_output(...) call re-emits a chunk on THIS node's
#     own stream side-channel, mid-execution, which the downstream reducer
#     consumes via its own aithericon.chunks().
#
# So the transform folds NOTHING — it maps each chunk (uppercase here) and
# streams the result straight through. The producer's completion (producer.out →
# this node's `in`) is the EOF that ends our chunks() loop; our own completion
# (this node's `out`, carrying our stream_count) is the EOF for the reducer.

from aithericon import chunks, set_output

for i, chunk in enumerate(chunks()):
    set_output(f"upper_{i}", str(chunk).upper())
