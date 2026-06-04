# VAD filter — decode the input WAV, energy-gate it, re-emit voiced audio on a
# Data/Out channel (docs/25).
#
# The audio arrives base64-encoded in the Start field `audio_b64` (passed inline
# in the start token — no file upload / object store in the loop, so the demo
# runs off plain JSON). Accessing `start.audio_b64` is the upstream borrow.
#
# The "VAD" here is a deliberately tiny pure-Python energy gate (stdlib
# `audioop` RMS per 30 ms frame, with a little hangover so word edges aren't
# clipped) — zero extra deps so the demo just runs. For real use swap in
# webrtcvad or silero-vad; the channel plumbing is identical.
#
# Voiced frames are re-emitted as raw 16-bit PCM over the `speech` data channel,
# ~100 ms per write. The bulk bytes ride the out-of-band JetStream datastream
# transport, NOT the net marking — the net sees only the channel's open + close.

import audioop
import base64
import io
import wave

from aithericon import open_output, set_output

FRAME_MS = 30
HANGOVER = 3  # frames kept on each side of a voiced frame

raw_wav = base64.b64decode(start.audio_b64)  # noqa: F821 — runner-injected Start borrow

with wave.open(io.BytesIO(raw_wav), "rb") as w:
    assert w.getsampwidth() == 2, "expect 16-bit PCM WAV"
    rate = w.getframerate()
    channels = w.getnchannels()
    pcm = w.readframes(w.getnframes())

# Downmix to mono if the source is stereo.
if channels > 1:
    pcm = audioop.tomono(pcm, 2, 0.5, 0.5)

frame_bytes = int(rate * FRAME_MS / 1000) * 2  # samples-per-frame * 2 bytes
frames = [pcm[i : i + frame_bytes] for i in range(0, len(pcm), frame_bytes)]
frames = [f for f in frames if len(f) == frame_bytes]

rms = [audioop.rms(f, 2) for f in frames]
peak = max(rms) if rms else 0
threshold = max(60, int(0.18 * peak))  # simple relative energy gate

voiced = [r > threshold for r in rms]
# Hangover: keep a few frames around each voiced frame so we don't clip onsets.
mask = list(voiced)
for i, v in enumerate(voiced):
    if v:
        lo = max(0, i - HANGOVER)
        hi = min(len(mask), i + HANGOVER + 1)
        for j in range(lo, hi):
            mask[j] = True

BATCH = max(1, 100 // FRAME_MS)  # ~100 ms per write — cheaper on JetStream
voiced_frames = 0
buf = bytearray()
pending = 0

with open_output("speech") as out:
    for frame, keep in zip(frames, mask):
        if not keep:
            continue
        voiced_frames += 1
        buf += frame
        pending += 1
        if pending >= BATCH:
            out.write(bytes(buf), content_type=f"audio/L16;rate={rate}")
            buf = bytearray()
            pending = 0
    if buf:
        out.write(bytes(buf), content_type=f"audio/L16;rate={rate}")

set_output("frames_total", len(frames))
set_output("frames_voiced", voiced_frames)
set_output("sample_rate", rate)
