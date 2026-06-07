"""Stream the commanded xArm 6 joint trajectory as a live digital-twin feed.

This is the twin half of demo 33: a plain Python AutomatedStep (no ROS
requirement) that synthesizes the SAME single-waypoint trajectory the `move`
branch commands on the real (fake-hardware) arm — linearly interpolating each of
the 6 joints from the all-zeros home pose to [0, -0.3, -0.3, 0, 0.6, 0] over
~2.5 s at 30 Hz — and emits each interpolated joint state as one NDJSON line
onto the Data/Out channel `joints`.

The element content_type is `application/vnd.aithericon.joint-state+x-ndjson`, so
the UI render registry (planLiveRender) routes the stream to the robot-arm twin
renderer, which animates a live 3D xArm 6 on the graph edge WHILE the real arm
follows the identical path in the parallel `move` branch. The bulk joint stream
rides the out-of-band JetStream datastream transport — it NEVER enters the net
marking; the net sees only the channel's open + close (2 firings for the whole
stream, regardless of frame count).
"""

import json
import time

from aithericon import open_output, set_output

JOINT_NAMES = ["joint1", "joint2", "joint3", "joint4", "joint5", "joint6"]
HOME = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
TARGET = [0.0, -0.3, -0.3, 0.0, 0.6, 0.0]

HZ = 30
SWEEP_S = 2.5  # one home→target (or target→home) leg; the first leg matches `move`
SWEEPS = 8  # home→target→home… — ~20 s of visible motion (comfortable to watch live)
LEG_FRAMES = int(round(HZ * SWEEP_S))  # ~75 frames per leg
CONTENT_TYPE = "application/vnd.aithericon.joint-state+x-ndjson;model=xarm6"


def _lerp(a, b, t):
    return a + (b - a) * t


frames_written = 0
with open_output("joints") as ch:
    for leg in range(SWEEPS):
        # Alternate direction each leg so the arm sweeps out, then back, repeatedly.
        a, b = (HOME, TARGET) if leg % 2 == 0 else (TARGET, HOME)
        for i in range(LEG_FRAMES + 1):  # include both endpoints
            t = i / LEG_FRAMES if LEG_FRAMES else 1.0
            positions = [round(_lerp(p, q, t), 6) for p, q in zip(a, b)]
            line = json.dumps({"joint_names": JOINT_NAMES, "positions": positions}) + "\n"
            ch.write(line.encode("utf-8"), content_type=CONTENT_TYPE)
            frames_written += 1
            time.sleep(1.0 / HZ)

set_output("frames", frames_written)
