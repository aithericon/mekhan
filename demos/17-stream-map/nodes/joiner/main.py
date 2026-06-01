# Joiner — concatenate the gathered Map collection (demo 16).
#
# The streaming Map parks its gathered collection as the envelope
# `{ output: [<element>, ...] }` at `p_mapper_data`. This source references
# `mapper.output` (an upstream parked Map producer), so the compiler scans it,
# synthesizes a read-arc into the Map's parked place, and stages the whole
# envelope as `mapper.json` — the runner promotes `mapper` to a global.
#
# Each element is one uppercased word (the Map lifted the body's `upper` value
# directly). Join them in stream order into the final transcript.

elems = mapper.output

transcript = " ".join(str(e) for e in elems)
