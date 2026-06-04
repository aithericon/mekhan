"""Map body — resolve the per-job grasp/insert poses from curated assets.

This step runs once per `run_job` element. Two asset collections are bound and
staged as injected globals (mirroring demo 21's `materials`):

  * `racks`  — the `sample_rack` records: list of {slot_id, grasp_pose,
               approach_pose, occupied}
  * `ports`  — the `instrument_ports` records: list of {port_id, instrument,
               insert_pose, retract_pose}

The job itself rides the token as the Map's itemVar `job` (no read-arc, no SDK
init — same as demo 12's `cand`). We read `job.from_slot` / `job.to_port`,
look up the matching rack slot + instrument port, and emit the four poses as
geometry_msgs/Pose JSON objects so they splice straight into the child
SubWorkflows' plan targets via the bare-placeholder whole-object path.

Outputs are implicit: the runner sweeps globals matching this step's declared
output port at the end of execution (same mechanism demo 21 relies on). We
assign plain dicts to `grasp_pose` / `grasp_approach` / `insert_pose` /
`insert_approach`.
"""

# itemVar (token-resident): the current run_job row.
from_slot = job.from_slot
to_port = job.to_port

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
    from_slot=from_slot,
    to_port=to_port,
    op=getattr(job, "op", None),
)
