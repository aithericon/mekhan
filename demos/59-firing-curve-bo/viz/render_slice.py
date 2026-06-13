"""Cut-through animation of an OpenFOAM solidDisplacementFoam puck case.

Reads the foamToVTK series (VTK/case_<N>.vtm, one per write time), slices the
quarter-symmetry puck on a plane through the z-axis (the symmetry mid-plane =
a true radial cross-section of the full puck), and renders two panels per
frame - temperature T [K] and von-Mises stress sigmaEq [MPa] - with color
scales held constant across the whole series, into an mp4.

Offscreen via Xvfb + Mesa software GL (no GPU). Run under a DISPLAY pointing at
an Xvfb screen (the aithericon-pvrender image's entrypoint provides one).

Usage: render_slice.py <case_dir> <out.mp4>
"""
import glob
import os
import re
import sys

import numpy as np
import pyvista as pv

pv.OFF_SCREEN = True

case_dir = sys.argv[1] if len(sys.argv) > 1 else "case"
out_path = sys.argv[2] if len(sys.argv) > 2 else "cutthrough.mp4"

vtms = glob.glob(os.path.join(case_dir, "VTK", "case_*.vtm"))
if not vtms:
    print("no case_*.vtm under " + case_dir + "/VTK", file=sys.stderr)
    sys.exit(2)


def tindex(p):
    m = re.search(r"case_(\d+)\.vtm$", p)
    return int(m.group(1)) if m else 0


vtms = sorted(vtms, key=tindex)


def read_step(path):
    """Return (internal UnstructuredGrid, physical_time) for one .vtm."""
    mb = pv.read(path)
    grid = None
    for key in mb.keys():
        if key and key.lower().startswith("internal") and mb[key] is not None:
            grid = mb[key]
            break
    if grid is None:
        for blk in mb:
            if blk is not None and getattr(blk, "n_points", 0):
                grid = blk
                break
    t = None
    try:
        tv = mb.field_data.get("TimeValue")
        if tv is not None and len(tv):
            t = float(tv[0])
    except Exception:
        pass
    return grid, t


# Pass 1: global color ranges (constant scale across the whole animation).
ranges = {"T": [np.inf, -np.inf], "sigma_MPa": [np.inf, -np.inf]}


def field_array(grid, name):
    a = grid.point_data.get(name)
    if a is None:
        a = grid.cell_data.get(name)
    if a is None:
        return None
    a = np.asarray(a)
    return np.linalg.norm(a, axis=1) if a.ndim > 1 else a


probe, _ = read_step(vtms[len(vtms) // 2])
if probe is None:
    print("could not extract internal block", file=sys.stderr)
    sys.exit(3)

for p in vtms:
    g, _ = read_step(p)
    if g is None:
        continue
    t = field_array(g, "T")
    s = field_array(g, "sigmaEq")
    if t is not None:
        ranges["T"] = [min(ranges["T"][0], float(np.nanmin(t))),
                       max(ranges["T"][1], float(np.nanmax(t)))]
    if s is not None:
        s = s / 1e6
        ranges["sigma_MPa"] = [min(ranges["sigma_MPa"][0], float(np.nanmin(s))),
                               max(ranges["sigma_MPa"][1], float(np.nanmax(s)))]

y_eps = probe.bounds[2] + 1e-4  # just inside symY -> internal (r,z) cut

PANELS = [
    ("T", "temperature  T  [K]", "inferno", "%.0f"),
    ("sigma_MPa", "von Mises  sigma  [MPa]", "viridis", "%.0f"),
]

pl = pv.Plotter(off_screen=True, shape=(1, 2), window_size=(1280, 540))
pl.open_movie(out_path, framerate=4)

n = 0
for p in vtms:
    grid, t = read_step(p)
    if grid is None:
        continue
    if "sigmaEq" in grid.point_data:
        grid.point_data["sigma_MPa"] = np.asarray(grid.point_data["sigmaEq"]) / 1e6
    if "sigmaEq" in grid.cell_data:
        grid.cell_data["sigma_MPa"] = np.asarray(grid.cell_data["sigmaEq"]) / 1e6
    sl = grid.slice(normal="y", origin=(0, y_eps, 0))
    if sl.n_points == 0:
        sl = grid.slice(normal="y")

    tlabel = ("t = %0.0f s" % t) if t is not None else ("step %d" % tindex(p))
    for col, (skey, title, cmap, fmt) in enumerate(PANELS):
        pl.subplot(0, col)
        if sl.n_points and skey in (set(sl.point_data) | set(sl.cell_data)):
            pl.add_mesh(
                sl, scalars=skey, cmap=cmap, clim=ranges[skey],
                show_edges=False, name="field",
                scalar_bar_args=dict(title=title, n_labels=4, fmt=fmt),
            )
        pl.add_text(tlabel, name="tlabel", font_size=10, position="upper_left")
        pl.view_xz()
        pl.camera.zoom(1.5)
    pl.write_frame()
    n += 1

pl.close()
print("wrote " + out_path + " (" + str(n) + " frames)")
