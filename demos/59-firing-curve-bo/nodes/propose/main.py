"""Loop body head — constrained-BO proposer for K candidate firing curves.

Searches the 3-D firing-curve space, normalized to the unit cube:

  u_ramp -> ramp_rate  = 2 + 148 * u_ramp    [K/min]  (2 .. 150)
  u_cool -> cool_rate  = 2 + 148 * u_cool    [K/min]  (2 .. 150)
  u_hold -> hold_time  = 600 + 6600 * u_hold [s]      (10 min .. 2 h)

(150 K/min reaches speed-sintering territory — wide enough that the REAL
crack boundary lies inside the box, so the optimizer stops at the material
limit, not at an arbitrary fence.)

CONSTRAINED BO (Gardner/Gelbart style), not penalized-cost BO: the cycle time
is a KNOWN closed-form function of the parameters, so nothing needs learning
there. What is unknown is the physics. Fitting one GP on a penalty-jump cost
smears the discontinuity over a wide neighborhood and EI then AVOIDS the
whole boundary region — exactly where the optimum lives. Instead:

  - GP_sigma : posterior over sigma_max [MPa]  (smooth, no cliff)
  - GP_soak  : posterior over soak_min  [K]    (smooth, no cliff)
  - acquisition(x) = max(best_feasible_cycle - cycle(x), 0)
                     * P(sigma(x) <= limit) * P(soak(x) >= target)

P(feasible) ~ 0.5 right at the learned boundary with a large improvement
factor, so the acquisition is ATTRACTED to the constraint boundary instead of
repelled. Before any feasible point exists the improvement factor is dropped
(pure feasibility search); if no candidate improves, fall back to
P(feasible) * sigma-uncertainty (probe the uncertain side of the boundary).

BATCH: Kriging Believer on the constraint GPs (append predicted sigma/soak at
each pick, refit with frozen hyperparameters) PLUS a constant-liar update on
the deterministic objective (assume the pick succeeds -> its cycle time
becomes the incumbent), so later picks can't cluster on the same spot.

IMPORTANT (compiler contract): this Python is SCANNED at compile time for slug
references, not executed. The literal source reads of `campaign.observations`,
`campaign.f_best`, `campaign.iteration`, `campaign.hold_temp`,
`campaign.sigma_limit`, and `campaign.solver_mode` below MUST remain verbatim
so the compiler synthesizes the read-arcs into the loop's parked accumulator
place. Removing or renaming any of them breaks read-arc synthesis.

FALLBACK: the BO phase needs scikit-learn + scipy (declared in
executionSpec.config.requirements). If importing them fails at runtime we DO
NOT crash the demo — we fall back to a numpy EI-lite proposer, and ultimately
a deterministic grid walk, so the Loop + Map + accumulator topology still runs
end-to-end (same philosophy as demo 12).
"""

import os
import random

# --- Read the borrowed campaign state (literal slug reads — DO NOT remove) ---
observations = campaign.observations
f_best = campaign.f_best
iteration = campaign.iteration
hold_temp = campaign.hold_temp
sigma_limit = campaign.sigma_limit
solver_mode = campaign.solver_mode
batch_k = campaign.batch_k

n_seen = len(observations)

# K=1 -> classic sequential BO (one simulation per iteration, EI argmax —
# the constant-liar loop below degenerates to it with zero lies). K>1 ->
# parallel batch dispatched data-parallel by the Map.
K = max(1, min(8, int(batch_k)))

log_info(
    f"propose: iteration={iteration} observations={n_seen} f_best={f_best} "
    f"K={K} hold_temp={hold_temp} sigma_limit={sigma_limit} solver={solver_mode}"
)

# Fixed space-filling points on the unit cube — bootstrap phase and ultimate
# deterministic fallback. Deliberately spans slow/safe, fast/cracky, and
# short/under-soaked corners so the GP sees the constraint structure early.
CANDIDATE_GRID = [
    [0.10, 0.15, 0.50],  # slow ramp, slow cool, medium hold  (safe, long)
    [0.90, 0.85, 0.50],  # fast ramp, fast cool, medium hold  (likely cracks)
    [0.50, 0.50, 0.10],  # medium rates, short hold           (under-soak risk)
    [0.30, 0.60, 0.70],
    [0.60, 0.30, 0.30],
    [0.20, 0.40, 0.90],
    [0.75, 0.50, 0.65],
    [0.45, 0.75, 0.20],
]


def _denorm(u):
    """Unit-cube point -> physical firing-curve candidate dict."""
    u_ramp = min(max(float(u[0]), 0.0), 1.0)
    u_cool = min(max(float(u[1]), 0.0), 1.0)
    u_hold = min(max(float(u[2]), 0.0), 1.0)
    return {
        "u_ramp": round(u_ramp, 6),
        "u_cool": round(u_cool, 6),
        "u_hold": round(u_hold, 6),
        "ramp_rate": round(2.0 + 148.0 * u_ramp, 2),
        "cool_rate": round(2.0 + 148.0 * u_cool, 2),
        "hold_time_s": round(600.0 + 6600.0 * u_hold, 0),
        "hold_temp": float(hold_temp),
        "sigma_limit": float(sigma_limit),
        "solver_mode": str(solver_mode),
    }


def _grid_batch(it):
    """Deterministic K-point batch: cycle CANDIDATE_GRID by iteration."""
    n = len(CANDIDATE_GRID)
    return [_denorm(CANDIDATE_GRID[(it * K + j) % n]) for j in range(K)]


# --- Bootstrap phase --------------------------------------------------------
# Only iteration 0 needs the space-filling walk (no observations yet); the
# GP phase starts as soon as >=2 observations exist (covers K=1 sequential).
if n_seen < 2 or iteration < 1:
    candidates = _grid_batch(iteration)
    log_info(f"propose: bootstrap -> {candidates}")

else:
    # --- BO phase: constraint GPs + improvement x P(feasible) acquisition ---
    candidates = None

    try:
        import numpy as np
        from sklearn.gaussian_process import GaussianProcessRegressor
        from sklearn.gaussian_process.kernels import Matern, WhiteKernel
        from scipy.stats import norm

        X = np.array(
            [
                [float(o["u_ramp"]), float(o["u_cool"]), float(o["u_hold"])]
                for o in observations
            ]
        )
        y_sigma = np.array([float(o["sigma_max_mpa"]) for o in observations])
        y_soak = np.array([float(o["soak_min_k"]) for o in observations])

        def _kernel():
            return Matern(
                nu=2.5,
                length_scale=[0.25, 0.25, 0.25],
                length_scale_bounds=(0.01, 1.0),
            ) + WhiteKernel(
                noise_level=1e-3,
                noise_level_bounds=(1e-10, 1.0),
            )

        gp_sigma = GaussianProcessRegressor(
            kernel=_kernel(), n_restarts_optimizer=5, normalize_y=True, random_state=42
        ).fit(X, y_sigma)
        gp_soak = GaussianProcessRegressor(
            kernel=_kernel(), n_restarts_optimizer=5, normalize_y=True, random_state=43
        ).fit(X, y_soak)

        soak_target = float(hold_temp) - 15.0
        xi = 0.01  # [h] minimum cycle improvement worth simulating

        def _cycle_h_of(pts):
            """Deterministic objective — known in closed form, no GP needed."""
            ramp = 2.0 + 148.0 * pts[:, 0]
            cool = 2.0 + 148.0 * pts[:, 1]
            hold = 600.0 + 6600.0 * pts[:, 2]
            dT = float(hold_temp) - 300.0
            return (dT / (ramp / 60.0) + hold + dT / (cool / 60.0)) / 3600.0

        feas = [o for o in observations if o.get("verdict") == "OK"]
        best_cycle = min(float(o["cycle_h"]) for o in feas) if feas else None

        def _acquisition(gs, gk, pts, incumbent_cycle):
            mu_s, sd_s = gs.predict(pts, return_std=True)
            sd_s = np.maximum(sd_s, 1e-9)
            mu_k, sd_k = gk.predict(pts, return_std=True)
            sd_k = np.maximum(sd_k, 1e-9)
            pf = norm.cdf((float(sigma_limit) - mu_s) / sd_s) * norm.cdf(
                (mu_k - soak_target) / sd_k
            )
            cyc = _cycle_h_of(pts)
            if incumbent_cycle is None:
                # No feasible point yet: pure feasibility search.
                imp = np.ones_like(cyc)
            else:
                imp = np.maximum(incumbent_cycle - cyc - xi, 0.0)
            acq = imp * pf
            if not np.any(acq > 0.0):
                # Nothing improves the incumbent: probe the uncertain side of
                # the constraint boundary instead of going silent.
                acq = pf * sd_s
            return acq

        # Dense random cloud over the unit cube (deterministic per iteration).
        rng = np.random.default_rng(1000 + int(iteration))
        grid = rng.random((4096, 3))

        # Batch: Kriging Believer on the constraint GPs + constant liar on the
        # deterministic objective (see module docstring).
        Xs = X.copy()
        ys_s = y_sigma.copy()
        ys_k = y_soak.copy()
        gs, gk = gp_sigma, gp_soak
        frozen_s, frozen_k = gp_sigma.kernel_, gp_soak.kernel_
        inc_cycle = best_cycle
        picked_u = []
        for _j in range(K):
            acq = _acquisition(gs, gk, grid, inc_cycle)
            idx = int(np.argmax(acq))
            pick = grid[idx].copy()
            picked_u.append(pick)
            # Believe the constraint models at the pick...
            mu_s1 = float(gs.predict(pick.reshape(1, -1))[0])
            mu_k1 = float(gk.predict(pick.reshape(1, -1))[0])
            Xs = np.vstack([Xs, pick])
            ys_s = np.append(ys_s, mu_s1)
            ys_k = np.append(ys_k, mu_k1)
            # ...and lie optimistically on the objective: assume the pick
            # succeeds, so neighbors can't improve on it anymore.
            cyc_pick = float(_cycle_h_of(pick.reshape(1, -1))[0])
            inc_cycle = cyc_pick if inc_cycle is None else min(inc_cycle, cyc_pick)
            if _j < K - 1:
                gs = GaussianProcessRegressor(
                    kernel=frozen_s, optimizer=None, normalize_y=True
                ).fit(Xs, ys_s)
                gk = GaussianProcessRegressor(
                    kernel=frozen_k, optimizer=None, normalize_y=True
                ).fit(Xs, ys_k)

        candidates = [_denorm(u) for u in picked_u]
        log_info(
            f"propose: constrained BO (improvement x P(feasible)) -> {candidates}"
        )

        # --- Interactive posterior artifact (`gp-posterior` render hint) ----
        # The frontend ships an echarts renderer for exactly this JSON shape
        # (HINT_RENDERERS['gp-posterior'] -> GpPosteriorRenderer): interactive
        # heatmaps with tooltips. One artifact per iteration + the `iteration`
        # metadata makes the artifact viewer group them into a single
        # scrubbable panel that live-updates while the campaign runs.
        # Panels now show the LEARNED CONSTRAINT: predicted sigma_max, its
        # uncertainty, and the improvement x P(feasible) acquisition, on the
        # (ramp, cool) slice at the incumbent's hold time, axes in physical
        # units. Emission is telemetry — never fails the step.
        try:
            import json as _json

            gn = 45
            gside = np.linspace(0.0, 1.0, gn)
            ggx, ggy = np.meshgrid(gside, gside)
            inc = min(observations, key=lambda o: float(o["z"]))
            slice_grid = np.column_stack(
                [ggx.ravel(), ggy.ravel(), np.full(gn * gn, float(inc["u_hold"]))]
            )
            gmu, gsd = gp_sigma.predict(slice_grid, return_std=True)
            gsd = np.maximum(gsd, 1e-9)
            gacq = _acquisition(gp_sigma, gp_soak, slice_grid, best_cycle)

            axis = [round(2.0 + 148.0 * float(u), 2) for u in gside]
            model_doc = {
                "gp_mean": [[round(v, 4) for v in row] for row in gmu.reshape(gn, gn).tolist()],
                "gp_std": [[round(v, 4) for v in row] for row in gsd.reshape(gn, gn).tolist()],
                "ei": [[round(float(v), 6) for v in row] for row in gacq.reshape(gn, gn).tolist()],
                "A_lin": axis,
                "D_lin": axis,
                "x_label": "ramp [K/min]",
                "y_label": "cool [K/min]",
                "titles": {
                    "mean": f"predicted sigma_max [MPa] (limit {float(sigma_limit):.0f})",
                    "std": "sigma_max uncertainty [MPa]",
                    "ei": "acquisition = improvement x P(feasible)",
                },
                "next_candidate": {
                    "a": candidates[0]["ramp_rate"],
                    "d": candidates[0]["cool_rate"],
                },
                "batch": [
                    {"a": c["ramp_rate"], "d": c["cool_rate"]} for c in candidates
                ],
                "n_observations": n_seen,
                "f_best_used": float(f_best),
                "slice_hold_min": round(float(inc["hold_time_s"]) / 60.0, 1),
            }
            _art_dir = os.environ.get("AITHERICON_ARTIFACTS_DIR", os.getcwd())
            _doc_path = os.path.join(_art_dir, f"gp_model_iter{int(iteration):03d}.json")
            with open(_doc_path, "w") as f:
                _json.dump(model_doc, f)
            log_artifact(
                _doc_path,
                name=f"gp_model_iter{int(iteration):03d}.json",
                category="plot",
                mime_type="application/json",
                metadata={
                    "render_hint": "gp-posterior",
                    "iteration": str(int(iteration)),
                    "kind": "bo_gp_posterior",
                },
            )
        except Exception as exc_viz:  # noqa: BLE001 — viz is telemetry
            log_warn(f"propose: gp-posterior artifact failed (non-fatal): {exc_viz!r}")

    except Exception as exc:  # noqa: BLE001 — fallback must never crash the demo
        log_warn(f"propose: GP/scipy unavailable ({exc!r}); using EI-lite fallback")
        try:
            import numpy as np

            rng = np.random.default_rng(42 + int(iteration))

            best = min(observations, key=lambda o: float(o["z"]))
            bu = np.array(
                [float(best["u_ramp"]), float(best["u_cool"]), float(best["u_hold"])]
            )

            n_samples = 512
            cand = rng.random((n_samples, 3))

            dist = np.sqrt(((cand - bu) ** 2).sum(axis=1))
            explore = rng.random(n_samples)
            score = -dist + 0.5 * explore

            order = np.argsort(score)[::-1]
            picked = []
            seen_pts = set()
            for idx in order:
                key = tuple(round(float(v), 4) for v in cand[idx])
                if key in seen_pts:
                    continue
                seen_pts.add(key)
                picked.append(_denorm(cand[idx]))
                if len(picked) >= K:
                    break
            candidates = picked
            log_info(f"propose: EI-lite fallback -> {candidates}")

        except Exception as exc2:  # noqa: BLE001
            log_warn(f"propose: numpy unavailable ({exc2!r}); random fallback")
            rnd = random.Random(42 + int(iteration))
            candidates = [
                _denorm([rnd.random(), rnd.random(), rnd.random()]) for _ in range(K)
            ]

    if not candidates:
        # Defensive: never emit an empty batch (the Map would have nothing to
        # scatter). Fall back to the deterministic grid walk.
        candidates = _grid_batch(iteration)
