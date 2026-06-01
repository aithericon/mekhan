# GPU Render — one shot of a warm, leased GPU batch.
#
# This body runs INSIDE a `LeaseScope { Loop { ... } }`. Its deploymentModel is
# `Scheduled` with no scheduler of its own — by CONTAINMENT it lowers to the
# executor-enqueue path and runs on the allocation the enclosing LeaseScope is
# holding (docs/17). The compiler stamps the lease's `executor_namespace` onto
# the job token, so the single persistent drain executor srun'd/dispatched on
# acquire pulls every iteration's job WARM (venv/model/GPU stay hot) instead of
# paying a fresh queue + cold-start per shot — the whole point of leasing.
#
# Direct slug access: `lp.iteration` is the enclosing loop's counter (parked in
# p_lp_data). The compiler scans this source, synthesizes a read-arc into the
# loop's parked place, and promotes `lp` to a Python global — no token[...] or
# SDK init needed (same mechanism as demo 04). `input.job_name` is the Start
# field (a control-token-resident leaf).

import time

RENDER_SECONDS = 2

shot = lp.iteration
log_info("rendering shot on the held GPU lease", scene=input.job_name, shot=shot)
time.sleep(RENDER_SECONDS)

# Implicit outputs: the runner sweeps globals matching the declared output port
# fields (`shot`, `frame`) at the end of execution.
frame = f"{input.job_name}_shot{shot:03d}.exr"

log_info("shot rendered — staying warm for the next iteration", scene=input.job_name, frame=frame)
