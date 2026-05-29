"""Loop body tail — fold the K evaluations into the observation set.

Reads the gathered Map collection via `evals.output` — the Map parks its
reduced collection as the envelope `{ output: [<element>, ...] }` at
`p_<map>_data`, and the compiler stages that whole envelope as `evals.json`
because this source references `evals.output` (an upstream parked Map
producer). Each element is one branin observation `{a, d, z}` (the Map lifts
the body's `resultVar` value directly). Also reads the prior incumbent
(`bo.f_best`, `bo.best_a`, `bo.best_d`). The outputs drive the loop's
accumulator mergeExprs (`gather.merged`, `gather.new_f_best`, ...).
"""

# The Map collection envelope: `evals.output` is the list of K observations,
# each element a plain `{a, d, z}` dict.
elems = evals.output

merged = [{"a": e["a"], "d": e["d"], "z": e["z"]} for e in elems]

cur_min = min(e["z"] for e in elems)

if cur_min < bo.f_best:
    new_f_best = cur_min
    arg = min(elems, key=lambda e: e["z"])
    new_best_a = arg["a"]
    new_best_d = arg["d"]
else:
    new_f_best = bo.f_best
    new_best_a = bo.best_a
    new_best_d = bo.best_d
