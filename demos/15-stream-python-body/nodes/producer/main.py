# Producer — Python-body streaming demo (demo 15, docs/25).
#
# Declares a Data/Out channel "words" (graph.json). `open_output("words")` fires
# an `open` control token EARLY carrying the transport descriptor, so the
# downstream reducer starts draining while this job still runs. Each
# `out.write(value)` publishes one out-of-band element envelope over the
# transport subject — the bytes never ride a net token. On context exit the EOF
# terminator + the `close` token (element count) are emitted.
#
# We write one element per word, each a plain string (Any element kind). The
# sleeps space the elements out so they genuinely stream over time. The reducer
# uppercases each element and concatenates them into "THE QUICK BROWN FOX".

import time

from aithericon import open_output

words = ["the", "quick", "brown", "fox"]
with open_output("words") as out:
    for w in words:
        out.write(w)
        time.sleep(1.0)
