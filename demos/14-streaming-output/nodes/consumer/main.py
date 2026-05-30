# Consumer — the stream side-tap.
#
# Wired FROM the producer's "stream" handle, so this step fires once per
# streaming Log token tapped out of `p_producer_stream`. There is no per-token
# payload on the control token (the stream place carries a slim signal token),
# so the consumer's job here is simply to emit a DISTINCTIVE, countable marker
# that proves streaming consumption happened.
#
# This step's control token deliberately DANGLES (no End, no join): the net's
# termination is governed solely by the producer -> End control path, so a
# fast/slow producer-consumer mismatch can never wedge NetCompleted. This is a
# prototype: stream termination + backpressure are known open issues.

from aithericon import log_info, set_output

log_info("CONSUMED", marker="stream-tap")

set_output("consumed", True)
