"""Detector — drain the LIVE `frames` channel, run YOLO per frame as each arrives,
emit recognized objects on a control stream AND stream back a box-annotated feed.

The first node to run object detection on a TRUE live stream and close the loop
with a *viewable* result. `stream('frames')` yields one JPEG per frame off the
lossy `nats-latest` transport (late frames already dropped — latest wins); we
`cv2.imdecode` each and call `model.track(frame, persist=True)` so ByteTrack
keeps stable ids ACROSS the per-frame calls (the live-tracking pattern).

Three outputs at once:
  • `detections` (Control/Out) — one `{frame,label,confidence,bbox,track_id}`
    control token per recognized object, emitted the moment its frame is
    processed (open → item* → close). A consumer-edge `join: gather` folds these
    into the `summary` report. This is the durable, structured record.
  • `annotated` (Data/Out, image/jpeg, transport `livekit`) — the SAME frame
    with YOLO's boxes + labels + track ids drawn on it (`res.plot()`),
    JPEG-encoded and published as a WebRTC VP8 track. This is a presentation-only
    EGRESS sink: a browser viewer subscribes to the room directly, but no in-net
    node can consume it. So you WATCH the live processed video as the run goes.
  • `recording` (Data/Out, image/jpeg, transport `jetstream`) — the IDENTICAL
    annotated frames mirrored onto a durable, lossless, ordered datastream. The
    downstream `recorder` step drains this into an MP4 and registers it in the
    file catalogue. WebRTC can't be read back in-net, so this mirror is how the
    annotated video is persisted.

Pure-cv2 (ultralytics' bundled OpenCV) for decode + annotate + encode — no PyAV,
so no libav symbol clash with the encoder path. LIVE-ONLY: the first run
pip-installs ultralytics (PyTorch + OpenCV) and downloads yolo26n.pt; warm runs
are fast.
"""

import cv2
import numpy as np
import torch
from ultralytics import YOLO

import aithericon
from aithericon import log_info, open_output, set_output, stream

device = "mps" if torch.backends.mps.is_available() else "cpu"
model = YOLO("yolo26n.pt")  # auto-downloads weights on first use
log_info(f"detector ready — YOLO26 + ByteTrack on {device}", device=device)

emitted = 0
frames_processed = 0
annotated_bytes = 0

# Open all outputs together: the structured control stream, the viewable
# annotated WebRTC feed, AND a durable mirror of that same feed for the
# recorder. `annotated` (transport `livekit`) is a presentation-only egress
# sink — a browser viewer subscribes to it directly, but no in-net node can
# consume it. So the IDENTICAL annotated JPEG frames are also written to
# `recording` (transport `jetstream`: lossless, ordered, replayable), which the
# downstream `recorder` step drains into an MP4 and registers in the file
# catalogue. Encode once, write to both. Each `model.track(...)` call advances
# ByteTrack state.
with aithericon.out("detections") as det_chan, open_output(
    "annotated"
) as viz_chan, open_output("recording") as rec_chan:
    for frame_idx, chunk in enumerate(stream("frames")):
        if not isinstance(chunk, (bytes, bytearray)):
            continue
        frame = cv2.imdecode(np.frombuffer(chunk, dtype=np.uint8), cv2.IMREAD_COLOR)
        if frame is None:
            continue
        frames_processed += 1

        res = model.track(frame, persist=True, device=device, verbose=False)[0]

        # Annotated frame (boxes + labels + track ids drawn). We emit EVERY
        # processed frame, even object-free ones, so the feed is a continuous
        # video rather than only firing on detections. The single encoded JPEG
        # goes to BOTH the live WebRTC view and the durable recording mirror.
        ok, buf = cv2.imencode(".jpg", res.plot(), [cv2.IMWRITE_JPEG_QUALITY, 80])
        if ok:
            jpeg = buf.tobytes()
            viz_chan.write(jpeg, content_type="image/jpeg")
            rec_chan.write(jpeg, content_type="image/jpeg")
            annotated_bytes += len(jpeg)

        boxes = res.boxes
        n = 0 if boxes is None else len(boxes)
        frame_labels = {}
        if boxes is not None and n:
            ids = boxes.id.tolist() if boxes.id is not None else [None] * n
            for xyxy, conf, cls, tid in zip(
                boxes.xyxy.tolist(), boxes.conf.tolist(), boxes.cls.tolist(), ids
            ):
                label = model.names[int(cls)]
                det_chan.emit(
                    {
                        "frame": frame_idx,
                        "label": label,
                        "confidence": round(float(conf), 3),
                        "bbox": [round(float(v), 1) for v in xyxy],  # x1,y1,x2,y2
                        "track_id": int(tid) if tid is not None else None,
                    }
                )
                emitted += 1
                frame_labels[label] = frame_labels.get(label, 0) + 1

        # One structured log line per frame, AS it is detected — the live feed.
        tally = ", ".join(f"{k}×{v}" for k, v in sorted(frame_labels.items()))
        log_info(
            f"frame {frame_idx:>3} — {n} objects: {tally or '(none)'}",
            frame=frame_idx,
            objects=n,
            **{f"n_{k}": v for k, v in frame_labels.items()},
        )

log_info(
    f"live detection complete — {emitted} objects across {frames_processed} frames; "
    f"streamed {annotated_bytes} B of annotated feed",
    detections_emitted=emitted,
    frames_processed=frames_processed,
    device=device,
)

set_output("detections_emitted", emitted)
set_output("frames_processed", frames_processed)
set_output("device", device)
