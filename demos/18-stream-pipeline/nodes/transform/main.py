# Transform — stage 2: a streaming transform, BOTH reader and writer (docs/25).
#
# This step declares a Data/In channel "lower" AND a Data/Out channel "upper"
# (graph.json), so it is simultaneously a stream consumer and producer:
#   - IN: `aithericon.stream("lower")` drains the producer's byte stream as
#     elements arrive (starting EARLY, when the producer's `open` reaches us).
#   - OUT: `open_output("upper")` opens our own downstream stream; each
#     `out.write(...)` re-emits a mapped element the reducer drains.
#
# So the transform folds NOTHING — it maps each element (uppercase) and streams
# the result straight through. Wrapping the read loop inside the `open_output`
# context means our `open` fires up front (reducer starts immediately) and our
# `close` fires once the producer's stream ends and we have re-emitted every
# element.

from aithericon import open_output, stream

with open_output("upper") as out:
    for word in stream("lower"):
        out.write(str(word).upper())
