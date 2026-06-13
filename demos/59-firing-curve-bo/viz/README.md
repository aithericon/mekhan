# Field cut-through renderer (`aithericon-pvrender`)

Headless VTK/PyVista renderer that turns an OpenFOAM `solidDisplacementFoam`
run into a **cut-through animation** — a radial (r, z) cross-section of the puck
colored by temperature **T [K]** and von-Mises stress **σ [MPa]**, animated over
the firing cycle, written as an `mp4`.

The `simulate` node calls this on the **docker solver path** (after `foamToVTK`
exports the field series), so each evaluation produces a `cutthrough_*.mp4` media
card on the process Overview tab alongside the firing-curve plot. It is
**best-effort**: if this image isn't built, the render is skipped and the
campaign runs unchanged.

## Build (once)

```bash
docker build -t aithericon-pvrender:dev demos/59-firing-curve-bo/viz
```

Native on both arm64 (Apple Silicon) and amd64. Rendering is offscreen via
**Xvfb + Mesa software GL (llvmpipe)** — no GPU, no host X server. (`vtk-osmesa`
has no arm64 wheel, hence the Xvfb route over the regular `vtk` wheel.)

## Run standalone

```bash
docker run --rm -v /path/to/case:/work aithericon-pvrender:dev \
  python -u /work/render_slice.py /work /work/cutthrough.mp4
```

`render_slice.py` here is the **canonical** copy. The same script is embedded as
`RENDER_SCRIPT` in `../nodes/simulate/main.py` (the executor ships only the node
entrypoint, so the node writes the script into the case dir at runtime). Keep the
two byte-identical.
