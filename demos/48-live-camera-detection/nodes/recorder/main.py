"""Recorder — drain the detector's annotated feed and persist it as a single MP4
registered in the platform's file catalogue.

Why a separate `recording` channel rather than reading `annotated` directly?
`annotated` is a `transport: "livekit"` channel — a WebRTC EGRESS sink for the
live browser view. By design it cannot be consumed by another node (the
executor's LiveKit transport hard-errors on `subscribe`: a livekit channel is
presentation-only). So the detector mirrors the SAME annotated JPEG frames onto
a second, durable `recording` channel (transport `jetstream`: lossless, ordered,
replayable — exactly the contract a recorder needs, unlike the lossy live
`livekit`/`nats-latest` paths). The browser watches the WebRTC feed; this node
drains the durable mirror and produces a persisted artifact.

Each chunk is one JPEG (`image/jpeg`). We decode it, lazily open a
`cv2.VideoWriter` (mp4v) sized to the first frame, append every frame, finalize
the MP4 when the stream closes, then hand the file to
`log_artifact(..., blocking=True)`. Blocking matters: it waits for the upload +
catalogue registration to complete before this node returns, so the recording is
a guaranteed side effect of the run (the graph's AND-join at `converge` also
gates End on this branch, so the net is never torn down mid-write).
"""

import cv2
import numpy as np

from aithericon import log_artifact, log_info, set_output, stream

OUTPUT_PATH = "annotated-detection.mp4"
# Container playback frame rate. The true capture cadence is set by the `camera`
# step's `fps`; this only affects how fast the saved file plays back. A fixed
# value keeps the recorder self-contained (it consumes only the byte stream).
OUTPUT_FPS = 10.0

writer = None
frames_recorded = 0
bytes_recorded = 0
width = height = 0

# `recording` is a Data/In channel: this node fires on its `open`, then
# `stream(...)` yields one JPEG (bytes) per frame in order until `close`.
for chunk in stream("recording"):
    if not isinstance(chunk, (bytes, bytearray)):
        continue
    frame = cv2.imdecode(np.frombuffer(chunk, dtype=np.uint8), cv2.IMREAD_COLOR)
    if frame is None:
        continue
    bytes_recorded += len(chunk)
    if writer is None:
        height, width = frame.shape[:2]
        fourcc = cv2.VideoWriter_fourcc(*"mp4v")
        writer = cv2.VideoWriter(OUTPUT_PATH, fourcc, OUTPUT_FPS, (width, height))
        log_info(
            f"recording started — {width}x{height} @ {OUTPUT_FPS:g} fps → {OUTPUT_PATH}",
            resolution=f"{width}x{height}",
        )
    writer.write(frame)
    frames_recorded += 1
    if frames_recorded % 10 == 1:
        log_info(
            f"recorded {frames_recorded} frames ({bytes_recorded} B)",
            frames=frames_recorded,
            bytes=bytes_recorded,
        )

if writer is not None:
    writer.release()

resolution = f"{width}x{height}" if frames_recorded else ""

if frames_recorded:
    log_info(
        f"recording complete — {frames_recorded} frames ({resolution}, {bytes_recorded} B); "
        f"registering {OUTPUT_PATH!r} in the file catalogue",
        frames_recorded=frames_recorded,
    )
    # blocking=True: wait for the upload + catalogue registration before
    # returning, so the artifact is durable before the run completes.
    log_artifact(
        OUTPUT_PATH,
        name=OUTPUT_PATH,
        category="dataset",
        mime_type="video/mp4",
        metadata={
            "frames": str(frames_recorded),
            "resolution": resolution,
            "fps": str(OUTPUT_FPS),
            "source": "annotated YOLO26 detection feed",
        },
        extract_metadata=True,
        blocking=True,
    )
else:
    log_info("no frames received on `recording` channel — nothing to register")

set_output("frames_recorded", frames_recorded)
set_output("bytes_recorded", bytes_recorded)
set_output("artifact", OUTPUT_PATH if frames_recorded else "")
set_output("resolution", resolution)
