# Build Greeting — the smallest possible AutomatedStep.
#
# `input.name` is the Start field, which rides on the control token (Start
# fields are control-token-resident leaves, not parked envelopes).
#
# `greeting` is an implicit output: the runner sweeps any global that
# matches a name declared in this step's output port (`greeting` here) at
# the end of execution. No `set_output(...)` call is required.

greeting = f"Hello, {input.name}!"

log_info("greeted user", name=input.name)
