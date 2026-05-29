# GPU Render — a mock render body that HOLDS a pooled GPU for its duration.
#
# This step declares `resourcePool` in the graph, so the compiler wraps it
# behind a capacity claim against the long-lived `resource-pool-net`: the net
# does not even start this body until a GPU token is granted from the pool,
# and the grant is returned on every exit. From the body's point of view the
# GPU is simply available — admission control and mutual exclusion are the
# Petri firing rule, not application code (see docs/14).
#
# `input.job_name` is the Start field (control-token-resident leaf). The
# ~12s sleep makes the 2-running / 2-waiting contention legible in the
# /nets/resource-pool dashboard while several instances contend.

import time

HOLD_SECONDS = 12

log_info("render started — holding a pooled GPU", job=input.job_name)
time.sleep(HOLD_SECONDS)

# Implicit outputs: the runner sweeps globals matching the declared output
# port fields (`rendered`, `gpu`) at the end of execution.
rendered = f"rendered {input.job_name} in {HOLD_SECONDS}s"
gpu = "(assigned by pool)"

log_info("render complete — releasing GPU back to the pool", job=input.job_name)
