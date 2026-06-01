# Reducer — stage 3: fold the transform's stream into the final transcript.
#
# streamInput=true: seeded at net entry, receives the TRANSFORM's chunks (the
# already-uppercased words) via aithericon.chunks() over IPC, and concatenates
# them in stream order. The transform's completion (transform.out → this node's
# `in`) is the EOF that ends our chunks() loop. We do NO uppercasing here — the
# transform already did it — so the result proves the transform ran.

from aithericon import chunks, set_output

acc = []
for chunk in chunks():
    acc.append(str(chunk))

transcript = " ".join(acc)
set_output("transcript", transcript)
