# Joiner — concatenate the gathered collection (docs/25).
#
# The producer's Control channel "items" is consumed by this edge with
# `join: gather`, so the compiler's gather barrier parks the gathered collection
# as the envelope `{ output: [<word>, ...] }` on the channel's gathered place.
# The graph wires that place (sourceHandle "items") straight into this node, so
# the gathered envelope IS this step's input token — read the list off
# `input.output` (the runner exposes the inbound token as the `input` global).
#
# The items are already in stream order (the gather barrier sorts by emit
# index). Uppercase each word and concatenate into the final transcript —
# "THE QUICK BROWN FOX".

elems = input.output or []

transcript = " ".join(str(e).upper() for e in elems)
