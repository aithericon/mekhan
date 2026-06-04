# Load the bound `lab_cell` asset collection into a token-resident array so a
# Map can scatter it. assetBindings stage the whole record collection as the
# `cells` global (list[dict]); we re-emit it as the node's declared outputs via
# the SDK. `items` feeds the Map's itemsRef; `count` is the data-driven object
# count the End reports (the Map-gather projection isn't a Rhai value you can
# .len(), so the count rides this output instead).
#
# Each item also carries a pre-wrapped `pose_stamped` (a geometry_msgs/PoseStamped
# = {header:{frame_id}, pose:<Pose>}). add_object's `pose` field references it as a
# TOP-LEVEL bare placeholder `{{ item.pose_stamped }}` so the ROS backend's
# whole-object re-parse adopts the typed object — a placeholder NESTED inside a
# literal object (e.g. {header, pose:"{{ item.pose }}"}) is NOT re-parsed and
# stringifies to "[object]", so the wrap has to happen here, not in the graph.
import json

from aithericon import set_output

cells_in = cells              # injected asset global: list[dict] of lab_cell records
# pose_stamped is a JSON *string* carrier, not a live object: Tera stringifies a
# live object to the literal "[object]", but a bare-placeholder field whose value
# is a JSON string is re-parsed by the ROS backend into the typed object (the same
# carrier trick demo 33 uses for `trajectory`). Arrays (dimensions) render as
# valid JSON directly, so only the object needs the string carrier.
items = [
    {
        **c,
        "pose_stamped": json.dumps(
            {"header": {"frame_id": "link_base"}, "pose": c["pose"]}
        ),
    }
    for c in cells_in
]
set_output("items", items)
set_output("count", len(cells_in))
