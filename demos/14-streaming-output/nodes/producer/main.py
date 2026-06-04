# Producer — data-plane streaming demo (docs/25).
#
# The producer declares a Data/Out channel "words" (graph.json). Opening it with
# `open_output("words")` fires an `open` control token EARLY (the moment the
# context is entered, mid-job) carrying the transport descriptor; the downstream
# consumer wired off this handle starts draining immediately, while we still
# produce. Each `out.write(value)` publishes one out-of-band element envelope
# over the transport subject — the bulk bytes NEVER ride a net token, so the
# petri net sees only the open + the close (two firings total).
#
# We write one element per word, each a plain string (Any element kind →
# JSON-framed on the wire, decoded back to the string on the consumer side). The
# sleeps space the elements out so they genuinely stream over time. On context
# exit the writer publishes the in-band EOF terminator and fires the `close`
# control token stamping the element count (4) + status.
#
# Replaces the old `streamOutput`/`set_output(f"chunk_i")` model: no per-chunk
# token spam in the marking.

import time

from aithericon import open_output

words = ["the", "quick", "brown", "fox"]
with open_output("words") as out:
    for w in words:
        out.write(w)
        time.sleep(1.0)
