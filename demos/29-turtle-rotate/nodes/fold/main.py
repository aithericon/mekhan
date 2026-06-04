# Fold — reduce the rotate action's gathered feedback collection (docs/25).
#
# The `rotate` node's Control/Scatter channel "feedback" parks its gathered
# collection as the envelope `{ output: [{ remaining }, ...] }` on the channel's
# gathered place. The graph wires that place (sourceHandle "feedback") straight
# into this node, so the gathered envelope IS this step's input token — read the
# list off `input.output` (the runner exposes the inbound token as `input`).
#
# The items are already in stream order (the gather barrier sorts by emit
# index). Count the distinct feedbacks and surface the first `remaining`. This is
# the fold the retired in-net StreamFold node used to do — it was never a net
# concern, just a reducer job (docs/25 §2).

from aithericon import set_output

feedbacks = input.output or []


def _remaining(frame):
    if isinstance(frame, dict):
        return frame.get("remaining", 0.0)
    return getattr(frame, "remaining", 0.0)


set_output("feedback_count", len(feedbacks))
set_output("rotation_delta", _remaining(feedbacks[0]) if feedbacks else 0.0)
