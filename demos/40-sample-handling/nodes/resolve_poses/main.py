"""Loop body head — resolve the CURRENT job's grasp/insert poses from assets.

The `run_job` array is staged ONCE by the upstream `load_jobs` step (parked
producer); this body indexes it by the Loop's built-in counter
`job_loop.iteration` (0-based). The Loop carries NO accumulators — the job list
is immutable, so there's nothing to fold; the body just reads the current record
directly. Running under a Loop (not a Map) means the jobs execute one-at-a-time,
so two jobs never send competing FollowJointTrajectory goals to the single arm
controller.

Two asset collections are bound and staged as injected globals (demo 21
pattern):

  * `racks`  — the `sample_rack` records: {slot_id, grasp_pose, approach_pose, ...}
  * `ports`  — the `instrument_ports` records: {port_id, insert_pose, retract_pose, ...}

COMPILER CONTRACT: this source is SCANNED (not executed) for `<slug>.<field>`
references. The literal `load_jobs.items` (the parked `run_job` array) and
`job_loop.iteration` (the Loop counter) reads below MUST stay verbatim so the
compiler stages BOTH parked envelopes as Python globals. Outputs are implicit:
the runner sweeps globals matching this step's declared output port (grasp_pose
/ grasp_approach / insert_pose / insert_approach / op / sample_id).
"""

# Current run_job row, indexed from the parked `load_jobs` array by the counter.
_job = load_jobs.items[job_loop.iteration]
from_slot = _job["from_slot"]
to_port = _job["to_port"]
op = _job["op"]
sample_id = _job["sample_id"]

# Injected asset-binding globals: whole record collections.
racks = racks      # list[dict] of sample_rack rows
ports = ports      # list[dict] of instrument_ports rows


def _find(rows, key, want):
    for r in rows:
        if r.get(key) == want:
            return r
    raise ValueError(f"no row with {key}=={want!r} in {[x.get(key) for x in rows]}")


rack = _find(racks, "slot_id", from_slot)
port = _find(ports, "port_id", to_port)

# Pose pairs as geometry_msgs/Pose JSON (already stored in that shape on the
# asset records — pass them through verbatim).
grasp_pose = rack["grasp_pose"]
grasp_approach = rack["approach_pose"]
insert_pose = port["insert_pose"]
insert_approach = port["retract_pose"]

log_info(
    "resolved job poses",
    iteration=job_loop.iteration,
    from_slot=from_slot,
    to_port=to_port,
    op=op,
    sample_id=sample_id,
)
