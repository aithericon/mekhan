# Double the Score — trivial derived field for the downstream Decision.
#
# `input.score` rides on the control token (Start field). `doubled` is an
# implicit output: name-matched against this step's output port and swept
# from globals at the end of execution.

doubled = (input.score or 0) * 2

log_info("doubled score", raw=input.score, doubled=doubled)
