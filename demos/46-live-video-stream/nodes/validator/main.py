"""Validator (consumer): drain the `video` data channel and prove the bytes that
ARRIVED are a genuine, MSE-appendable fragmented-MP4 (H.264) stream — the
wire-level half of a verification the browser can't automate.

Identical in shape to demo 45's validator (only the channel name differs): it
never names a transport, accumulates the received bytes, and parses the
top-level MP4 box structure. A fragmented stream MSE can append is `ftyp`, then a
`moov` carrying `mvex` (the fragmented init segment), followed by one or more
`moof` + `mdat` media fragments. Because the channel transport (JetStream) is
durable + ordered, the received byte count MUST equal the producer's
`bytes_written`.
"""

import aithericon
from aithericon import set_output


def iter_boxes(data):
    """Yield (type, offset, size) for each top-level ISO-BMFF box."""
    off = 0
    n = len(data)
    while off + 8 <= n:
        size = int.from_bytes(data[off : off + 4], "big")
        typ = data[off + 4 : off + 8].decode("latin1", "replace")
        if size == 1:
            if off + 16 > n:
                break
            size = int.from_bytes(data[off + 8 : off + 16], "big")
        if size < 8 or off + size > n:
            break
        yield typ, off, size
        off += size


buf = bytearray()
for chunk in aithericon.stream("video"):
    if isinstance(chunk, (bytes, bytearray)):
        buf += chunk

types = []
moov_span = None
for typ, off, size in iter_boxes(buf):
    types.append(typ)
    if typ == "moov":
        moov_span = (off, size)

ftyp_first = bool(types) and types[0] == "ftyp"
has_moov = "moov" in types
has_mvex = False
if moov_span is not None:
    o, s = moov_span
    has_mvex = b"mvex" in bytes(buf[o : o + s])
moof = types.count("moof")
mdat = types.count("mdat")

valid_fmp4 = ftyp_first and has_moov and has_mvex and moof >= 1 and moof == mdat

set_output("received_bytes", len(buf))
set_output("moof_fragments", moof)
set_output("valid_fmp4", valid_fmp4)
