"""Loop body tail — fold the K firing-curve evaluations into the campaign.

Reads the gathered Map collection via `evals.output` (the Map parks its
reduced collection as the envelope `{ output: [<element>, ...] }`; each
element is one `simulate` observation), plus the prior incumbent
(`campaign.f_best`, `campaign.best_ramp`, `campaign.best_cool`,
`campaign.best_hold`). The outputs drive the loop's accumulator mergeExprs
(`gather.merged`, `gather.new_f_best`, ...). Literal slug reads below are the
compiler's read-arc anchors — keep them verbatim.
"""

elems = evals.output

merged = [
    {
        "u_ramp": e["u_ramp"],
        "u_cool": e["u_cool"],
        "u_hold": e["u_hold"],
        "ramp_rate": e["ramp_rate"],
        "cool_rate": e["cool_rate"],
        "hold_time_s": e["hold_time_s"],
        "sigma_max_mpa": e["sigma_max_mpa"],
        "soak_min_k": e["soak_min_k"],
        "cycle_h": e["cycle_h"],
        "source": e.get("source", "unknown"),
        "verdict": e.get("verdict", ""),
        "z": e["z"],
    }
    for e in elems
]

cur_min = min(e["z"] for e in elems)

if cur_min < campaign.f_best:
    arg = min(elems, key=lambda e: e["z"])
    new_f_best = cur_min
    new_best_ramp = arg["ramp_rate"]
    new_best_cool = arg["cool_rate"]
    new_best_hold = arg["hold_time_s"]
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
    log_info(f"gather: incumbent holds (z={new_f_best:.3f}, batch min {cur_min:.3f})")
