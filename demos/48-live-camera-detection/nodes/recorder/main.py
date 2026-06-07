"""Recorder — drain the detector's annotated feed and persist it as a single
**H.264** MP4 registered in the platform's file catalogue.

The detector's `annotated` Data/Out channel is a durable `jetstream` datastream
(lossless, ordered, replayable). It has two independent consumers off the one
stream: the instance view taps it for the live MJPEG feed, and THIS node drains
it for the recording. JetStream's Limits retention (messages kept by age/size,
not deleted on ack) is what lets both read it without stepping on each other.

Each chunk is one JPEG (`image/jpeg`). We decode it (Pillow), lazily open a PyAV
H.264 encoder sized to the first frame, append every frame, finalize the MP4
when the stream closes, then hand the file to `log_artifact(..., blocking=True)`.

Why H.264 / PyAV and not `cv2.VideoWriter`? OpenCV's `mp4v` fourcc writes
MPEG-4 Part 2, which **no browser decodes** in a `<video>` element — so the
recording was downloadable but not viewable inline. PyAV bundles libav + libx264
(no system ffmpeg needed), so we encode Constrained-Baseline H.264 (`avc1`) with
`faststart`, which the instance artifact viewer plays directly. We deliberately
do NOT import cv2 here: cv2 and PyAV both vendor libav and clash on shared
symbols in one process, so JPEG decode goes through Pillow (libjpeg) instead.

Blocking matters: `log_artifact(..., blocking=True)` waits for the upload +
catalogue registration before this node returns, so the recording is a
guaranteed side effect of the run (the graph's AND-join at `converge` also gates
End on this branch, so the net is never torn down mid-write).
"""

import io

import av
import numpy as np
from PIL import Image

from aithericon import log_artifact, log_info, set_output, stream

OUTPUT_PATH = "annotated-detection.mp4"
# Container playback frame rate. The true capture cadence is set by the `camera`
# step's `fps`; this only affects how fast the saved file plays back. A fixed
# value keeps the recorder self-contained (it consumes only the byte stream).
OUTPUT_FPS = 10

container = None
vstream = None
frames_recorded = 0
bytes_recorded = 0
width = height = 0


def _even(n: int) -> int:
    # H.264 / yuv420p requires even dimensions.
    return n - (n % 2)


# `annotated` is a Data/In channel: this node fires on its `open`, then
# `stream(...)` yields one JPEG (bytes) per frame in order until `close`.
for chunk in stream("annotated"):
    if not isinstance(chunk, (bytes, bytearray)):
        continue
    try:
        img = Image.open(io.BytesIO(chunk)).convert("RGB")
    except Exception:
        continue
    bytes_recorded += len(chunk)
    if container is None:
        width, height = _even(img.width), _even(img.height)
        container = av.open(
            OUTPUT_PATH, mode="w", format="mp4", options={"movflags": "faststart"}
        )
        vstream = container.add_stream("h264", rate=OUTPUT_FPS)
        vstream.width = width
        vstream.height = height
        vstream.pix_fmt = "yuv420p"  # browser-decodable chroma
        vstream.options = {
            "profile": "baseline",
            "level": "3.0",
            "preset": "veryfast",
            "tune": "zerolatency",
        }
        log_info(
            f"recording started — {width}x{height} @ {OUTPUT_FPS} fps H.264 → {OUTPUT_PATH}",
            resolution=f"{width}x{height}",
        )
    # Lock every frame to the first frame's (even) size so the encoder gets a
    # consistent geometry.
    if (img.width, img.height) != (width, height):
        img = img.resize((width, height))
    arr = np.ascontiguousarray(np.asarray(img, dtype=np.uint8))
    frame = av.VideoFrame.from_ndarray(arr, format="rgb24").reformat(format="yuv420p")
    for pkt in vstream.encode(frame):
        container.mux(pkt)
    frames_recorded += 1
    if frames_recorded % 10 == 1:
        log_info(
            f"recorded {frames_recorded} frames ({bytes_recorded} B)",
            frames=frames_recorded,
            bytes=bytes_recorded,
        )

if container is not None:
    for pkt in vstream.encode(None):  # flush the encoder
        container.mux(pkt)
    container.close()

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
            "codec": "h264",
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
