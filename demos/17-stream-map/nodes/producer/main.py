# Producer — emit the words onto a Control/Out channel (docs/25).
#
# The producer declares a Control channel "items" (graph.json). It emits ONE
# uniform bracketed episode — open → N items → close — and does NOT decide how
# that episode folds. The CONSUMER edge
# (sourceHandle "items") carries `join: gather`, so the compiler synthesizes a
# per-channel gather barrier (sized on the close count) that re-orders the items
# by emit index and parks the gathered collection as `{ output: [<word>, ...] }`
# on the channel's gathered place. The downstream edge feeds that collection
# straight into the joiner.
#
# `out("items")` opens the episode: every `ch.emit(value)` fires one
# instance-colored `item` control token; closing the context fires a `close`
# carrying the total item count.
#
# Replaces the old `streamOutput`/`set_output(f"chunk_i")` model: no per-chunk
# token spam in the marking, just one open + N items + one close, each a control
# emission off-band of the net's firing.

import aithericon

words = ["the", "quick", "brown", "fox"]
with aithericon.out("items") as ch:
    for w in words:
        ch.emit(w)
