# Transcribe — drain the voiced-audio data channel and run faster-whisper
# (docs/25).
#
# `aithericon.stream("speech")` drains the VAD step's out-of-band byte stream. It
# starts EARLY — the moment the VAD step's `open` descriptor reaches this node —
# so transcription can begin while the producer is still emitting. Each element
# is a chunk of raw 16-bit PCM; we reassemble the whole utterance, then run
# faster-whisper over it.
#
# The channel's element kind is `binary`, so `stream()` yields `bytes`. The demo
# contract is 16 kHz mono (what the VAD step emits and what Whisper wants), so we
# decode int16 → float32 in [-1, 1] and hand the array straight to the model.

import numpy as np
from faster_whisper import WhisperModel

from aithericon import set_output, stream

SAMPLE_RATE = 16000

pcm = b"".join(stream("speech"))
audio = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0

# `tiny` keeps the model download (~75 MB) and CPU inference small for a demo.
model = WhisperModel("tiny", device="cpu", compute_type="int8")
segments, _info = model.transcribe(audio, language="en", vad_filter=False)
text = " ".join(seg.text.strip() for seg in segments).strip()

set_output("transcript", text)
set_output("seconds", round(len(audio) / SAMPLE_RATE, 2))
