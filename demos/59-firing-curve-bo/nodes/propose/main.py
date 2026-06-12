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

import random

# --- Read the borrowed campaign state (literal slug reads — DO NOT remove) ---
observations = campaign.observations
f_best = campaign.f_best
iteration = campaign.iteration
hold_temp = campaign.hold_temp
sigma_limit = campaign.sigma_limit
solver_mode = campaign.solver_mode

n_seen = len(observations)

log_info(
    f"propose: iteration={iteration} observations={n_seen} f_best={f_best} "
    f"hold_temp={hold_temp} sigma_limit={sigma_limit} solver={solver_mode}"
)

K = 3

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
if n_seen < 4 or iteration < 1:
    candidates = _grid_batch(iteration)
    log_info(f"propose: bootstrap -> {candidates}")

else:
    # --- BO phase: GP fit + Expected Improvement over a sampled cube --------
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

        mu, sigma = gp.predict(grid, return_std=True)

        # Expected Improvement for MINIMIZATION.
        xi = 0.01
        imp = f_best - mu - xi
        sigma = np.maximum(sigma, 1e-9)
        Z = imp / sigma
        ei = imp * norm.cdf(Z) + sigma * norm.pdf(Z)

        # Top-K DISTINCT points by EI.
        order = np.argsort(ei)[::-1]
        picked = []
        seen_pts = set()
        for idx in order:
            key = tuple(round(float(v), 4) for v in grid[idx])
            if key in seen_pts:
                continue
            seen_pts.add(key)
            picked.append(_denorm(grid[idx]))
            if len(picked) >= K:
                break

        candidates = picked
        log_info(f"propose: GP+EI -> {candidates}")

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
