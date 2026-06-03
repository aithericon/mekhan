# Measure (on lab fleet) — a minimal presence-pooled AutomatedStep.
#
# This body runs only after the `lab_fleet` presence pool GRANTS a unit, i.e.
# once at least one enrolled runner is live (heartbeating `runner.{id}.presence`).
# `input.sample` is the Start field (control-token-resident). `result` is an
# implicit output: the runner sweeps any global matching a declared output-port
# name at the end of execution, so no `set_output(...)` call is needed.

result = f"measured {input.sample} on the lab fleet"

log_info("ran on lab fleet runner", sample=input.sample)
