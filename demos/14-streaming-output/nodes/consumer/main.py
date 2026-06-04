# Consumer — data-plane fold demo (docs/25).
#
# The consumer declares a Data/In channel "words" wired from the producer's
# `open_output("words")` handle (graph.json). `aithericon.stream("words")` drains
# the producer's out-of-band byte stream — it starts as soon as the producer's
# `open` descriptor reaches this node (EARLY, independent of the producer job
# finishing) and yields one decoded element per envelope until the in-band EOF.
#
# The channel's element kind is `any`, so each element decodes JSON → the
# original string. We fold (concatenate in stream order) into the full sentence
# "the quick brown fox" and set it as the transcript output.
#
# This is the fold the retired StreamFold node used to do in-net: it was never a
# net concern, just a reducer job (docs/25 §2).

from aithericon import set_output, stream

acc = [str(w) for w in stream("words")]

transcript = " ".join(acc)
set_output("transcript", transcript)
