# Transcribe — drain the voiced-audio data channel, run faster-whisper, and
# EMIT each segment as a control-plane stream (docs/25).
#
# `aithericon.stream("speech")` drains the VAD step's out-of-band byte stream. It
# starts EARLY — the moment the VAD step's `open` descriptor reaches this node —
# so transcription can begin while the producer is still emitting. Each element
# is a chunk of raw 16-bit PCM; we reassemble the whole utterance, then run
# faster-whisper over it. The channel's element kind is `binary`, so `stream()`
# yields `bytes`; the contract is 16 kHz mono, so we decode int16 → float32.
#
# Whisper yields a GENERATOR of segments (inference is lazy), so instead of
# joining them into one string we re-emit each segment the moment it lands, as a
# control-plane item on the OUT channel `parts`. `aithericon.out("parts")` opens
# one bracketed episode — open → item* → close(count) — and each `.emit(...)`
# fires a single tiny `{index,text,start,end}` control token. The producer does
# NOT fold; the consumer edge (`join: gather`) does, in the `collect` step. The
# bulk audio already rode the data plane — only small text tokens flow here.

import numpy as np
from faster_whisper import WhisperModel

import aithericon
from aithericon import set_output, stream

SAMPLE_RATE = 16000

pcm = b"".join(stream("speech"))
audio = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0

# `tiny` keeps the model download (~75 MB) and CPU inference small for a demo.
model = WhisperModel("tiny", device="cpu", compute_type="int8")
segments, _info = model.transcribe(audio, language="en", vad_filter=False)

count = 0
with aithericon.out("parts") as parts:
    for seg in segments:  # lazy generator — each pass runs the next chunk
        text = seg.text.strip()
        if not text:
            continue
        parts.emit(
            {
                "index": count,
                "text": text,
                "start": round(seg.start, 2),
                "end": round(seg.end, 2),
            }
        )
        count += 1
# close(count) fires on clean block exit — the gather barrier sizes on it.

set_output("segment_count", count)
set_output("seconds", round(len(audio) / SAMPLE_RATE, 2))
