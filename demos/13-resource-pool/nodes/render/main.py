# GPU Render — a mock render body.
#
# Seeded as a plain inline step (the demo seeder provisions templates, not
# resources). To turn it into the shared-admission showcase: create a
# `capacity` resource at /resources (the `limit` preset, seeded N), then set this step's deploymentModel to
# `{ mode: "executor", capacity: { alias: "<your-limit>" } }`. The compiler then wraps
# this body behind a capacity claim against the limit's backing net: the net does
# not start the body until a unit token is granted, and the grant is returned on
# every exit. From the body's point of view the unit is simply available —
# admission control and mutual exclusion are the Petri firing rule, not
# application code (see docs/14). The granted lease is staged as `lease.json`
# and parked, so body code can read `lease.unit_id`.
#
# `input.job_name` is the Start field (control-token-resident leaf). The
# ~12s sleep makes contention legible on the pool's dashboard once several
# instances contend for a small pool.

import time

HOLD_SECONDS = 12

log_info("render started — holding a pooled GPU", job=input.job_name)
time.sleep(HOLD_SECONDS)

# Implicit outputs: the runner sweeps globals matching the declared output
# port fields (`rendered`, `gpu`) at the end of execution.
rendered = f"rendered {input.job_name} in {HOLD_SECONDS}s"
gpu = "(assigned by pool)"

log_info("render complete — releasing GPU back to the pool", job=input.job_name)
