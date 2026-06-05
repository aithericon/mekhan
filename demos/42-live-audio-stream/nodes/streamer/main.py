# Pace the uploaded WAV out over the `speech` data channel in ~real time, so a
# live tap (`?follow=1`) can play it through Web Audio WHILE this step is still
# producing (docs/25).
#
# The audio arrives as a `file`-kind Start field, so `start.audio` is a FileRef;
# `aithericon.file(...).retrieve()` brokers the bytes to a local path. We read
# the raw 16-bit PCM and write it over the `speech` Data/Out channel in ~200 ms
# slices, sleeping ~200 ms between writes — so the whole episode lasts about as
# long as the audio. Nothing in the net consumes the channel; the bytes ride the
# out-of-band JetStream datastream, which the UI taps directly.

import time
import wave

import aithericon
from aithericon import open_output, set_output

CHUNK_MS = 200  # audio per write — also the pacing sleep, so it streams ~real-time

wav_path = aithericon.file(start.audio["key"]).retrieve()  # noqa: F821

with wave.open(wav_path, "rb") as w:
    assert w.getsampwidth() == 2, "expect 16-bit PCM WAV"
    rate = w.getframerate()
    channels = w.getnchannels()
    pcm = w.readframes(w.getnframes())

# The live player assumes mono L16; downmix if needed (stdlib, no deps).
if channels > 1:
    import audioop

    pcm = audioop.tomono(pcm, 2, 0.5, 0.5)
    channels = 1

frame_bytes = int(rate * CHUNK_MS / 1000) * 2  # samples-per-slice * 2 bytes (mono 16-bit)
sent = 0

with open_output("speech") as out:
    for i in range(0, len(pcm), frame_bytes):
        chunk = pcm[i : i + frame_bytes]
        out.write(chunk, content_type=f"audio/L16;rate={rate}")
        sent += len(chunk)
        time.sleep(CHUNK_MS / 1000.0)  # pace ~real-time so the tap is perceptibly live

set_output("bytes_streamed", sent)
set_output("sample_rate", rate)
set_output("seconds", round(len(pcm) / (rate * 2), 2))
