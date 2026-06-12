"""Post-loop reporter — animate the Gaussian-process surrogate itself.

Runs ONCE after the campaign loop exits. Reads the full ordered observation
history off the loop accumulators and re-fits the same GP the proposer used on
each iteration's observation PREFIX, rendering one frame per iteration:

  +----------------+----------------+----------------+
  | posterior mean | posterior sigma| expected impr. |   (ramp, cool) slice
  |  mu(ramp,cool) |  (uncertainty) |  (acquisition) |   at the incumbent's
  +----------------+----------------+----------------+   hold time
  | mu(ramp,hold)  | where we looked| convergence    |
  | slice @ cool*  | obs scatter    | best-so-far    |
  +----------------+----------------+----------------+

The frames are stacked into an animated GIF (`gp_evolution.gif`) — the
"watch the surrogate learn the landscape" artifact — plus the final frame as
a standalone PNG and emitted via log_artifact (image/* MIME -> renderable
media cards on the process Overview tab, same as the per-run firing plots).

Why slices: the search space is 3-D (ramp, cool, hold). The physics makes
(ramp, cool) the load-bearing plane — sigma_max is rate-driven and the soak
constraint is ramp-driven — so the top row slices the cube at the incumbent's
hold time; the second row adds a (ramp, hold) slice at the incumbent's cool
rate so all three dimensions are visible.

IMPORTANT (compiler contract): the literal reads of `campaign.observations`,
`campaign.f_best`, `campaign.best_ramp`, `campaign.best_cool`,
`campaign.best_hold`, `campaign.sigma_limit`, and `campaign.hold_temp` below
are the compiler's read-arc anchors into the loop's parked accumulator place
— keep them verbatim.

FALLBACK LADDER (never fails the workflow): no sklearn -> scatter +
convergence only (no GP panels); no matplotlib/pillow -> summary output only.
"""

import io
import os

# --- Borrowed campaign state (literal slug reads — DO NOT remove) ------------
observations = campaign.observations
f_best = campaign.f_best
best_ramp = campaign.best_ramp
best_cool = campaign.best_cool
best_hold = campaign.best_hold
sigma_limit = campaign.sigma_limit
hold_temp = campaign.hold_temp

K = 3  # candidates per iteration — matches propose; prefixes chunk by K

n_obs = len(observations)
n_iters = max(1, (n_obs + K - 1) // K)
feasible = [o for o in observations if o.get("verdict") == "OK"]

log_info(
    f"report: {n_obs} observations over {n_iters} iterations, "
    f"{len(feasible)} feasible, f_best={f_best}"
)

_artifacts_dir = os.environ.get(
    "AITHERICON_ARTIFACTS_DIR", os.environ.get("AITHERICON_RUN_DIR", os.getcwd())
)

VERDICT_COLOR = {"OK": "#16a34a", "CRACK RISK": "#dc2626", "UNDER-SOAKED": "#d97706"}


def _denorm_axes():
    """Physical-unit extents for the unit-cube axes."""
    return (2.0, 60.0), (2.0, 60.0), (600.0, 7200.0)  # ramp, cool, hold


def _fit_gp(obs):
    """Same surrogate family as the proposer (Matern-5/2 + white noise)."""
    import numpy as np
    from sklearn.gaussian_process import GaussianProcessRegressor
    from sklearn.gaussian_process.kernels import Matern, WhiteKernel

    X = np.array([[o["u_ramp"], o["u_cool"], o["u_hold"]] for o in obs])
    y = np.array([float(o["z"]) for o in obs])
    kernel = Matern(
        nu=2.5, length_scale=[0.25, 0.25, 0.25], length_scale_bounds=(0.01, 1.0)
    ) + WhiteKernel(noise_level=1e-3, noise_level_bounds=(1e-10, 1.0))
    gp = GaussianProcessRegressor(
        kernel=kernel, n_restarts_optimizer=3, normalize_y=True, random_state=42
    )
    gp.fit(X, y)
    return gp


def _render_frame(obs_prefix, it):
    """Render one campaign-state frame; returns a PIL Image."""
    import numpy as np
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    from PIL import Image
    from scipy.stats import norm

    (r_lo, r_hi), (c_lo, c_hi), (h_lo, h_hi) = _denorm_axes()

    inc = min(obs_prefix, key=lambda o: float(o["z"]))
    fb = float(inc["z"])
    u_hold_star = float(inc["u_hold"])
    u_cool_star = float(inc["u_cool"])

    gp = None
    try:
        if len(obs_prefix) >= 4:
            gp = _fit_gp(obs_prefix)
    except Exception as exc:  # noqa: BLE001
        log_warn(f"report: GP fit failed for frame {it} ({exc!r})")

    n = 55
    side = np.linspace(0.0, 1.0, n)
    gx, gy = np.meshgrid(side, side)

    fig, axes = plt.subplots(2, 3, figsize=(13.5, 8.2))
    ramp_ext = [r_lo, r_hi, c_lo, c_hi]

    def scatter_obs(ax, xs_key="ramp_rate", ys_key="cool_rate"):
        for o in obs_prefix:
            ax.plot(
                o[xs_key], o[ys_key], "o", ms=5,
                color=VERDICT_COLOR.get(o.get("verdict", ""), "#64748b"),
                mec="white", mew=0.5,
            )
        ax.plot(
            inc[xs_key], inc[ys_key], "*", ms=16, color="#facc15",
            mec="black", mew=0.8, zorder=5,
        )

    if gp is not None:
        # Top row: (ramp, cool) slice at the incumbent's hold.
        grid_rc = np.column_stack(
            [gx.ravel(), gy.ravel(), np.full(n * n, u_hold_star)]
        )
        mu, sd = gp.predict(grid_rc, return_std=True)
        sd = np.maximum(sd, 1e-9)
        imp = fb - mu - 0.01
        zz = imp / sd
        ei = imp * norm.cdf(zz) + sd * norm.pdf(zz)

        panels = [
            (mu, "posterior mean  $\\mu(ramp, cool)$  [cost]", "viridis"),
            (sd, "posterior $\\sigma$ (uncertainty)", "magma"),
            (np.maximum(ei, 0.0), "Expected Improvement (next look)", "cividis"),
        ]
        for ax, (vals, title, cmap) in zip(axes[0], panels):
            im = ax.imshow(
                vals.reshape(n, n), origin="lower", extent=ramp_ext,
                aspect="auto", cmap=cmap,
            )
            fig.colorbar(im, ax=ax, fraction=0.046, pad=0.03)
            scatter_obs(ax)
            ax.set_title(title, fontsize=9)
            ax.set_xlabel("ramp [K/min]", fontsize=8)
            ax.set_ylabel("cool [K/min]", fontsize=8)

        # Bottom-left: (ramp, hold) slice at the incumbent's cool rate.
        grid_rh = np.column_stack(
            [gx.ravel(), np.full(n * n, u_cool_star), gy.ravel()]
        )
        mu2, _ = gp.predict(grid_rh, return_std=True)
        ax = axes[1][0]
        im = ax.imshow(
            mu2.reshape(n, n), origin="lower",
            extent=[r_lo, r_hi, h_lo / 60.0, h_hi / 60.0],
            aspect="auto", cmap="viridis",
        )
        fig.colorbar(im, ax=ax, fraction=0.046, pad=0.03)
        for o in obs_prefix:
            ax.plot(
                o["ramp_rate"], o["hold_time_s"] / 60.0, "o", ms=5,
                color=VERDICT_COLOR.get(o.get("verdict", ""), "#64748b"),
                mec="white", mew=0.5,
            )
        ax.plot(
            inc["ramp_rate"], inc["hold_time_s"] / 60.0, "*", ms=16,
            color="#facc15", mec="black", mew=0.8, zorder=5,
        )
        ax.set_title("$\\mu(ramp, hold)$ @ incumbent cool", fontsize=9)
        ax.set_xlabel("ramp [K/min]", fontsize=8)
        ax.set_ylabel("hold [min]", fontsize=8)
    else:
        for ax in list(axes[0]) + [axes[1][0]]:
            ax.text(
                0.5, 0.5, "GP pending\n(bootstrap phase)", ha="center",
                va="center", fontsize=10, color="#94a3b8",
                transform=ax.transAxes,
            )
            ax.set_xticks([])
            ax.set_yticks([])

    # Bottom-middle: where we looked (all observations, verdict-colored).
    ax = axes[1][1]
    scatter_obs(ax)
    ax.set_xlim(r_lo, r_hi)
    ax.set_ylim(c_lo, c_hi)
    ax.set_title("evaluations so far (green OK / red crack / orange soak)", fontsize=9)
    ax.set_xlabel("ramp [K/min]", fontsize=8)
    ax.set_ylabel("cool [K/min]", fontsize=8)
    ax.grid(alpha=0.25)

    # Bottom-right: convergence (best-so-far cost per evaluation).
    ax = axes[1][2]
    zs = [float(o["z"]) for o in obs_prefix]
    best_so_far, cur = [], float("inf")
    for z in zs:
        cur = min(cur, z)
        best_so_far.append(cur)
    ax.plot(range(1, len(zs) + 1), zs, "o", ms=4, color="#94a3b8", label="evaluation")
    ax.step(
        range(1, len(best_so_far) + 1), best_so_far, where="post",
        color="#2563eb", lw=2, label="best so far",
    )
    ax.set_title(f"convergence — best cost {fb:.3f}", fontsize=9)
    ax.set_xlabel("evaluation #", fontsize=8)
    ax.set_ylabel("cost [h + penalties]", fontsize=8)
    ax.legend(fontsize=7)
    ax.grid(alpha=0.25)

    fig.suptitle(
        f"Firing-curve BO — surrogate state after iteration {it + 1} "
        f"({len(obs_prefix)} simulations)",
        fontsize=12,
    )
    fig.tight_layout(rect=(0, 0, 1, 0.95))

    buf = io.BytesIO()
    fig.savefig(buf, format="png", dpi=92)
    plt.close(fig)
    buf.seek(0)
    return Image.open(buf).convert("RGB")


frames_rendered = 0
try:
    frames = []
    for it in range(n_iters):
        prefix = observations[: K * (it + 1)]
        if not prefix:
            continue
        frames.append(_render_frame(prefix, it))
        log_info(f"report: rendered frame {it + 1}/{n_iters}")

    if frames:
        # Final state as a crisp standalone PNG.
        final_png = os.path.join(_artifacts_dir, "gp_final_state.png")
        frames[-1].save(final_png)
        log_artifact(
            final_png, name="gp_final_state.png", category="plot",
            mime_type="image/png",
            metadata={"kind": "bo_surrogate_final", "evaluations": str(n_obs)},
        )

        # The animation: hold the last frame a few beats so the loop reads
        # (per-frame durations — Pillow dedupes appended identical frames).
        gif_path = os.path.join(_artifacts_dir, "gp_evolution.gif")
        frames[0].save(
            gif_path,
            save_all=True,
            append_images=frames[1:],
            duration=[1100] * (len(frames) - 1) + [4000],
            loop=0,
        )
        log_artifact(
            gif_path, name="gp_evolution.gif", category="plot",
            mime_type="image/gif",
            metadata={
                "kind": "bo_surrogate_evolution",
                "frames": str(len(frames)),
                "evaluations": str(n_obs),
            },
        )
        frames_rendered = len(frames)
except Exception as exc:  # noqa: BLE001 — reporting is telemetry, not physics
    log_warn(f"report: GP animation failed (non-fatal): {exc!r}")

summary = {
    "evaluations": n_obs,
    "iterations": n_iters,
    "feasible": len(feasible),
    "frames_rendered": frames_rendered,
    "best": {
        "ramp_K_min": best_ramp,
        "cool_K_min": best_cool,
        "hold_s": best_hold,
        "cost": f_best,
    },
    "sigma_limit_mpa": sigma_limit,
    "hold_temp_k": hold_temp,
}
