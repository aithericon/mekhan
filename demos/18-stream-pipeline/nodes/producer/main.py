# Producer — stage 1 of the streaming pipeline (demo 18, docs/25).
#
# Declares a Data/Out channel "lower" (graph.json). `open_output("lower")` fires
# an `open` control token EARLY so the downstream transform starts draining while
# this job still runs; each `out.write(value)` publishes one out-of-band element
# envelope over the transport. The bytes never ride a net token — the net sees
# only this stage's open + close.
#
# We write one element per LOWERCASE word; the transform stream-maps each
# (uppercase) and re-streams it on its own Data/Out channel. The sleeps space the
# elements out so they genuinely stream over time.

import time

from aithericon import open_output

words = ["the", "quick", "brown", "fox"]
with open_output("lower") as out:
    for w in words:
        out.write(w)
        time.sleep(1.0)
