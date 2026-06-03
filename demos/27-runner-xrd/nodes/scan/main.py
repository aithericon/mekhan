# Diffraction Scan — a capability-matched, presence-pooled AutomatedStep.
#
# This body runs only on a `lab_fleet` runner whose advertised caps satisfy the
# step's placement Requirement (`xrd.max_2theta >= 120`). The match happens in
# the presence pool's `t_grant` guard before this code is ever dispatched, so by
# the time we run we are guaranteed to be on a capable diffractometer.
# `summary` is an implicit output (swept from the global by name).

summary = f"diffraction scan of {input.sample} complete on a capable XRD runner"

log_info("xrd scan ran on capable runner", sample=input.sample)
