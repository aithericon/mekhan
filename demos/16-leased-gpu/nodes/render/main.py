# GPU Render — one shot of a warm, leased GPU batch.
#
# This body runs INSIDE a `LeaseScope { Loop { ... } }`. Its deploymentModel is
# `Scheduled` with no scheduler of its own — by CONTAINMENT it lowers to the
# executor-enqueue path and runs on the allocation the enclosing LeaseScope is
# holding (docs/17). The compiler stamps the lease's `executor_namespace` onto
# the job token, so the single persistent drain executor dispatched on acquire
# pulls every iteration's job WARM (venv/model/GPU stay hot) instead of paying a
# fresh queue + cold-start per shot — the whole point of leasing.
#
# Direct slug access: `lp.iteration` is the enclosing loop's counter (parked in
# p_lp_data). The compiler scans this source, synthesizes a read-arc into the
# loop's parked place, and promotes `lp` to a Python global (same mechanism as
# demo 04). It survives EVERY iteration because it is read through that read-arc.
#
# NOTE: don't read Start fields (e.g. `input.job_name`) inside a loop body —
# they ride the control token and are only present on the FIRST iteration; the
# loop's continue carries only the slim token forward. The scene name is shown
# instead via the Start node's `processName` ("Render {{ job_name }}").

import time

RENDER_SECONDS = 2

shot = lp.iteration
log_info("rendering shot on the held GPU lease", shot=shot)
time.sleep(RENDER_SECONDS)

# Implicit outputs: the runner sweeps globals matching the declared output port
# fields (`shot`, `frame`) at the end of execution.
frame = f"shot{shot:03d}.exr"

log_info("shot rendered — staying warm for the next iteration", shot=shot, frame=frame)
