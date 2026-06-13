"""Post-loop reporter — animate the Gaussian-process surrogate itself.

Runs ONCE after the campaign loop exits. Reads the full ordered observation
history off the loop accumulators and re-fits the same GP the proposer used on
each iteration's observation PREFIX, rendering one frame per iteration:

  +----------------+----------------+----------------+
  | predicted      | sigma_max      | acquisition    |   (ramp, cool) slice
  | sigma_max [MPa]| uncertainty    | imp x P(feas)  |   at the incumbent's
  +----------------+----------------+----------------+   hold time
  | predicted soak | where we looked| convergence    |
  | min @ cool*    | obs scatter    | best-so-far    |
  +----------------+----------------+----------------+

CONSTRAINT GPs, not a cost GP: fitting on the penalized cost would smear the
penalty cliff and distort the rendered landscape (same reason the proposer
switched to constrained BO — improvement x P(feasible) acquisition).

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
batch_k = campaign.batch_k

# Candidates per iteration — matches propose; prefixes chunk by K. K=1
# (sequential BO) animates one frame per simulation.
K = max(1, min(8, int(batch_k)))

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
    return (2.0, 150.0), (2.0, 150.0), (600.0, 7200.0)  # ramp, cool, hold


def _fit_gp(obs, values):
    """Same surrogate family as the proposer (Matern-5/2 + white noise).

    Fits on a CONSTRAINT quantity (sigma_max / soak_min) — never on the
    penalized cost: the penalty jump would smear into the GP and distort
    the rendered landscape, the same failure the proposer avoids.
    """
    import numpy as np
    from sklearn.gaussian_process import GaussianProcessRegressor
    from sklearn.gaussian_process.kernels import Matern, WhiteKernel

    X = np.array([[o["u_ramp"], o["u_cool"], o["u_hold"]] for o in obs])
    y = np.array([float(v) for v in values])
    kernel = Matern(
        nu=2.5, length_scale=[0.25, 0.25, 0.25], length_scale_bounds=(0.01, 1.0)
    ) + WhiteKernel(noise_level=1e-3, noise_level_bounds=(1e-10, 1.0))
    gp = GaussianProcessRegressor(
        kernel=kernel, n_restarts_optimizer=3, normalize_y=True, random_state=42
    )
    gp.fit(X, y)
    return gp


def _cycle_h_of(pts, ht):
    """Deterministic cycle time [h] for unit-cube points (closed form)."""
    import numpy as np

    ramp = 2.0 + 148.0 * pts[:, 0]
    cool = 2.0 + 148.0 * pts[:, 1]
    hold = 600.0 + 6600.0 * pts[:, 2]
    dT = float(ht) - 300.0
    return (dT / (ramp / 60.0) + hold + dT / (cool / 60.0)) / 3600.0


def _acquisition(gs, gk, pts, incumbent_cycle, limit, target, ht):
    """improvement x P(feasible) — mirrors the proposer's acquisition."""
    import numpy as np
    from scipy.stats import norm

    mu_s, sd_s = gs.predict(pts, return_std=True)
    sd_s = np.maximum(sd_s, 1e-9)
    mu_k, sd_k = gk.predict(pts, return_std=True)
    sd_k = np.maximum(sd_k, 1e-9)
    pf = norm.cdf((float(limit) - mu_s) / sd_s) * norm.cdf((mu_k - float(target)) / sd_k)
    cyc = _cycle_h_of(pts, ht)
    if incumbent_cycle is None:
        imp = np.ones_like(cyc)
    else:
        imp = np.maximum(incumbent_cycle - cyc - 0.01, 0.0)
    acq = imp * pf
    if not np.any(acq > 0.0):
        acq = pf * sd_s
    return acq


def _render_frame(obs_prefix, it):
    """Render one campaign-state frame; returns a PIL Image."""
    import numpy as np
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    from PIL import Image

    (r_lo, r_hi), (c_lo, c_hi), (h_lo, h_hi) = _denorm_axes()

    inc = min(obs_prefix, key=lambda o: float(o["z"]))
    fb = float(inc["z"])
    u_hold_star = float(inc["u_hold"])
    u_cool_star = float(inc["u_cool"])
    soak_target = float(hold_temp) - 15.0
    feas = [o for o in obs_prefix if o.get("verdict") == "OK"]
    best_cycle = min(float(o["cycle_h"]) for o in feas) if feas else None

    gp_s, gp_k = None, None
    try:
        if len(obs_prefix) >= 2:
            gp_s = _fit_gp(obs_prefix, [o["sigma_max_mpa"] for o in obs_prefix])
            gp_k = _fit_gp(obs_prefix, [o["soak_min_k"] for o in obs_prefix])
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

    if gp_s is not None and gp_k is not None:
        # Top row: the LEARNED CONSTRAINT on the (ramp, cool) slice at the
        # incumbent's hold — predicted sigma_max, its uncertainty, and the
        # improvement x P(feasible) acquisition (mirrors the proposer).
        grid_rc = np.column_stack(
            [gx.ravel(), gy.ravel(), np.full(n * n, u_hold_star)]
        )
        mu, sd = gp_s.predict(grid_rc, return_std=True)
        sd = np.maximum(sd, 1e-9)
        acq = _acquisition(
            gp_s, gp_k, grid_rc, best_cycle, sigma_limit, soak_target, hold_temp
        )

        panels = [
            (mu, f"predicted $\\sigma_{{max}}$ [MPa] (limit {float(sigma_limit):.0f})", "viridis"),
            (sd, "$\\sigma_{max}$ uncertainty [MPa]", "magma"),
            (acq, "acquisition = improvement $\\times$ P(feasible)", "cividis"),
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

        # Bottom-left: predicted soak_min on the (ramp, hold) slice at the
        # incumbent's cool rate — the OTHER learned constraint.
        grid_rh = np.column_stack(
            [gx.ravel(), np.full(n * n, u_cool_star), gy.ravel()]
        )
        mu2, _ = gp_k.predict(grid_rh, return_std=True)
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
        ax.set_title(
            f"predicted soak min [K] (target {soak_target:.0f})", fontsize=9
        )
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


# --- Closing posterior for the interactive gp-posterior panel ----------------
# `propose` snapshots the GP BEFORE evaluating its picks (the decision state),
# so the last interactive snapshot is always fitted on n - K observations and
# the final simulation(s) never get a panel. Emit one closing gp_model fitted
# on the FULL observation set — no next_candidate (the campaign is over), EI
# shown vs the final incumbent ("where it would look next if continued") —
# with an iteration number that sorts after every propose snapshot.
try:
    import json as _json

    import numpy as np

    if n_obs >= 2:
        gp_s_full = _fit_gp(observations, [o["sigma_max_mpa"] for o in observations])
        gp_k_full = _fit_gp(observations, [o["soak_min_k"] for o in observations])
        inc_full = min(observations, key=lambda o: float(o["z"]))
        _target = float(hold_temp) - 15.0
        _feas = [o for o in observations if o.get("verdict") == "OK"]
        _best_cycle = min(float(o["cycle_h"]) for o in _feas) if _feas else None

        gn = 45
        gside = np.linspace(0.0, 1.0, gn)
        ggx, ggy = np.meshgrid(gside, gside)
        slice_grid = np.column_stack(
            [ggx.ravel(), ggy.ravel(), np.full(gn * gn, float(inc_full["u_hold"]))]
        )
        gmu, gsd = gp_s_full.predict(slice_grid, return_std=True)
        gsd = np.maximum(gsd, 1e-9)
        gacq = _acquisition(
            gp_s_full, gp_k_full, slice_grid, _best_cycle, sigma_limit, _target, hold_temp
        )

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
            "n_observations": n_obs,
            "f_best_used": float(inc_full["z"]),
            "slice_hold_min": round(float(inc_full["hold_time_s"]) / 60.0, 1),
        }
        _doc_path = os.path.join(_artifacts_dir, "gp_model_final.json")
        with open(_doc_path, "w") as f:
            _json.dump(model_doc, f)
        log_artifact(
            _doc_path,
            name="gp_model_final.json",
            category="plot",
            mime_type="application/json",
            metadata={
                "render_hint": "gp-posterior",
                # propose snapshots carry the loop iteration counter; +2 keeps
                # this sorting strictly after the last one regardless of the
                # 0/1-based counter convention.
                "iteration": str(n_iters + 2),
                "kind": "bo_gp_posterior_final",
            },
        )
        log_info(f"report: closing posterior emitted (n={n_obs})")
except Exception as exc:  # noqa: BLE001 — telemetry, never fails the step
    log_warn(f"report: closing posterior failed (non-fatal): {exc!r}")

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

# --- Review-facing outputs (consumed by the `review` HumanTask) ---------------
# The HumanTask's blocks interpolate `{{ report.<field> }}` at instance time:
# `report_md` / `limits_md` render as mdsvex blocks, `table_rows` feeds the
# sortable results table (`rows_ref: report.table_rows`), the storage KEYS
# feed `/api/v1/files/{{ report.gp_final_key }}` image/download blocks, and
# `top_curves` drives a Repeater (`report.top_curves[*]`) with a per-curve
# "queue for physical verification" checkbox.

# Artifact storage keys are deterministic: artifacts/{execution_id}/{id}/{name}
# (observed executor-storage layout). Empty when rendering was skipped.
_exec_id = os.environ.get("AITHERICON_EXECUTION_ID", "")
if frames_rendered > 0 and _exec_id:
    gp_final_key = f"artifacts/{_exec_id}/gp_final_state.png/gp_final_state.png"
    gif_key = f"artifacts/{_exec_id}/gp_evolution.gif/gp_evolution.gif"
else:
    gp_final_key = ""
    gif_key = ""

_soak_target = float(hold_temp) - 15.0
_n_crack = sum(1 for o in observations if o.get("verdict") == "CRACK RISK")
_n_soak = sum(1 for o in observations if o.get("verdict") == "UNDER-SOAKED")


def _curve_label(o):
    return (
        f"{float(o['cycle_h']):.2f} h — ramp {float(o['ramp_rate']):.1f} / "
        f"cool {float(o['cool_rate']):.1f} K/min, hold "
        f"{float(o['hold_time_s']) / 60.0:.0f} min  "
        f"(sigma {float(o['sigma_max_mpa']):.1f} MPa, soak "
        f"{float(o['soak_min_k']):.1f} K, {o.get('verdict', '?')})"
    )


# Top candidates: feasible curves by cycle time; pad with the nearest misses
# (boundary probes) so the verification queue always has material.
_ranked = sorted(feasible, key=lambda o: float(o["cycle_h"]))[:5]
if len(_ranked) < 5:
    _misses = sorted(
        (o for o in observations if o.get("verdict") != "OK"),
        key=lambda o: float(o["z"]),
    )
    _ranked = _ranked + _misses[: 5 - len(_ranked)]

top_curves = [
    {
        "label": _curve_label(o),
        "ramp_rate": float(o["ramp_rate"]),
        "cool_rate": float(o["cool_rate"]),
        "hold_min": round(float(o["hold_time_s"]) / 60.0, 1),
        "sigma_max_mpa": float(o["sigma_max_mpa"]),
        "soak_min_k": float(o["soak_min_k"]),
        "cycle_h": float(o["cycle_h"]),
        "verdict": o.get("verdict", "?"),
    }
    for o in _ranked
]

if feasible:
    _b = min(feasible, key=lambda o: float(o["cycle_h"]))
    best_label = _curve_label(_b)
else:
    best_label = "no feasible firing curve found — consider more iterations"

# Sortable results table: the review task's `table` block resolves
# `rows_ref: report.table_rows` against the staged payload — string cells,
# one row per top curve, columns matching the block's `headers`.
table_rows = [
    [
        str(i + 1),
        f"{c['cycle_h']:.2f}",
        f"{c['ramp_rate']:.1f}",
        f"{c['cool_rate']:.1f}",
        f"{c['hold_min']:.0f}",
        f"{c['sigma_max_mpa']:.1f}",
        f"{c['soak_min_k']:.1f}",
        c["verdict"],
    ]
    for i, c in enumerate(top_curves)
]
_max_feas_sigma = max((float(o["sigma_max_mpa"]) for o in feasible), default=0.0)
_min_feas_soak = min((float(o["soak_min_k"]) for o in feasible), default=0.0)

report_md = f"""### Campaign summary

**{n_obs} simulations** across **{n_iters} iterations** — **{len(feasible)} feasible**, {_n_crack} crack-risk, {_n_soak} under-soaked. The infeasible runs are not waste: they are the boundary probes that taught the surrogate where the material limits lie.
"""

# Rendered as its own mdsvex block AFTER the sortable top-curves table.
limits_md = f"""#### How close did we get to the limits?

- Crack threshold **{float(sigma_limit):.0f} MPa** — best feasible curve peaks at **{_max_feas_sigma:.1f} MPa** ({float(sigma_limit) - _max_feas_sigma:.1f} MPa of margin).
- Soak target **{_soak_target:.0f} K** (hold temp − 15) — tightest feasible soak: **{_min_feas_soak:.1f} K**.

_Search space: ramp / cool 2–150 K/min, hold 10–120 min @ {float(hold_temp):.0f} K. Objective: minimize cycle time subject to sigma_max ≤ {float(sigma_limit):.0f} MPa and full core soak (constrained BO — improvement × P(feasible))._
"""
