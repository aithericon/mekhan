"""Detector — drain the `video` data channel, run streaming object detection
(YOLO11n + ByteTrack), and EMIT each recognized object as a control-plane stream.

This is the AI half of the demo and the first node in the whole streaming arc
that CONSUMES a data stream and TRANSFORMS it into a control stream:

    video bytes (data plane, JetStream)  →  YOLO per frame  →  detections (control plane)

`aithericon.stream("video")` drains the stream step's out-of-band fragmented-MP4
byte stream (it starts EARLY — the moment the producer's `open` descriptor
reaches this node). We buffer the fragments into a temp `.mp4` and hand it to
Ultralytics, which decodes frame-by-frame (its bundled OpenCV/ffmpeg) and runs
YOLO11n — the practical SOTA real-time detector — with ByteTrack for stable
per-object IDs across frames. Inference runs on the Apple-Silicon GPU via the
PyTorch MPS backend (falling back to CPU), comfortably real-time for this clip.

Instead of returning one blob of results, we re-emit each detected object the
moment its frame is processed, as a control-plane item on the OUT channel
`detections`. `aithericon.out("detections")` opens one bracketed episode —
open → item* → close(count) — and each `.emit(...)` fires one small
`{frame, t, label, confidence, bbox, track_id}` control token. The producer does
NOT fold; the consumer edge (`join: gather`) does, in the `summary` step.

LIVE-ONLY (run via the API, not `mekhan test`): the first run pip-installs
`ultralytics` (PyTorch + OpenCV, a few hundred MB) into the venv and downloads
the `yolo11n.pt` weights (~5 MB) — slow + network-dependent, exceeds the 60 s
test cap on a cold run. Warm runs are fast (venv + weights cached).
"""

import os
import tempfile

import torch
from ultralytics import YOLO

import aithericon
from aithericon import log_info, set_output, stream

FPS = 10  # the clip's frame rate, used to stamp each detection's timestamp

# Buffer the out-of-band byte stream into a temp container the decoder can open.
tmp = tempfile.NamedTemporaryFile(suffix=".mp4", delete=False)
try:
    tmp_bytes = 0
    for chunk in stream("video"):
        if isinstance(chunk, (bytes, bytearray)):
            tmp_bytes += tmp.write(chunk)
    tmp.flush()
    tmp.close()

    device = "mps" if torch.backends.mps.is_available() else "cpu"
    log_info(f"buffered {tmp_bytes} bytes of video; loading YOLO11n on {device}", device=device, video_bytes=tmp_bytes)
    model = YOLO("yolo11n.pt")  # auto-downloads the weights on first use

    emitted = 0
    frames_processed = 0
    # `stream=True` yields one Results object per frame (lazy); `persist=True`
    # keeps the tracker state so `box.id` is a stable track id across frames.
    # ByteTrack needs the `lap` solver, which is NOT a default ultralytics dep —
    # it is pinned in this node's `requirements` so it is pre-installed in the
    # venv (otherwise ultralytics tries to pip-install it at runtime and the
    # first run fails needing a restart).
    results = model.track(
        source=tmp.name,
        stream=True,
        persist=True,
        device=device,
        verbose=False,
    )
    with aithericon.out("detections") as out_chan:
        for frame_idx, res in enumerate(results):
            frames_processed += 1
            boxes = res.boxes
            if boxes is None:
                continue
            ids = boxes.id.tolist() if boxes.id is not None else [None] * len(boxes)
            frame_labels = {}  # per-frame tally for a watchable live log line
            for xyxy, conf, cls, tid in zip(
                boxes.xyxy.tolist(),
                boxes.conf.tolist(),
                boxes.cls.tolist(),
                ids,
            ):
                label = model.names[int(cls)]
                out_chan.emit(
                    {
                        "frame": frame_idx,
                        "t": round(frame_idx / FPS, 2),
                        "label": label,
                        "confidence": round(float(conf), 3),
                        "bbox": [round(float(v), 1) for v in xyxy],  # x1,y1,x2,y2
                        "track_id": int(tid) if tid is not None else None,
                    }
                )
                emitted += 1
                frame_labels[label] = frame_labels.get(label, 0) + 1
            # One structured log line per frame, emitted AS the frame is detected,
            # so the live execution log shows the detection stream in real time.
            tally = ", ".join(f"{k}×{v}" for k, v in sorted(frame_labels.items()))
            log_info(
                f"frame {frame_idx:>2} @ {frame_idx / FPS:4.1f}s — {len(boxes)} objects: {tally or '(none)'}",
                frame=frame_idx,
                t=round(frame_idx / FPS, 2),
                objects=len(boxes),
                **{f"n_{k}": v for k, v in frame_labels.items()},
            )
    # close(count) fires on clean block exit — the gather barrier sizes on it.
    log_info(
        f"detection complete — {emitted} objects across {frames_processed} frames",
        detections_emitted=emitted,
        frames_processed=frames_processed,
        device=device,
    )

    set_output("detections_emitted", emitted)
    set_output("frames_processed", frames_processed)
    set_output("device", device)
finally:
    try:
        os.unlink(tmp.name)
    except OSError:
        pass
