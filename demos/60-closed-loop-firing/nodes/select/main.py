"""Join the reviewer's verification picks with the curve parameters.

The 59-firing-curve-bo child returns two index-aligned arrays:

  - `campaign.verification_queue` — the review Repeater's per-row answers,
    `[{fire_and_verify: bool, note: str}, ...]` in top-curves order;
  - `campaign.top_curves` — the curve parameter dicts the Repeater iterated.

This step zips them, keeps the curves the reviewer flagged for physical
verification, and decides whether the robot leg runs:

  dispatch = (review decision == "approve")
       AND at least one curve picked
       AND the `robot` start field enables it ("auto"/"yes"/"true"/"1";
           default/empty = "skip" so the workflow completes everywhere —
           the robot leg needs the ROS/Isaac fleet up).

Literal slug reads below (`campaign.*`, `start.robot`) are the compiler's
read-arc anchors — keep them verbatim.
"""

queue = campaign.verification_queue
curves = campaign.top_curves
decision = campaign.decision
try:
    robot_mode = start.robot  # literal read — the compiler's read-arc anchor
except AttributeError:
    # Optional Start field omitted from the token (untouched Run form):
    # the staged dict raises on missing attributes, unlike Rhai's unit value.
    robot_mode = ""

rows = list(queue) if isinstance(queue, list) else []
params = list(curves) if isinstance(curves, list) else []

queued = []
for i, row in enumerate(rows):
    if not isinstance(row, dict) or not row.get("fire_and_verify"):
        continue
    curve = params[i] if i < len(params) and isinstance(params[i], dict) else {}
    queued.append({**curve, "note": str(row.get("note", ""))})

n_queued = len(queued)

robot_on = str(robot_mode).strip().lower() in ("auto", "yes", "true", "1", "robot")
approved = str(decision).strip().lower() == "approve"

dispatch = bool(approved and n_queued > 0 and robot_on)

if dispatch:
    skip_reason = ""
elif not approved:
    skip_reason = f"review decision was '{decision}', not approve"
elif n_queued == 0:
    skip_reason = "reviewer queued no curves for physical verification"
else:
    skip_reason = "robot leg disabled (start field `robot` != auto)"

log_info(
    f"select: decision={decision} queued={n_queued}/{len(rows)} "
    f"robot_mode={robot_mode!r} -> dispatch={dispatch} {skip_reason}"
)
