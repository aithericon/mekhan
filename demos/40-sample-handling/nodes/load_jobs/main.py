# Loader head for the sequential job Loop. assetBindings stage the whole
# `run_job` record collection as the `jobs` global (list[dict]); we re-emit it
# as `items` (the array the Loop iterates ONE record at a time) and `count`
# (the record count the loop condition + End report). Mirrors demo 41's
# load_cells loader — but here the consumer is a sequential Loop, not a Map, so
# the jobs never drive the single arm concurrently (a concurrent Map fan-out
# made two jobs race the one /xarm6_traj_controller and preempt each other).
from aithericon import set_output

jobs_in = jobs                # injected asset global: list[dict] of run_job rows
set_output("items", jobs_in)
set_output("count", len(jobs_in))
