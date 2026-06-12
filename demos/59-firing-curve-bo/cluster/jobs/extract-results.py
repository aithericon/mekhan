#!/usr/bin/env python3
"""Extract campaign objectives from a finished puck-firing case.

Usage: extract-results.py <case_dir>   (params read from env, results.json written to case_dir)
"""
import json, os, re, glob, sys

case = sys.argv[1] if len(sys.argv) > 1 else "."
os.chdir(case)

ramp = float(os.environ.get("RAMP_RATE", 5))
hold_T = float(os.environ.get("HOLD_TEMP", 1850))
hold_t = float(os.environ.get("HOLD_TIME", 3600))
cool = float(os.environ.get("COOL_RATE", 10))
T0 = 300.0
t_ramp_end = (hold_T - T0) / (ramp / 60.0)
t_hold_end = t_ramp_end + hold_t
t_end = t_hold_end + (hold_T - T0) / (cool / 60.0)

# Peak von Mises stress over the whole cycle, from solver log
sigma_max, sigma_max_t = 0.0, None
t_cur = None
for line in open("log.solidDisplacementFoam"):
    m = re.match(r"^Iteration: (\S+)", line)
    if m:
        t_cur = float(m.group(1))
        continue
    m = re.search(r"Max sigmaEq = (\S+)", line)
    if m:
        s = float(m.group(1))
        if s > sigma_max:
            sigma_max, sigma_max_t = s, t_cur

# Soak completeness: min core temperature during the hold window
soak_min = None
for dat in sorted(glob.glob("postProcessing/minMaxT/*/fieldMinMax.dat")):
    for line in open(dat):
        if line.startswith("#"):
            continue
        parts = line.split()
        try:
            t, tmin = float(parts[0]), float(parts[2])
        except (ValueError, IndexError):
            continue
        if t_ramp_end <= t <= t_hold_end:
            soak_min = tmin if soak_min is None else min(soak_min, tmin)

results = {
    "params": {"ramp_rate_K_min": ramp, "hold_temp_K": hold_T,
               "hold_time_s": hold_t, "cool_rate_K_min": cool},
    "cycle_time_s": t_end,
    "max_sigmaEq_Pa": sigma_max,
    "max_sigmaEq_time_s": sigma_max_t,
    "soak_min_core_T_K": soak_min,
    "job_id": os.environ.get("SLURM_JOB_ID"),
}
with open("results.json", "w") as f:
    json.dump(results, f, indent=2)
print(json.dumps(results, indent=2))
