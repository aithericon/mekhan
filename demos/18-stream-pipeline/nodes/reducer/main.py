# Reducer — stage 3: fold the transform's stream into the final transcript (docs/25).
#
# Declares a Data/In channel "upper" wired from the transform's
# `open_output("upper")` handle (graph.json). `aithericon.stream("upper")` drains
# the transform's out-of-band byte stream (the already-uppercased elements) and
# we concatenate them in stream order. We do NO uppercasing here — the transform
# already did it — so the "THE QUICK BROWN FOX" result proves the transform stage
# ran. The transform's in-band EOF ends our loop; this single step owns the
# reduction (no net gather barrier).

from aithericon import set_output, stream

acc = [str(word) for word in stream("upper")]

transcript = " ".join(acc)
set_output("transcript", transcript)
