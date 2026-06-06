"""Producer: synthesize a short moving-image clip, mux it as a FRAGMENTED MP4 /
H.264 stream with PyAV, and pace the fragments over the `video` data channel in
~real time.

This is the VIDEO sibling of demo 45 (fragmented MP4 / AAC audio) — the leg of
the streaming arc that makes the original goal ("load-bearing for video") real.
Same shape as 45, only the codec differs: an `ftyp` + `moov`(mvex) init segment
then `moof` + `mdat` H.264 media fragments. The channel's element content_type
is `video/mp4;codecs="avc1.42E01E"` (Constrained Baseline 3.0), so the UI render
registry (`planLiveRender`) returns `mediaKind: 'video'` and the panel appends
the stream into a `<video>` element via Media Source Extensions.

PyAV bundles libav with libx264, so no system ffmpeg is needed — `av` is just a
pip requirement. libav calls `.write()` on a non-seekable file-like as it muxes;
we capture each chunk in order, then replay them over the channel with a small
sleep so a live `?follow=1` tap can append-and-play WHILE we're still producing.
The bytes ride the out-of-band JetStream datastream; the net only ever sees the
channel's open + close (2 firings), never the frames.
"""

import io
import math
import time

import av
import numpy as np

import aithericon
from aithericon import open_output, set_output

WIDTH, HEIGHT = 320, 240
FPS = 30
SECONDS = 3
# Constrained Baseline 3.0 — the most broadly MSE-supported H.264 string.
CONTENT_TYPE = 'video/mp4;codecs="avc1.42E01E"'


class _CaptureWriter(io.RawIOBase):
    """A non-seekable sink libav writes muxed fragments into, in order."""

    def __init__(self):
        self.chunks = []

    def writable(self):
        return True

    def write(self, b):
        self.chunks.append(bytes(b))
        return len(b)


def _encode_fragmented_mp4():
    """Mux a synthesized clip into a fragmented MP4 byte stream; return chunks."""
    writer = _CaptureWriter()
    container = av.open(
        writer,
        mode="w",
        format="mp4",
        options={
            # Streamable init segment + media fragments; frag_keyframe cuts a
            # fresh moof at each keyframe (gop_size below ⇒ ~every 0.5 s) so a
            # tap can start decoding before the producer finishes.
            "movflags": "frag_keyframe+empty_moov+default_base_moof",
            "frag_duration": "500000",  # microseconds
        },
    )
    stream = container.add_stream("h264", rate=FPS)
    stream.width = WIDTH
    stream.height = HEIGHT
    stream.pix_fmt = "yuv420p"  # H.264 / browser-decodable chroma
    stream.options = {
        "profile": "baseline",
        "level": "3.0",
        "preset": "veryfast",
        "tune": "zerolatency",
    }
    stream.codec_context.gop_size = FPS // 2  # keyframe ~every 0.5 s

    xx = np.arange(WIDTH)
    yy = np.arange(HEIGHT)
    for i in range(FPS * SECONDS):
        t = i / FPS
        # A scrolling colour gradient with a bouncing white box — visibly
        # "real motion" so a human can confirm playback at a glance.
        img = np.zeros((HEIGHT, WIDTH, 3), dtype=np.uint8)
        img[:, :, 0] = ((xx[None, :] + int(t * 60)) % 256).astype(np.uint8)
        img[:, :, 2] = ((yy[:, None] + int(t * 30)) % 256).astype(np.uint8)
        bx = int((WIDTH - 40) * (0.5 + 0.5 * math.sin(t * 3)))
        by = int((HEIGHT - 40) * (0.5 + 0.5 * math.cos(t * 2)))
        img[by : by + 40, bx : bx + 40, :] = 255
        frame = av.VideoFrame.from_ndarray(img, format="rgb24").reformat(format="yuv420p")
        for pkt in stream.encode(frame):
            container.mux(pkt)
    for pkt in stream.encode(None):  # flush the encoder
        container.mux(pkt)
    container.close()
    return writer.chunks


chunks = _encode_fragmented_mp4()
# Pace the replay over ~the clip duration so the live tap is perceptibly live
# (and the validator drains a real stream).
pace = SECONDS / max(1, len(chunks))

bytes_written = 0
with open_output("video") as out:
    for chunk in chunks:
        out.write(chunk, content_type=CONTENT_TYPE)
        bytes_written += len(chunk)
        time.sleep(pace)

set_output("fragments_written", len(chunks))
set_output("bytes_written", bytes_written)
set_output("video_seconds", SECONDS)
