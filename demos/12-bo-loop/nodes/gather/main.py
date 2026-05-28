"""Loop body tail — fold the K evaluations into the observation set.

Reads the gathered Map collection `evals[*].obs` (the `[*]` boundary is
mandatory — the compiler stages the parked collection and the runner promotes
`evals` as a list of elements, each with `.obs == {a, d, z}`), plus the prior
incumbent (`bo.f_best`, `bo.best_a`, `bo.best_d`). The outputs drive the loop's
accumulator mergeExprs (`gather.merged`, `gather.new_f_best`, ...).
"""

# `evals` is the gathered collection; each element carries the body output `obs`.
zs = [e.obs for e in evals]

merged = [{"a": o["a"], "d": o["d"], "z": o["z"]} for o in zs]

cur_min = min(o["z"] for o in zs)

if cur_min < bo.f_best:
    new_f_best = cur_min
    arg = min(zs, key=lambda o: o["z"])
    new_best_a = arg["a"]
    new_best_d = arg["d"]
else:
    new_f_best = bo.f_best
    new_best_a = bo.best_a
    new_best_d = bo.best_d
