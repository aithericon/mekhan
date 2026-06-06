"""Camera — capture a LIVE feed frame-by-frame and stream each frame as one
JPEG over the `frames` Data/Out channel (lossy `nats-latest` transport).

This is the producer half of the live-viz loop. Unlike demo 47 (a bundled file
re-muxed to fragmented MP4), the source here is whatever `cv2.VideoCapture`
accepts — a webcam index ("0"), an RTSP/HTTP camera URL, or a file path — which
is the source-agnostic thesis made concrete: the SAME downstream detector
workflow runs against a laptop webcam, an IP camera, or (later) a Gazebo / Isaac
simulated robot camera. Only this node's `source` string changes; nothing
downstream moves.

Each captured frame is JPEG-encoded and written as ONE self-contained binary
chunk. Because the channel's transport is `nats-latest` (lossy-latest core NATS,
NOT durable JetStream), if the detector falls behind the OLD frames are dropped
whole and the latest wins — exactly camera semantics, and lossless per frame: a
JPEG is independently decodable, unlike a dropped fragmented-MP4 fragment which
would corrupt the stream. The net never sees the frames — only the channel's
open + close (2 firings for the whole feed).

A workflow run is bounded, so we capture a `duration_s` window of the live feed
at a target `fps`. LIVE-ONLY when sourced from a webcam: macOS gates camera
access behind a TCC permission prompt attributed to the terminal/app that
launched the dev stack — grant it (System Settings → Privacy & Security →
Camera), or point `source` at an RTSP/HTTP URL or a local video file path.
"""

import time

import cv2

import aithericon
from aithericon import log_info, open_output, set_output

TARGET_WIDTH = 640  # downscale wide frames so detection stays comfortably real-time


# Read the Start borrows with LITERAL `start.<field>` access. This is
# load-bearing, not stylistic: the compiler's borrow planner statically scans
# the node source for `start.<field>` patterns (`compiler/python_refs.rs`) to
# synthesize the read-arc that stages the Start token into this node and exposes
# `start` as a Python global. A dynamic `getattr(start, name)` — or a reference
# buried in an f-string or behind a call/subscript — is invisible to that scan,
# so the borrow is never wired and every field reads as absent. (An earlier
# version did exactly that and silently ignored the caller's source/fps/
# duration_s, always using the defaults below — i.e. the webcam at 8 s / 6 fps.)
#
# `source` is a cv2.VideoCapture target:
#   "0" → first webcam (macOS AVFoundation) · "rtsp://…"/"http://…" → IP camera
#   "/path/clip.mp4" → a local video file (deterministic, no camera/TCC).
source = str(start.source).strip()  # noqa: F821 — runner-injected Start borrow
try:
    duration_s = float(start.duration_s)  # noqa: F821
except (TypeError, ValueError, NameError, AttributeError):
    duration_s = 8.0
try:
    fps = float(start.fps)  # noqa: F821
except (TypeError, ValueError, NameError, AttributeError):
    fps = 6.0
interval = 1.0 / max(1.0, fps)

cap = cv2.VideoCapture(int(source) if source.isdigit() else source)
if not cap.isOpened():
    raise RuntimeError(
        f"could not open camera source {source!r}. If this is a webcam index on "
        "macOS, grant Camera permission to the terminal/app that launched the dev "
        "stack (System Settings → Privacy & Security → Camera); otherwise point "
        "`source` at an RTSP/HTTP camera URL or a local video file path."
    )

log_info(
    f"capturing {duration_s:g}s @ ~{fps:g} fps from source {source!r}",
    source=source,
    fps=fps,
    duration_s=duration_s,
)

frames_sent = 0
bytes_written = 0
last_dims = ""
started = time.monotonic()
next_tick = started
with open_output("frames") as out_chan:
    while time.monotonic() - started < duration_s:
        ok, frame = cap.read()
        if not ok:
            break  # file ended (or a transient camera hiccup) — stop cleanly
        height, width = frame.shape[:2]
        if width > TARGET_WIDTH:
            scale = TARGET_WIDTH / width
            frame = cv2.resize(frame, (TARGET_WIDTH, int(height * scale)))
            height, width = frame.shape[:2]
        ok, buf = cv2.imencode(".jpg", frame, [cv2.IMWRITE_JPEG_QUALITY, 80])
        if not ok:
            continue
        jpeg = buf.tobytes()
        out_chan.write(jpeg, content_type="image/jpeg")
        frames_sent += 1
        bytes_written += len(jpeg)
        last_dims = f"{width}x{height}"
        if frames_sent % 5 == 1:
            log_info(
                f"streamed frame {frames_sent} ({len(jpeg)} B, {last_dims}) over nats-latest",
                frame=frames_sent,
                bytes=len(jpeg),
                resolution=last_dims,
            )
        # Pace to the target fps off a fixed schedule so jitter doesn't accumulate.
        next_tick += interval
        sleep = next_tick - time.monotonic()
        if sleep > 0:
            time.sleep(sleep)
cap.release()

log_info(
    f"capture complete — streamed {frames_sent} frames ({bytes_written} B) from {source!r}",
    frames_sent=frames_sent,
    bytes_written=bytes_written,
    source=source,
)

set_output("frames_sent", frames_sent)
set_output("bytes_written", bytes_written)
set_output("source", source)
set_output("fps", fps)
set_output("resolution", last_dims)
