"""Stream the source clip as a fragmented-MP4 video over the `video` data channel.

The clip is a real uploaded file: the Start `video` field is `kind: file`, so
`start.video` is a `FileRef` ({key, url, filename, content_type, size}). We
broker the bytes from the object store to a local path through the sidecar
(`aithericon.file(...).retrieve()` — the child holds no storage creds), decode
the frames with PyAV, and re-mux them into a FRAGMENTED MP4 / H.264 byte stream
(`ftyp` + `moov`(mvex) init segment, then `moof` + `mdat` media fragments).

Same shape as demo 46 — only the frames come from a real video instead of being
synthesized. The element content_type is `video/mp4;codecs="avc1.42E01E"`, so the
UI render registry (planLiveRender) returns mediaKind=video and the panel can
append the stream into a `<video>` element via Media Source Extensions WHILE the
detector downstream is already pulling the same bytes off the data plane and
running YOLO over them. The bulk video rides the out-of-band JetStream
datastream transport — it NEVER enters the net marking; the net sees only the
channel's open + close (2 firings for the whole clip).
"""

import io
import time

import av

import aithericon
from aithericon import open_output, set_output

FPS = 10  # the bundled clip is 10 fps
# Constrained Baseline 3.0 — the most broadly MSE-supported H.264 string.
CONTENT_TYPE = 'video/mp4;codecs="avc1.42E01E"'

# `start.video` is the runner-injected Start borrow (a FileRef map). Retrieve the
# bytes to a local path through the sidecar.
clip_path = aithericon.file(start.video["key"]).retrieve()  # noqa: F821


class _CaptureWriter(io.RawIOBase):
    """A non-seekable sink libav writes the muxed fragments into, in order."""

    def __init__(self):
        self.chunks = []

    def writable(self):
        return True

    def write(self, b):
        self.chunks.append(bytes(b))
        return len(b)


def _remux_to_fragmented_mp4(src_path):
    """Decode the source clip and re-encode it into a fragmented MP4 byte stream
    (baseline H.264, browser-MSE-appendable); return the ordered chunks."""
    src = av.open(src_path)
    in_stream = src.streams.video[0]
    width = in_stream.codec_context.width
    height = in_stream.codec_context.height

    writer = _CaptureWriter()
    out = av.open(
        writer,
        mode="w",
        format="mp4",
        options={
            "movflags": "frag_keyframe+empty_moov+default_base_moof",
            "frag_duration": "500000",  # microseconds
        },
    )
    ostream = out.add_stream("h264", rate=FPS)
    ostream.width = width
    ostream.height = height
    ostream.pix_fmt = "yuv420p"
    ostream.options = {
        "profile": "baseline",
        "level": "3.0",
        "preset": "veryfast",
        "tune": "zerolatency",
    }
    ostream.codec_context.gop_size = max(1, FPS // 2)  # keyframe ~every 0.5 s

    frames = 0
    for frame in src.decode(video=0):
        frame = frame.reformat(format="yuv420p")
        for pkt in ostream.encode(frame):
            out.mux(pkt)
        frames += 1
    for pkt in ostream.encode(None):  # flush the encoder
        out.mux(pkt)
    out.close()
    src.close()
    return writer.chunks, frames, width, height


chunks, frames, width, height = _remux_to_fragmented_mp4(clip_path)
# Pace the replay over ~the clip duration so the live tap is perceptibly live and
# the detector drains a real, time-spread stream.
clip_seconds = max(0.1, frames / FPS)
pace = clip_seconds / max(1, len(chunks))

bytes_written = 0
with open_output("video") as out_chan:
    for chunk in chunks:
        out_chan.write(chunk, content_type=CONTENT_TYPE)
        bytes_written += len(chunk)
        time.sleep(pace)

set_output("fragments_written", len(chunks))
set_output("bytes_written", bytes_written)
set_output("frames", frames)
set_output("resolution", f"{width}x{height}")
