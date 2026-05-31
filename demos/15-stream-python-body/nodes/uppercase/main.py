# Per-chunk body — demo 15 (StreamConsumer dispatch=SequentialBody).
#
# The StreamConsumer dispatches ONE body run per drained stream chunk, one at a
# time in stream order (the sequential permit + next-expected-sequence gate). The
# consumer stamps the chunk value namespace-on-token under the consumer's
# `resultVar` (here `item`); the compiler scans this source, sees the bare `item`
# read, synthesizes a read-arc, and promotes `item` to a Python global (the same
# direct-slug-access mechanism AutomatedStep bodies use).
#
# We uppercase the chunk and publish it under the consumer's `resultVar` name
# (`item`) so `t_collect` can lift `body.detail.outputs.item` into the gather.
# The gather then Concat-joins the four uppercased chunks in stream order →
# "THE QUICK BROWN FOX".

from aithericon import set_output

# `input.item` is the per-chunk value injected by the StreamConsumer body dispatch.
# (Scalars on the inbound token are reachable via `input.<field>`).
chunk = input.item

set_output("item", str(chunk).upper())
