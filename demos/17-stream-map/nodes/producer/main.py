# Producer — scatter the words onto a Control/Out channel (docs/25).
#
# The producer declares a Control channel "items" with contract=Scatter and
# max_fanout=8 (graph.json). `scatter("items")` opens a fan-out: every
# `s.emit(value)` fires one instance-colored `scatter_item` control token into
# the channel; closing the context fires a `scatter_close` carrying the total
# item count. The compiler's per-channel gather barrier (sized on that count)
# re-orders the items by emit index and parks the gathered collection as
# `{ output: [<word>, ...] }` on the channel's gathered place. The downstream
# edge (sourceHandle "items") feeds that collection straight into the joiner.
#
# Replaces the old `streamOutput`/`set_output(f"chunk_i")` model: no per-chunk
# token spam in the marking, just one scatter open + N emits + one close, each a
# control emission off-band of the net's firing.

from aithericon import scatter

words = ["the", "quick", "brown", "fox"]
with scatter("items") as s:
    for w in words:
        s.emit(w)
