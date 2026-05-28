"""Loop body head — fit a GP surrogate and propose K candidate points.

Real Bayesian-optimization proposer for the BO loop. Each iteration it reads
the accumulated campaign state off the loop accumulators:

  - `bo.observations` : list of {a, d, z} evaluated so far (z = objective, min)
  - `bo.f_best`       : incumbent (best/lowest z seen)
  - `bo.iteration`    : loop counter (control-token leaf)

and emits `candidates` — a JSON array of K=4 {a, d} dicts on the unit square
for the downstream Map to scatter through the `branin` objective.

IMPORTANT (compiler contract): this Python is SCANNED at compile time for slug
references, not executed. The literal source reads `bo.observations`,
`bo.f_best`, and `bo.iteration` below MUST remain verbatim so the compiler
synthesizes the read-arcs into the loop's parked accumulator place. Removing or
renaming any of them breaks read-arc synthesis.

Accumulator-init note: the campaign seed is merged at the loop accumulator init
(`observations` ← `input.observations`, `f_best` ← `input.f_best`), NOT in this
body — so this proposer only consumes `bo.*`; it does not re-merge the seed.

FALLBACK: the BO phase needs scikit-learn + scipy (declared in this node's
executionSpec.config.requirements; the first run pays a one-time `uv pip
install`, cached thereafter). If importing sklearn/scipy fails at runtime
(install hiccup, offline sandbox, etc.) we DO NOT crash the demo — we fall back
to a numpy/stdlib EI-lite proposer (and ultimately a deterministic grid walk)
so the Loop + Map + accumulator topology still runs end-to-end. The fallback is
gated on the import inside the BO phase below.
"""

import math
import random

# --- Read the borrowed campaign state (literal slug reads — DO NOT remove) ---
# These three reads are what the compiler scans to synthesize the read-arcs
# into the loop's parked accumulator place. Keep them verbatim.
observations = bo.observations
f_best = bo.f_best
iteration = bo.iteration

n_seen = len(observations)

log_info(
    f"propose: iteration={iteration} observations={n_seen} f_best={f_best}"
)

K = 4

# A fixed seed grid on the unit square, used for the bootstrap phase and as the
# ultimate deterministic fallback if both the GP and EI-lite paths are unavailable.
CANDIDATE_GRID = [
    [0.5, 0.5],
    [0.25, 0.75],
    [0.75, 0.25],
    [0.1, 0.9],
    [0.9, 0.1],
    [0.4, 0.6],
    [0.6, 0.4],
    [0.15, 0.5],
]


def _grid_batch(it):
    """Deterministic K-point batch: cycle CANDIDATE_GRID by iteration."""
    n = len(CANDIDATE_GRID)
    out = []
    for j in range(K):
        p = CANDIDATE_GRID[(it * K + j) % n]
        out.append({"a": float(p[0]), "d": float(p[1])})
    return out


# --- Bootstrap phase --------------------------------------------------------
# Not enough data to fit a meaningful surrogate yet: walk the seed grid.
if n_seen < 4 or iteration < 1:
    candidates = _grid_batch(iteration)
    log_info(f"propose: bootstrap -> {candidates}")

else:
    # --- BO phase: GP fit + Expected Improvement over a dense grid ----------
    candidates = None

    try:
        import numpy as np
        from sklearn.gaussian_process import GaussianProcessRegressor
        from sklearn.gaussian_process.kernels import Matern, WhiteKernel
        from scipy.stats import norm

        X = np.array([[float(o["a"]), float(o["d"])] for o in observations])
        y = np.array([float(o["z"]) for o in observations])

        kernel = Matern(
            nu=2.5,
            length_scale=[0.2, 0.2],
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

        # Dense 80x80 grid over the unit square.
        side = np.linspace(0.0, 1.0, 80)
        gx, gy = np.meshgrid(side, side)
        grid = np.column_stack([gx.ravel(), gy.ravel()])

        mu, sigma = gp.predict(grid, return_std=True)

        # Expected Improvement for MINIMIZATION.
        xi = 0.01
        imp = f_best - mu - xi
        sigma = np.maximum(sigma, 1e-9)
        Z = imp / sigma
        ei = imp * norm.cdf(Z) + sigma * norm.pdf(Z)

        # Top-K DISTINCT grid points by EI.
        order = np.argsort(ei)[::-1]
        picked = []
        seen_pts = set()
        for idx in order:
            a = round(float(grid[idx, 0]), 6)
            d = round(float(grid[idx, 1]), 6)
            key = (a, d)
            if key in seen_pts:
                continue
            seen_pts.add(key)
            picked.append({"a": a, "d": d})
            if len(picked) >= K:
                break

        candidates = picked
        log_info(f"propose: GP+EI -> {candidates}")

    except Exception as exc:  # noqa: BLE001 — fallback must never crash the demo
        # FALLBACK PATH: sklearn/scipy unavailable or GP fit failed. Use a
        # numpy/stdlib EI-lite proposer so the Loop+Map+accumulator topology
        # still runs. We sample many random points in the unit square and score
        # each by a cheap surrogate biased toward the incumbent, mixing in an
        # exploration term so we don't collapse onto a single point.
        log_warn(f"propose: GP/scipy unavailable ({exc!r}); using EI-lite fallback")
        try:
            import numpy as np

            rng = np.random.default_rng(42 + int(iteration))

            # Incumbent location (best observed point).
            best = min(observations, key=lambda o: float(o["z"]))
            ba, bd = float(best["a"]), float(best["d"])

            n_samples = 512
            cand = rng.random((n_samples, 2))

            # Cheap acquisition: exploit near incumbent, explore away from it.
            dist = np.sqrt((cand[:, 0] - ba) ** 2 + (cand[:, 1] - bd) ** 2)
            explore = rng.random(n_samples)
            score = -dist + 0.5 * explore

            order = np.argsort(score)[::-1]
            picked = []
            seen_pts = set()
            for idx in order:
                a = round(float(cand[idx, 0]), 6)
                d = round(float(cand[idx, 1]), 6)
                key = (a, d)
                if key in seen_pts:
                    continue
                seen_pts.add(key)
                picked.append({"a": a, "d": d})
                if len(picked) >= K:
                    break
            candidates = picked
            log_info(f"propose: EI-lite fallback -> {candidates}")

        except Exception as exc2:  # noqa: BLE001
            # Last-resort fallback: random points in the unit square (pure
            # stdlib). Guarantees the demo still produces K candidates.
            log_warn(f"propose: numpy unavailable ({exc2!r}); random-in-grid fallback")
            rnd = random.Random(42 + int(iteration))
            candidates = [
                {"a": round(rnd.random(), 6), "d": round(rnd.random(), 6)}
                for _ in range(K)
            ]

    if not candidates:
        # Defensive: never emit an empty batch (the Map would have nothing to
        # scatter). Fall back to the deterministic grid walk.
        candidates = _grid_batch(iteration)
