"""Loop body head — resolve the CURRENT transfer job's grasp/insert poses.

The `transfer_jobs` array is staged ONCE by the upstream `load_jobs` step (parked
producer); this body indexes it by the Loop's built-in counter
`job_loop.iteration` (0-based). The Loop carries NO accumulators — the job list
is immutable, so the body just reads the current record directly. Running under a
Loop (not a Map) means the jobs execute one-at-a-time, so two jobs never send
competing FollowJointTrajectory goals to the single arm controller.

UNIFIED LOCATION MODEL — the key to swapping a sample IN and OUT of the port.
A job's `from_loc` / `to_loc` is a location id that can name EITHER a rack slot
(A1/A2/A3, from the `sample_rack` asset) OR the instrument port (P1, from the
`instrument_ports` asset). `_loc_poses` resolves a location id to its (at, above)
pose pair regardless of which collection it lives in:

  * rack slot — at = grasp_pose,  above = approach_pose
  * port      — at = insert_pose, above = retract_pose

We emit the GRASP poses from `from_loc` (where the arm grabs the sample) and the
INSERT poses from `to_loc` (where it releases). A `pick` job uses the grasp pair;
a `place` job uses the insert pair. So `pick A1→P1` then `place A1→P1` carries a
sample from the rack into the port, and a later `pick P1→A1` + `place P1→A1`
returns it — the sample cycles in and out of the experiment.

COMPILER CONTRACT: this source is SCANNED (not executed) for `<slug>.<field>`
references. The literal `load_jobs.items` (the parked `transfer_jobs` array) and
`job_loop.iteration` (the Loop counter) reads below MUST stay verbatim so the
compiler stages BOTH parked envelopes as Python globals. Outputs are implicit:
the runner sweeps globals matching this step's declared output port (grasp_pose /
grasp_approach / insert_pose / insert_approach / op / sample_id).
"""

# Current transfer_job row, indexed from the parked `load_jobs` array by counter.
_job = load_jobs.items[job_loop.iteration]
from_loc = _job["from_loc"]
to_loc = _job["to_loc"]
op = _job["op"]
sample_id = _job["sample_id"]

# Injected asset-binding globals: whole record collections.
racks = racks      # list[dict] of sample_rack rows {slot_id, grasp_pose, approach_pose, ...}
ports = ports      # list[dict] of instrument_ports rows {port_id, insert_pose, retract_pose, ...}


def _loc_poses(loc_id):
    """Resolve a location id to its (at_pose, above_pose) pair — rack OR port."""
    for r in racks:
        if r.get("slot_id") == loc_id:
            return r["grasp_pose"], r["approach_pose"]
    for p in ports:
        if p.get("port_id") == loc_id:
            return p["insert_pose"], p["retract_pose"]
    raise ValueError(
        f"no rack slot or instrument port with id {loc_id!r} "
        f"(racks={[r.get('slot_id') for r in racks]}, ports={[p.get('port_id') for p in ports]})"
    )


# Grasp at `from_loc` (where we pick the sample up); insert at `to_loc` (where we
# set it down). Both pairs are emitted every job; the Decision routes to the op
# that uses the relevant pair.
grasp_pose, grasp_approach = _loc_poses(from_loc)
insert_pose, insert_approach = _loc_poses(to_loc)

log_info(
    "resolved transfer poses",
    iteration=job_loop.iteration,
    from_loc=from_loc,
    to_loc=to_loc,
    op=op,
    sample_id=sample_id,
)
