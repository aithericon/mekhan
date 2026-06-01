# Reducer — demo 15 (StreamConsumer dispatch=LiveReduce).
#
# LiveReduce: the StreamConsumer is a thin router that forwards chunks to this
# long-lived Python process via IPC. We receive chunks via `aithericon.chunks()`,
# do our own reduction (uppercase + concatenate), and emit the final result.
#
# The StreamConsumer owns the stream_count barrier and EOF sentinel; we own the
# reduction logic. No Petri-net gather barrier — the Python script is the reducer.
#
# `aithericon.chunks()` is a generator that yields chunk values as they arrive
# over the IPC sidecar. It terminates when the producer's stream ends (EOF).

from aithericon import chunks, set_output

acc = []
for chunk in chunks():
    acc.append(str(chunk).upper())

transcript = " ".join(acc)
set_output("transcript", transcript)
