"""Producer: synthesize a short tone arpeggio, mux it as a FRAGMENTED MP4 / AAC
stream with PyAV, and pace the fragments out over the `media` data channel in
~real time.

This is the container-codec sibling of demo 42 (raw-PCM live audio). Where 42
emits headerless L16 the browser plays through Web Audio, this emits a
fragmented-MP4 byte stream — an `ftyp` + `moov` (carrying `mvex`) init segment
followed by `moof` + `mdat` media fragments — which is exactly what Media Source
Extensions appends progressively. The channel's element content_type is
`audio/mp4;codecs="mp4a.40.2"`, so the UI's render-adapter registry
(`planLiveRender`) dispatches THIS channel to the MSE player, not Web Audio:
the presentation-side analog of the wire's transport dispatch.

PyAV bundles libav, so no system ffmpeg is needed — `av` is just a pip
requirement. libav calls `.write()` on a non-seekable file-like as it muxes; we
capture each chunk in order, then replay them over the channel with a small
sleep so a live `?follow=1` tap can append-and-play WHILE we're still producing.
The bytes ride the out-of-band JetStream datastream; the net only ever sees the
channel's open + close (2 firings).
"""

import io
import math
import time

import av
import numpy as np

import aithericon
from aithericon import open_output, set_output

SR = 44100  # sample rate (Hz)
NOTE_SECONDS = 1.0  # duration of each arpeggio note
NOTES_HZ = [440.0, 554.37, 659.25, 880.0]  # A4 · C#5 · E5 · A5 — an A-major arpeggio
# AAC-LC in fragmented MP4. The `codecs=` param is required: the browser's
# MediaSource.isTypeSupported / addSourceBuffer both need it.
CONTENT_TYPE = 'audio/mp4;codecs="mp4a.40.2"'


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
    """Mux the arpeggio into a fragmented MP4 byte stream; return its chunks."""
    writer = _CaptureWriter()
    container = av.open(
        writer,
        mode="w",
        format="mp4",
        options={
            # empty_moov + frag_keyframe → a streamable init segment then media
            # fragments (vs. a single moov at the end a non-fragmented MP4 needs,
            # which MSE can't append). frag_duration cuts a fresh moof every
            # 250 ms so a tap can start decoding before the producer finishes.
            "movflags": "frag_keyframe+empty_moov+default_base_moof",
            "frag_duration": "250000",  # microseconds
        },
    )
    stream = container.add_stream("aac", rate=SR)
    stream.codec_context.layout = "mono"

    # Synthesize the arpeggio as one float32 mono signal, with a short fade per
    # note so the segment boundaries don't click.
    total = int(SR * NOTE_SECONDS * len(NOTES_HZ))
    t = np.arange(total) / SR
    sig = np.zeros(total, dtype=np.float32)
    for i, hz in enumerate(NOTES_HZ):
        lo = int(i * NOTE_SECONDS * SR)
        hi = int((i + 1) * NOTE_SECONDS * SR)
        seg = t[lo:hi]
        env = np.minimum(1.0, np.minimum(seg - seg[0], seg[-1] - seg) * 20.0)
        sig[lo:hi] = (0.25 * env * np.sin(2 * math.pi * hz * seg)).astype(np.float32)

    # The AAC encoder takes fixed-size frames (frame_size samples); feed it via a
    # FIFO so we hand it exactly that many at a time.
    fifo = av.audio.fifo.AudioFifo()
    frame = av.AudioFrame.from_ndarray(sig.reshape(1, -1), format="fltp", layout="mono")
    frame.sample_rate = SR
    fifo.write(frame)

    fsize = stream.codec_context.frame_size or 1024
    while True:
        block = fifo.read(fsize)
        if block is None:
            break
        block.pts = None
        for pkt in stream.encode(block):
            container.mux(pkt)
    for pkt in stream.encode(None):  # flush the encoder
        container.mux(pkt)
    container.close()  # writes the final moof/mdat (+ mfra index)
    return writer.chunks


chunks = _encode_fragmented_mp4()
audio_seconds = NOTE_SECONDS * len(NOTES_HZ)
# Pace the replay so the episode lasts about as long as the audio, making the
# live tap perceptibly live (and giving the validator a real stream to drain).
pace = audio_seconds / max(1, len(chunks))

bytes_written = 0
with open_output("media") as out:
    for chunk in chunks:
        out.write(chunk, content_type=CONTENT_TYPE)
        bytes_written += len(chunk)
        time.sleep(pace)

set_output("fragments_written", len(chunks))
set_output("bytes_written", bytes_written)
set_output("audio_seconds", round(audio_seconds, 2))
