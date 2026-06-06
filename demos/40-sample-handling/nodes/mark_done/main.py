"""Map body terminal — converge the op branches into one uniform result.

The `handle_jobs` Map gathers a single `resultVar` (`done`) per element, but the
Decision dispatches each job into one of three SubWorkflows whose result shapes
differ (`picked` / `placed` / `swapped`). This trivial step runs once per job,
after whichever branch fired, and emits a present, typed `done: true` so the
gather always finds the declared field regardless of the op.

Output is implicit: the runner sweeps module globals matching the declared
output port (`done`), the same convention `resolve_poses` uses.
"""

done = True
