"""Metric extraction + scoring for the firing-curve evaluator (composite f001).

Reads the generic `openfoam/run-case` solver's RAW outputs (`run.solver_log`,
`run.postprocessing`, `run.field_key`, `run.success`) plus the firing params and
timing from `casegen.*`, and turns them into the firing-curve objectives. The
firing-specific reading of a solidDisplacementFoam run lives HERE, not in the
generic solver node.

If the solve succeeded, parse peak von-Mises stress (solver log) + soak
completeness (the fieldMinMax `.dat`); otherwise evaluate the calibrated
closed-form surrogate. `solver_mode == "docker"` is real-physics-or-bust (raise
if the solve failed); `auto`/`surrogate` fall back. Emits the firing objectives,
the cost `z`, a `verdict`, the field-series `field_key` (for the post-loop
cut-through render), and a single `evaluation` bundle (a Map gathers one field).
"""

import math
import os
import re

# --- Inputs: casegen.* (firing params + timing) and run.* (raw solve) ----------
ramp_rate = float(casegen.ramp_rate)
cool_rate = float(casegen.cool_rate)
hold_time_s = float(casegen.hold_time_s)
hold_temp = float(casegen.hold_temp)
sigma_limit = float(casegen.sigma_limit)
solver_mode = str(casegen.solver_mode)
cycle_h = float(casegen.cycle_h)
t_ramp_end = float(casegen.t_ramp_end)
t_hold_end = float(casegen.t_hold_end)
t_end = float(casegen.t_end)
T0 = float(casegen.T0)

run_success = bool(run.success)
solver_log = str(run.solver_log or "")
postprocessing = run.postprocessing or {}
run_field_key = str(run.field_key or "")

_artifacts_dir = os.environ.get("AITHERICON_ARTIFACTS_DIR", os.getcwd())


# --- Real-run extraction (from the generic solver's raw text outputs) ----------
def _parse_openfoam():
    """Firing-specific reading of a solidDisplacementFoam run. v2506 prints
    `Iteration: <t>` then `Max sigmaEq = <Pa>` per step; the fieldMinMax FO writes
    min core T to postProcessing/minMaxT/*/fieldMinMax.dat (col 0=time, col 2=min).
    """
    sigma_t, sigma_v = [], []
    t_cur = None
    for line in solver_log.splitlines():
        m = re.match(r"^Iteration: (\S+)", line)
        if m:
            t_cur = float(m.group(1))
            continue
        m = re.search(r"Max sigmaEq = (\S+)", line)
        if m and t_cur is not None:
            sigma_t.append(t_cur)
            sigma_v.append(float(m.group(1)))
    if not sigma_v:
        raise RuntimeError("no `Max sigmaEq` lines in solver log")

    core_t, core_v = [], []
    for rel, content in sorted(postprocessing.items()):
        if "minMaxT" not in rel or not rel.endswith("fieldMinMax.dat"):
            continue
        for line in content.splitlines():
            if line.startswith("#"):
                continue
            parts = line.split()
            try:
                core_t.append(float(parts[0]))
                core_v.append(float(parts[2]))
            except (ValueError, IndexError):
                continue

    sigma_max_pa = max(sigma_v)
    sigma_max_time = sigma_t[sigma_v.index(sigma_max_pa)]
    soak_vals = [v for t, v in zip(core_t, core_v) if t_ramp_end <= t <= t_hold_end]
    soak_min = min(soak_vals) if soak_vals else 0.0

    return {
        "source": "openfoam",
        "sigma_max_mpa": sigma_max_pa / 1e6,
        "sigma_max_time_s": sigma_max_time,
        "soak_min_k": soak_min,
        "trace": {
            "sigma_t": sigma_t,
            "sigma_mpa": [v / 1e6 for v in sigma_v],
            "core_t": core_t,
            "core_k": core_v,
        },
    }


def _run_surrogate():
    """Closed-form stand-in calibrated against the cluster-validated runs.

    Calibration anchors (Elwetritsch zr runs, hold @ 1850 K; ramp/hold/cool):
      5 / 3600 / 10  -> 18.49 MPa, soak_min 1848.47 K
      20 / 1800 / 30 -> 29.39 MPa, soak_min 1842.80 K
      60 /  600 /120 -> 103.2 MPa, soak_min 1825.25 K
    sigma is rate-driven (peak at end-of-cool): power-law on max(ramp, cool).
    soak_min is the core temperature at hold START (ramp-lag), so it depends
    on ramp only — exact 3-point fit: deficit = 0.256 * ramp^1.11.
    """
    r_eff = max(ramp_rate, cool_rate)
    dT = hold_temp - T0
    sigma_max_mpa = 18.5 * (r_eff / 10.0) ** 0.69 * (dT / 1550.0)
    soak_deficit = 0.256 * ramp_rate**1.11
    soak_min = hold_temp - soak_deficit

    n = 200
    ts = [t_end * i / (n - 1) for i in range(n)]
    sigma_trace = []
    for t in ts:
        if t <= t_ramp_end:
            s = sigma_max_mpa * 0.55 * (t / max(t_ramp_end, 1.0))
        elif t <= t_hold_end:
            frac = (t - t_ramp_end) / max(hold_time_s, 1.0)
            s = sigma_max_mpa * 0.55 * math.exp(-2.5 * frac)
        else:
            frac = (t - t_hold_end) / max(t_end - t_hold_end, 1.0)
            s = sigma_max_mpa * (0.15 + 0.85 * frac)
        sigma_trace.append(s)
    core_trace = [
        max(T0, min(_kiln_T(t) - soak_deficit * _lag_frac(t), hold_temp))
        for t in ts
    ]

    return {
        "source": "surrogate",
        "sigma_max_mpa": sigma_max_mpa,
        "sigma_max_time_s": t_end,
        "soak_min_k": soak_min,
        "trace": {
            "sigma_t": ts,
            "sigma_mpa": sigma_trace,
            "core_t": ts,
            "core_k": core_trace,
        },
    }


def _kiln_T(t):
    """Prescribed kiln curve (the table BC)."""
    if t <= t_ramp_end:
        return T0 + (hold_temp - T0) * t / max(t_ramp_end, 1.0)
    if t <= t_hold_end:
        return hold_temp
    return hold_temp - (hold_temp - T0) * (t - t_hold_end) / max(t_end - t_hold_end, 1.0)


def _lag_frac(t):
    """Rough thermal-lag weight for the synthetic core trace."""
    if t <= t_ramp_end:
        return 1.0
    if t <= t_hold_end:
        return math.exp(-2.0 * (t - t_ramp_end) / max(hold_time_s, 1.0))
    return 0.3


# --- Dispatch: real extraction or surrogate fallback ----------------------------
result = None
if run_success:
    try:
        result = _parse_openfoam()
    except Exception as exc:  # noqa: BLE001
        if solver_mode == "docker":
            raise  # real-physics-or-bust
        log_warn(f"extract: openfoam parse failed ({exc!r}); surrogate fallback")
        result = _run_surrogate()
else:
    if solver_mode == "docker":
        raise RuntimeError(
            "solver_mode=docker but the OpenFOAM solve did not succeed "
            "(run.success=false) — real-physics-or-bust"
        )
    result = _run_surrogate()

sigma_max_mpa = float(result["sigma_max_mpa"])
soak_min_k = float(result["soak_min_k"])
source = str(result["source"])

# --- 4. Cost --------------------------------------------------------------------
soak_target = hold_temp - 15.0
soak_deficit = max(0.0, soak_target - soak_min_k)

pen_sigma = 0.0
if sigma_max_mpa > sigma_limit:
    pen_sigma = 25.0 + 50.0 * (sigma_max_mpa - sigma_limit) / sigma_limit
pen_soak = 0.0
if soak_deficit > 0.0:
    pen_soak = 10.0 + 2.0 * soak_deficit

z = cycle_h + pen_sigma + pen_soak
ok = pen_sigma == 0.0 and pen_soak == 0.0
verdict = "OK" if ok else ("CRACK RISK" if pen_sigma > 0.0 else "UNDER-SOAKED")

log_info(
    f"extract[{source}]: sigma_max={sigma_max_mpa:.1f} MPa "
    f"(limit {sigma_limit:.0f}) soak_min={soak_min_k:.1f} K "
    f"(target {soak_target:.0f}) cycle={cycle_h:.2f} h -> z={z:.3f} [{verdict}]"
)

# --- 5. Media-card PNG (best-effort — never fails the evaluation) ----------------
try:
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    trace = result["trace"]
    kiln_ts = [t_end * i / 399.0 for i in range(400)]

    fig, (ax_t, ax_s) = plt.subplots(
        2, 1, figsize=(9.0, 6.4), sharex=True,
        gridspec_kw={"height_ratios": [1.1, 1.0]},
    )

    ax_t.plot(
        [t / 3600.0 for t in kiln_ts], [_kiln_T(t) for t in kiln_ts],
        color="#d97706", lw=2.0, label="kiln set-point",
    )
    if trace["core_t"]:
        ax_t.plot(
            [t / 3600.0 for t in trace["core_t"]], trace["core_k"],
            color="#2563eb", lw=1.6, label="slowest point (min T)",
        )
    ax_t.axhline(soak_target, color="#2563eb", ls=":", lw=1.0, alpha=0.6)
    ax_t.axvspan(t_ramp_end / 3600.0, t_hold_end / 3600.0, color="#d97706", alpha=0.08)
    ax_t.set_ylabel("T [K]")
    ax_t.legend(loc="lower center", fontsize=8, ncol=2)

    ax_s.plot(
        [t / 3600.0 for t in trace["sigma_t"]], trace["sigma_mpa"],
        color="#dc2626", lw=1.8, label="max von-Mises",
    )
    ax_s.axhline(
        sigma_limit, color="#dc2626", ls="--", lw=1.2, alpha=0.8,
        label=f"crack threshold {sigma_limit:.0f} MPa",
    )
    ax_s.set_xlabel("time [h]")
    ax_s.set_ylabel("sigma_eq [MPa]")
    ax_s.legend(loc="upper left", fontsize=8)

    src_tag = " (surrogate)" if source == "surrogate" else ""
    fig.suptitle(
        f"ramp {ramp_rate:.0f} K/min · hold {hold_time_s / 60.0:.0f} min @ "
        f"{hold_temp:.0f} K · cool {cool_rate:.0f} K/min{src_tag}\n"
        f"sigma_max {sigma_max_mpa:.1f} MPa · soak min {soak_min_k:.0f} K · "
        f"cycle {cycle_h:.2f} h — {verdict} (cost {z:.2f})",
        fontsize=10,
    )
    fig.tight_layout(rect=(0, 0, 1, 0.91))

    png_name = f"firing_r{ramp_rate:.0f}_h{hold_time_s:.0f}_c{cool_rate:.0f}.png"
    png_path = os.path.join(_artifacts_dir, png_name)
    fig.savefig(png_path, dpi=110)
    plt.close(fig)

    log_artifact(
        png_path,
        name=png_name,
        category="plot",
        mime_type="image/png",
        metadata={
            "kind": "firing_curve_eval",
            "verdict": verdict,
            "sigma_max_mpa": f"{sigma_max_mpa:.2f}",
            "cost": f"{z:.3f}",
            "source": source,
        },
    )
except Exception as exc:  # noqa: BLE001 — plotting is telemetry, not physics
    log_warn(f"simulate: plot/artifact failed (non-fatal): {exc!r}")

# --- field_key: only the real OpenFOAM path produced a field series ------------
# (the generic solver persisted the VTK tarball on a successful export; the
# surrogate has no spatial field). Threaded out so demo 59's gather can track
# the incumbent's key for the post-loop cut-through render.
field_key = run_field_key if source == "openfoam" else ""

# --- Typed outputs (swept from globals matching the output port) ----------------
sigma_max_mpa = round(sigma_max_mpa, 3)
soak_min_k = round(soak_min_k, 2)
cycle_h = round(cycle_h, 4)
z = round(z, 4)

# Single gatherable bundle. A Map body gathers exactly ONE named field, so the
# constrained-BO loop (demo 59) pins `resultVar: "evaluation"`.
evaluation = {
    "sigma_max_mpa": sigma_max_mpa,
    "soak_min_k": soak_min_k,
    "cycle_h": cycle_h,
    "z": z,
    "verdict": verdict,
    "source": source,
    "field_key": field_key,
}
