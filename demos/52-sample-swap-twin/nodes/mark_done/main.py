"""Loop body terminal — converge the op branches into one uniform result.

The Decision dispatches each job into one of two SubWorkflows whose result
shapes differ (`picked` / `placed`). This trivial step runs once per job, after
whichever branch fired, and emits a present, typed `done: true` so the body has
one uniform terminal field regardless of the op.

Output is implicit: the runner sweeps module globals matching the declared
output port (`done`), the same convention `resolve_poses` uses.
"""

done = True
