# Producer — stage 1 of the streaming pipeline (demo 18).
#
# `streamOutput: true` makes every `set_output(name, value)` call emit an
# OutputSet event PER CALL, mid-execution, onto this node's stream side-channel.
# We emit one distinct-named chunk per LOWERCASE word; the downstream transform
# receives each via aithericon.chunks(), uppercases it, and re-streams it.
#
# IMPORTANT: a streaming producer must emit ONLY stream chunks — every
# set_output is counted into stream_count (the end-of-stream N that sizes the
# transform's EOF). Do NOT set any control output here.

import time

from aithericon import set_output

words = ["the", "quick", "brown", "fox"]
for i, w in enumerate(words):
    set_output(f"chunk_{i}", w)
    time.sleep(1.0)
