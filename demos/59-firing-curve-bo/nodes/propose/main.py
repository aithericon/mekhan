"""Loop body head — fit a GP surrogate and propose K candidate firing curves.

Bayesian-optimization proposer over the 3-D firing-curve space, normalized to
the unit cube:

  u_ramp -> ramp_rate  = 2 + 58 * u_ramp     [K/min]  (2 .. 60)
  u_cool -> cool_rate  = 2 + 58 * u_cool     [K/min]  (2 .. 60)
  u_hold -> hold_time  = 600 + 6600 * u_hold [s]      (10 min .. 2 h)

Each iteration it reads the accumulated campaign state off the loop
accumulators and emits `candidates` — a JSON array of K=3 firing-curve dicts
for the downstream Map to scatter through the `simulate` objective. Campaign
constants (`hold_temp`, `sigma_limit`, `solver_mode`) are threaded INTO each
candidate so the Map body only ever touches token-resident `cand.*` fields.

BATCH ACQUISITION: the K parallel candidates are chosen by Expected
Improvement with the CONSTANT-LIAR heuristic (CL-min) — sequential EI argmax
with hallucinated `lie = f_best` outcomes appended between picks (posterior
updated, hyperparameters frozen). Plain top-K-by-EI degenerates to K copies
of the same acquisition peak; the lie collapses EI around each pick so the
batch diversifies. This is the cheap, standard q-EI stand-in.

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
        "ramp_rate": round(2.0 + 58.0 * u_ramp, 2),
        "cool_rate": round(2.0 + 58.0 * u_cool, 2),
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
    # --- BO phase: GP fit + batch Expected Improvement (Constant Liar) ------
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
        y = np.array([float(o["z"]) for o in observations])

        kernel = Matern(
            nu=2.5,
            length_scale=[0.25, 0.25, 0.25],
            length_scale_bounds=(0.01, 1.0),
        ) + WhiteKernel(
            noise_level=1e-3,
            noise_level_bounds=(1e-10, 1.0),
        )

        gp = GaussianProcessRegressor(
            kernel=kernel,
            n_restarts_optimizer=5,
            normalize_y=True,
            random_state=42,
        )
        gp.fit(X, y)

        # Dense random cloud over the unit cube (deterministic per iteration).
        rng = np.random.default_rng(1000 + int(iteration))
        grid = rng.random((4096, 3))
        xi = 0.01

        def _ei_over(model, pts, incumbent):
            """Expected Improvement for MINIMIZATION."""
            mu, sigma = model.predict(pts, return_std=True)
            sigma = np.maximum(sigma, 1e-9)
            imp = incumbent - mu - xi
            Z = imp / sigma
            return imp * norm.cdf(Z) + sigma * norm.pdf(Z)

        # Batch selection via CONSTANT LIAR (CL-min, Ginsbourger et al.):
        # plain top-K-by-EI puts all K picks on the SAME acquisition peak (EI
        # is smooth — ranks 1..K are near-identical points), wasting the
        # parallel Map budget on duplicate simulations. Instead: pick the EI
        # argmax, append it with the hallucinated outcome `lie = f_best`
        # (optimistic — kills the improvement potential around the pick),
        # refit the posterior with HYPERPARAMETERS FROZEN (optimizer=None on
        # the already-fitted kernel), and re-maximize EI for the next pick.
        # Each lie collapses EI in a neighborhood of the pick, so the batch
        # spreads across distinct regions of the acquisition landscape.
        X_aug = X.copy()
        y_aug = y.copy()
        lie = float(np.min(y))
        frozen = gp.kernel_
        picked_u = []
        model = gp
        for _j in range(K):
            ei = _ei_over(model, grid, f_best)
            idx = int(np.argmax(ei))
            picked_u.append(grid[idx].copy())
            X_aug = np.vstack([X_aug, grid[idx]])
            y_aug = np.append(y_aug, lie)
            if _j < K - 1:
                model = GaussianProcessRegressor(
                    kernel=frozen, optimizer=None, normalize_y=True, random_state=42
                )
                model.fit(X_aug, y_aug)

        candidates = [_denorm(u) for u in picked_u]
        log_info(f"propose: GP + batch-EI (constant liar) -> {candidates}")

        # --- Interactive posterior artifact (`gp-posterior` render hint) ----
        # The frontend ships an echarts renderer for exactly this JSON shape
        # (HINT_RENDERERS['gp-posterior'] -> GpPosteriorRenderer): interactive
        # mu / sigma / EI heatmaps with tooltips. One artifact per iteration +
        # the `iteration` metadata makes the artifact viewer group them into a
        # single scrubbable panel that live-updates while the campaign runs.
        # Grids are the (ramp, cool) slice at the incumbent's hold time, axes
        # in physical units. Emission is telemetry — never fails the step.
        try:
            import json as _json

            gn = 45
            gside = np.linspace(0.0, 1.0, gn)
            ggx, ggy = np.meshgrid(gside, gside)
            inc = min(observations, key=lambda o: float(o["z"]))
            slice_grid = np.column_stack(
                [ggx.ravel(), ggy.ravel(), np.full(gn * gn, float(inc["u_hold"]))]
            )
            gmu, gsd = gp.predict(slice_grid, return_std=True)
            gsd = np.maximum(gsd, 1e-9)
            gimp = f_best - gmu - xi
            gz = gimp / gsd
            gei = np.maximum(gimp * norm.cdf(gz) + gsd * norm.pdf(gz), 0.0)

            axis = [round(2.0 + 58.0 * float(u), 2) for u in gside]
            model_doc = {
                "gp_mean": [[round(v, 4) for v in row] for row in gmu.reshape(gn, gn).tolist()],
                "gp_std": [[round(v, 4) for v in row] for row in gsd.reshape(gn, gn).tolist()],
                "ei": [[round(v, 6) for v in row] for row in gei.reshape(gn, gn).tolist()],
                "A_lin": axis,
                "D_lin": axis,
                "x_label": "ramp [K/min]",
                "y_label": "cool [K/min]",
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
