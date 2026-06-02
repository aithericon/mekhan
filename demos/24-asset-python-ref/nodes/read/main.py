# Reference curated assets DIRECTLY by their library ref-key in a Python body —
# no `assetBindings`, no alias. The compiler scans this source, resolves each
# `<head>.<field>` / bare `<head>` against the asset library, and auto-stages
# the asset by cardinality:
#   - `steel_spec` (object)     -> staged as its record DICT, so attribute
#                                  access (`steel_spec.yield_strength`) works.
#   - `metals_db`  (collection) -> staged as the row LIST (`len(metals_db)`).
# This is the unified named-global path: an asset is first-class in a Python
# body exactly like a resource (`pg.host`) — one body scan, the matched
# global's kind picks the staging transport.

log_info("step started")

# Object asset: attribute access on the single staged record dict.
ys = steel_spec.yield_strength
grade = steel_spec.grade

# Collection asset: the staged list of record dicts.
material_count = len(metals_db)

log_info("read assets", grade=grade, yield_strength=ys, materials=material_count)

# Implicit outputs: the runner sweeps globals matching this step's declared
# output port at the end of execution.
yield_strength = ys
steel_grade = grade
