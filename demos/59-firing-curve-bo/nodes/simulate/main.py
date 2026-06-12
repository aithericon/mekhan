"""Map body — evaluate ONE candidate firing curve with a real OpenFOAM run.

Reads the per-candidate firing curve off the token-resident itemVar (`cand.*`
— no read-arc, same as demo 12's branin body), then:

  1. generates a complete solidDisplacementFoam case (zirconia 3Y-TZP puck,
     Ø60 x 12 mm, quarter symmetry, kiln-curve table BC) in the run directory
     — byte-identical to the cluster-validated template in this demo's
     cluster/puck-firing/, with only system/caseParams templated;
  2. runs blockMesh + solidDisplacementFoam in the multi-arch
     `opencfd/openfoam-default:2506` Docker image (the run dir is on the host,
     bind-mounted into the container);
  3. extracts the campaign objectives: peak von-Mises stress over the cycle
     (solver log), soak completeness = min core temperature during the hold
     window (fieldMinMax function object), and total cycle time;
  4. renders a firing-curve + stress-trace PNG and registers it via
     `log_artifact(category="plot", mime_type="image/png")` so each evaluation
     shows up as a renderable media card on the process Overview tab;
  5. emits `obs` — the observation dict the Map lifts as the gathered element.

SOLVER MODES (cand.solver_mode, threaded from the Start form by `propose`):
  - "docker"    : require Docker; raise on failure (real-physics-or-bust).
  - "surrogate" : skip Docker entirely; closed-form surrogate (below).
  - "auto"      : try Docker, fall back to the surrogate on ANY failure
                  (no docker binary, no image + no network, ...) so the demo
                  topology never hard-fails on an unprovisioned machine.

The surrogate is calibrated against three cluster-validated OpenFOAM runs on
RPTU Elwetritsch (rates 10/20/60 K/min -> sigma_max 18.5/29.4/103 MPa, soak
deficits ~2/7/25 K at 1 h hold). It exists so CI and docker-less machines can
exercise the full Loop+Map+BO topology; observations it produces are flagged
`"source": "surrogate"` and its plot is labeled accordingly.

The cost (MINIMIZED by the BO loop) is deliberate:
    z = cycle_hours
        + (0 if sigma <= limit else 25 + 50 * (sigma - limit) / limit)
        + (0 if soak ok    else 10 + 2 * soak_deficit_K)
A cracked puck or an under-sintered core is a scrapped part — both penalties
jump discontinuously (a part is scrapped or it isn't) and then grow, so the
optimum is the FASTEST cycle that keeps sigma_max under the crack threshold
AND soaks the core to within 15 K of the hold temperature.
"""

import json
import math
import os
import re
import shutil
import subprocess

# --- Per-candidate firing curve (token-resident itemVar reads) ---------------
ramp_rate = float(cand.ramp_rate)        # K/min
cool_rate = float(cand.cool_rate)        # K/min
hold_time_s = float(cand.hold_time_s)    # s
hold_temp = float(cand.hold_temp)        # K
sigma_limit = float(cand.sigma_limit)    # MPa
solver_mode = str(cand.solver_mode)
u_ramp = float(cand.u_ramp)
u_cool = float(cand.u_cool)
u_hold = float(cand.u_hold)

T0 = 300.0
N_STEPS = 800

t_ramp_end = (hold_temp - T0) / (ramp_rate / 60.0)
t_hold_end = t_ramp_end + hold_time_s
t_end = t_hold_end + (hold_temp - T0) / (cool_rate / 60.0)
cycle_h = t_end / 3600.0

log_info(
    f"simulate: ramp={ramp_rate} K/min hold={hold_time_s:.0f}s@{hold_temp:.0f}K "
    f"cool={cool_rate} K/min -> cycle {cycle_h:.2f} h (mode={solver_mode})"
)

DOCKER_IMAGE = "opencfd/openfoam-default:2506"

_run_dir = os.environ.get("AITHERICON_RUN_DIR", os.getcwd())
_artifacts_dir = os.environ.get("AITHERICON_ARTIFACTS_DIR", _run_dir)
case_dir = os.path.join(_run_dir, "case")


# --- 1. Case generation -------------------------------------------------------
# The full validated case, embedded. Only `system/caseParams` carries the
# candidate's parameters — every other file is byte-stable across runs (the
# "single templating seam" the Slurm sbatch wrapper in cluster/jobs/ shares).

def _foam_header(cls, obj):
    return (
        "FoamFile\n{\n"
        "    version     2.0;\n"
        "    format      ascii;\n"
        f"    class       {cls};\n"
        f"    object      {obj};\n"
        "}\n"
    )


CASE_FILES = {
    "system/caseParams": _foam_header("dictionary", "caseParams")
    + f"""
rampRate        {ramp_rate};
holdTemp        {hold_temp};
holdTime        {hold_time_s};
coolRate        {cool_rate};

T0              {T0};
nSteps          {N_STEPS};
nWrites         20;

tRampEnd        #eval{{ ($holdTemp - $T0) / ($rampRate / 60.0) }};
tHoldEnd        #eval{{ $tRampEnd + $holdTime }};
tEnd            #eval{{ $tHoldEnd + ($holdTemp - $T0) / ($coolRate / 60.0) }};
dt              #eval{{ $tEnd / $nSteps }};
writeInt        #eval{{ $tEnd / $nWrites }};
""",
    "system/blockMeshDict": _foam_header("dictionary", "blockMeshDict")
    + """
// Quarter cylinder (puck): R=30mm, H=12mm (scale 1.5 on base R=20mm coords).
scale   1.5;

vertices
(
    (0          0          0)
    (0.008      0          0)
    (0.02       0          0)
    (0.0141421  0.0141421  0)
    (0.008      0.008      0)
    (0          0.008      0)
    (0          0.02       0)
    (0          0          0.008)
    (0.008      0          0.008)
    (0.02       0          0.008)
    (0.0141421  0.0141421  0.008)
    (0.008      0.008      0.008)
    (0          0.008      0.008)
    (0          0.02       0.008)
);

blocks
(
    hex (0 1 4 5  7 8 11 12)   (8 8 10) simpleGrading (1 1 1)
    hex (1 2 3 4  8 9 10 11)   (8 8 10) simpleGrading (1 1 1)
    hex (5 4 3 6  12 11 10 13) (8 8 10) simpleGrading (1 1 1)
);

edges
(
    arc  2  3 (0.0184776 0.0076537 0)
    arc  9 10 (0.0184776 0.0076537 0.008)
    arc  3  6 (0.0076537 0.0184776 0)
    arc 10 13 (0.0076537 0.0184776 0.008)
);

boundary
(
    symX
    {
        type symmetryPlane;
        faces ((0 1 8 7) (1 2 9 8));
    }
    symY
    {
        type symmetryPlane;
        faces ((0 7 12 5) (5 12 13 6));
    }
    outer
    {
        type patch;
        faces ((2 3 10 9) (3 6 13 10));
    }
    bottom
    {
        type patch;
        faces ((0 5 4 1) (1 4 3 2) (5 6 3 4));
    }
    top
    {
        type patch;
        faces ((7 8 11 12) (8 9 10 11) (12 11 10 13));
    }
);
""",
    "system/controlDict": _foam_header("dictionary", "controlDict")
    + """
#include "caseParams"

application     solidDisplacementFoam;
startFrom       startTime;
startTime       0;
stopAt          endTime;
endTime         $tEnd;
deltaT          $dt;
writeControl    runTime;
writeInterval   $writeInt;
purgeWrite      0;
writeFormat     ascii;
writePrecision  6;
writeCompression off;
timeFormat      general;
timePrecision   6;
runTimeModifiable true;

functions
{
    minMaxT
    {
        type            fieldMinMax;
        libs            (fieldFunctionObjects);
        fields          (T);
        writeControl    timeStep;
        writeInterval   1;
        log             false;
    }
}
""",
    "system/fvSchemes": _foam_header("dictionary", "fvSchemes")
    + """
// Quasi-static stress (steadyState d2dt2) + transient heat conduction.
d2dt2Schemes  { default steadyState; }
ddtSchemes    { default Euler; }
gradSchemes
{
    default         leastSquares;
    grad(D)         leastSquares;
    grad(T)         leastSquares;
}
divSchemes
{
    default         none;
    div(sigmaD)     Gauss linear;
}
laplacianSchemes
{
    default         none;
    laplacian(DD,D) Gauss linear corrected;
    laplacian(DT,T) Gauss linear corrected;
}
interpolationSchemes { default linear; }
snGradSchemes        { default none; }
""",
    "system/fvSolution": _foam_header("dictionary", "fvSolution")
    + """
solvers
{
    "(D|T)"
    {
        solver          GAMG;
        tolerance       1e-06;
        relTol          0.9;
        smoother        GaussSeidel;
        nCellsInCoarsestLevel 20;
    }
}

stressAnalysis
{
    compactNormalStress yes;
    nCorrectors     1;
    D               1e-06;
}
""",
    "constant/mechanicalProperties": _foam_header("dictionary", "mechanicalProperties")
    + """
// Zirconia (3Y-TZP).
rho { type uniform; value 6050; }
nu  { type uniform; value 0.30; }
E   { type uniform; value 2.0e+11; }
planeStress     no;
""",
    "constant/thermalProperties": _foam_header("dictionary", "thermalProperties")
    + """
// Zirconia (3Y-TZP); thermal stress coupling ON.
C     { type uniform; value 460; }
k     { type uniform; value 2.0; }
alpha { type uniform; value 1.0e-05; }
thermalStress   yes;
""",
    "0/T": _foam_header("volScalarField", "T")
    + """
#include "../system/caseParams"

dimensions      [0 0 0 1 0 0 0];
internalField   uniform $T0;

boundaryField
{
    symX { type symmetryPlane; }
    symY { type symmetryPlane; }
    "(outer|top|bottom)"
    {
        type            uniformFixedValue;
        uniformValue    table
        (
            (0          $T0)
            ($tRampEnd  $holdTemp)
            ($tHoldEnd  $holdTemp)
            ($tEnd      $T0)
        );
    }
}
""",
    "0/D": _foam_header("volVectorField", "D")
    + """
dimensions      [0 1 0 0 0 0 0];
internalField   uniform (0 0 0);

boundaryField
{
    symX { type symmetryPlane; }
    symY { type symmetryPlane; }
    "(outer|top|bottom)"
    {
        type            tractionDisplacement;
        traction        uniform (0 0 0);
        pressure        uniform 0;
        value           uniform (0 0 0);
    }
}
""",
}


def _write_case():
    if os.path.isdir(case_dir):
        shutil.rmtree(case_dir, ignore_errors=True)
    for rel, content in CASE_FILES.items():
        path = os.path.join(case_dir, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(content)


# --- 2./3. Docker run + extraction --------------------------------------------

def _run_docker():
    """Run the case in the OpenFOAM container; return parsed results."""
    _write_case()
    # `bash -lc` is REQUIRED: the opencfd image sources the OpenFOAM
    # environment from the login profile. That profile also `cd`s to $HOME,
    # clobbering `docker -w` — so the explicit `cd /case` inside the script
    # is load-bearing, not belt-and-braces.
    cmd = [
        "docker", "run", "--rm",
        "-v", f"{case_dir}:/case",
        DOCKER_IMAGE,
        "bash", "-lc",
        "cd /case && blockMesh > log.blockMesh 2>&1 && "
        "solidDisplacementFoam > log.solidDisplacementFoam 2>&1",
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=900)
    if proc.returncode != 0:
        tail = ""
        log_path = os.path.join(case_dir, "log.solidDisplacementFoam")
        if os.path.exists(log_path):
            with open(log_path) as f:
                tail = "".join(f.readlines()[-25:])
        raise RuntimeError(
            f"docker/OpenFOAM failed rc={proc.returncode}: "
            f"{proc.stderr[-500:]}\n{tail}"
        )

    # Peak von Mises over the cycle + full trace, from the solver log.
    # v2506 solidDisplacementFoam prints `Iteration: <t>` (NOT `Time =`)
    # followed by `Max sigmaEq = <Pa>` per step.
    sigma_t, sigma_v = [], []
    t_cur = None
    with open(os.path.join(case_dir, "log.solidDisplacementFoam")) as f:
        for line in f:
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

    # Core temperature trace (min T over the mesh) from the fieldMinMax FO.
    # Column layout (proven on the cluster): col 0 = time, col 2 = min.
    core_t, core_v = [], []
    import glob as _glob

    for dat in sorted(
        _glob.glob(os.path.join(case_dir, "postProcessing/minMaxT/*/fieldMinMax.dat"))
    ):
        with open(dat) as f:
            for line in f:
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


# --- Surrogate fallback ---------------------------------------------------------

def _run_surrogate():
    """Closed-form stand-in calibrated against the cluster-validated runs.

    Calibration anchors (Elwetritsch zr runs, hold @ 1850 K; ramp/hold/cool):
      5 / 3600 / 10  -> 18.49 MPa, soak_min 1848.47 K
      20 / 1800 / 30 -> 29.39 MPa, soak_min 1842.80 K
      60 /  600 /120 -> 103.2 MPa, soak_min 1825.25 K
    sigma is rate-driven (peak at end-of-cool): power-law on max(ramp, cool).
    soak_min is the core temperature at hold START (ramp-lag), so it depends
    on ramp only — exact 3-point fit: deficit = 0.256 * ramp^1.11 (hold time
    doesn't move the window MIN, only how long the core stays soaked).
    """
    r_eff = max(ramp_rate, cool_rate)
    dT = hold_temp - T0
    sigma_max_mpa = 18.5 * (r_eff / 10.0) ** 0.69 * (dT / 1550.0)
    soak_deficit = 0.256 * ramp_rate**1.11
    soak_min = hold_temp - soak_deficit

    # Synthesize plausible traces so the plot still tells the story:
    # stress rises with the ramp, relaxes during the hold, peaks at end-of-cool
    # (the validated runs peak at the outer rim at end-of-cool).
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


# --- Dispatch by solver mode ----------------------------------------------------
result = None
if solver_mode == "surrogate":
    result = _run_surrogate()
else:
    try:
        result = _run_docker()
    except Exception as exc:  # noqa: BLE001
        if solver_mode == "docker":
            raise  # real-physics-or-bust: route out the error port
        log_warn(f"simulate: docker path failed ({exc!r}); surrogate fallback")
        result = _run_surrogate()

sigma_max_mpa = float(result["sigma_max_mpa"])
soak_min_k = float(result["soak_min_k"])

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
    f"simulate[{result['source']}]: sigma_max={sigma_max_mpa:.1f} MPa "
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

    src_tag = " (surrogate)" if result["source"] == "surrogate" else ""
    fig.suptitle(
        f"ramp {ramp_rate:.0f} K/min · hold {hold_time_s / 60.0:.0f} min @ "
        f"{hold_temp:.0f} K · cool {cool_rate:.0f} K/min{src_tag}\n"
        f"sigma_max {sigma_max_mpa:.1f} MPa · soak min {soak_min_k:.0f} K · "
        f"cycle {cycle_h:.2f} h — {verdict} (cost {z:.2f})",
        fontsize=10,
    )
    fig.tight_layout(rect=(0, 0, 1, 0.91))

    png_name = (
        f"firing_r{ramp_rate:.0f}_h{hold_time_s:.0f}_c{cool_rate:.0f}.png"
    )
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
            "source": result["source"],
        },
    )
except Exception as exc:  # noqa: BLE001 — plotting is telemetry, not physics
    log_warn(f"simulate: plot/artifact failed (non-fatal): {exc!r}")

# Persist the raw extraction next to the case for debugging.
try:
    with open(os.path.join(_run_dir, "results.json"), "w") as f:
        json.dump(
            {k: v for k, v in result.items() if k != "trace"}
            | {"z": z, "cycle_h": cycle_h, "verdict": verdict},
            f,
            indent=2,
        )
except Exception:  # noqa: BLE001
    pass

# --- 6. Observation (the Map's resultVar) ----------------------------------------
obs = {
    "u_ramp": u_ramp,
    "u_cool": u_cool,
    "u_hold": u_hold,
    "ramp_rate": ramp_rate,
    "cool_rate": cool_rate,
    "hold_time_s": hold_time_s,
    "sigma_max_mpa": round(sigma_max_mpa, 3),
    "soak_min_k": round(soak_min_k, 2),
    "cycle_h": round(cycle_h, 4),
    "source": result["source"],
    "verdict": verdict,
    "z": round(z, 4),
}
