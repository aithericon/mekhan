# Fold — reduce the trajectory action's gathered feedback collection (docs/25).
#
# The `move` node's Control channel "feedback" is consumed by this edge with
# `join: gather`, so the gather barrier parks its gathered collection as the
# envelope `{ output: [<FollowJointTrajectory_Feedback>, ...] }`
# on the channel's gathered place. The graph wires that place (sourceHandle
# "feedback") straight into this node, so the gathered envelope IS this step's
# input token — read the list off `input.output`.
#
# The items are already in stream order (the gather barrier sorts by emit
# index). Count the distinct frames and surface the first one verbatim, proving a
# rich, nested ROS action-feedback frame survives the channel round-trip without
# depending on its inner field names. Replaces the retired in-net StreamFold.

from aithericon import set_output

feedbacks = input.output or []

set_output("feedback_count", len(feedbacks))
set_output("sample_frame", feedbacks[0] if feedbacks else {})
