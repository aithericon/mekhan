"""Loop body tail — fold the K firing-curve evaluations into the campaign.

Reads the gathered Map collection via `evals.output` (one per-candidate physics
bundle from the OpenFOAM library node — sigma_max / soak_min / cycle / cost /
verdict / source / field_key) and zips it BY INDEX with `propose.candidates`.
The zip is sound because the Map's gather barrier sorts results by map index, so
`evals.output[i]` is the evaluation of `propose.candidates[i]`. The candidate
side carries the BO-internal unit-cube coords (`u_ramp`/`u_cool`/`u_hold`) plus
the physical firing curve — the library node deliberately does NOT echo these,
and the proposer's constraint GPs need the u-coords back to fit.

Also reads the prior incumbent (`campaign.f_best`, `campaign.best_ramp`,
`campaign.best_cool`, `campaign.best_hold`, `campaign.best_field_key`). The
outputs drive the loop's accumulator mergeExprs (`gather.merged`,
`gather.new_f_best`, `gather.new_best_field_key`, ...). Literal slug reads below
are the compiler's read-arc anchors — keep them verbatim.
"""

evals_out = evals.output
cands = propose.candidates

# Re-attach each candidate's u-coords + physical curve to its physics bundle.
rows = []
for i in range(len(evals_out)):
    e = evals_out[i]
    c = cands[i]
    rows.append(
        {
            "u_ramp": c["u_ramp"],
            "u_cool": c["u_cool"],
            "u_hold": c["u_hold"],
            "ramp_rate": c["ramp_rate"],
            "cool_rate": c["cool_rate"],
            "hold_time_s": c["hold_time_s"],
            "sigma_max_mpa": e["sigma_max_mpa"],
            "soak_min_k": e["soak_min_k"],
            "cycle_h": e["cycle_h"],
            "source": e.get("source", "unknown"),
            "verdict": e.get("verdict", ""),
            "z": e["z"],
            "field_key": e.get("field_key", ""),
        }
    )

# `merged` feeds the proposer's GP (via `campaign.observations`); the per-row
# field_key is render-only, so it's dropped here — only the incumbent's key is
# tracked, on the `best_field_key` accumulator below.
merged = [{k: v for k, v in r.items() if k != "field_key"} for r in rows]

cur_min = min(r["z"] for r in rows)

if cur_min < campaign.f_best:
    arg = min(rows, key=lambda r: r["z"])
    new_f_best = cur_min
    new_best_ramp = arg["ramp_rate"]
    new_best_cool = arg["cool_rate"]
    new_best_hold = arg["hold_time_s"]
    new_best_field_key = arg["field_key"]
    log_info(
        f"gather: NEW incumbent z={cur_min:.3f} "
        f"(ramp {new_best_ramp} / cool {new_best_cool} / hold {new_best_hold}s, "
        f"{arg.get('verdict', '?')})"
    )
else:
    new_f_best = campaign.f_best
    new_best_ramp = campaign.best_ramp
    new_best_cool = campaign.best_cool
    new_best_hold = campaign.best_hold
    new_best_field_key = campaign.best_field_key
    log_info(f"gather: incumbent holds (z={new_f_best:.3f}, batch min {cur_min:.3f})")
