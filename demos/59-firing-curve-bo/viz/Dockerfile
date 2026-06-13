# Headless VTK/PyVista render image for the firing-curve cut-through animation.
#
# vtk-osmesa has no arm64 wheel, so we use the regular `vtk` wheel (arm64 wheels
# exist) driven by Xvfb + Mesa software GL (llvmpipe) — offscreen rendering with
# no GPU and no host X server. Runs natively on both arm64 (Apple Silicon dev)
# and amd64.
#
# Build (from repo root):
#   docker build --platform "$(uname -m | sed s/aarch64/arm64/;s/x86_64/amd64/)" \
#     -t aithericon-pvrender:dev demos/59-firing-curve-bo/viz
#
# The simulate node shells `docker run aithericon-pvrender:dev` after the solver
# (foamToVTK → cut-through mp4). If the image is absent, the render is skipped
# (the campaign still runs) — see nodes/simulate/main.py.
FROM python:3.12-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
      xvfb \
      libgl1-mesa-dri libglx-mesa0 libgl1 \
      libxrender1 libxext6 libsm6 libx11-6 \
 && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir pyvista numpy imageio imageio-ffmpeg
ENV PYVISTA_OFF_SCREEN=true \
    LIBGL_ALWAYS_SOFTWARE=1 \
    GALLIUM_DRIVER=llvmpipe \
    DISPLAY=:99
WORKDIR /work
# Start a manual Xvfb screen + GL context, then exec the render process.
# (xvfb-run wrapper hangs in this slim image; a manual Xvfb is reliable and
# needs no xauth.)
RUN printf '#!/bin/sh\nXvfb :99 -screen 0 1600x900x24 >/tmp/xvfb.log 2>&1 &\nfor i in $(seq 1 50); do [ -e /tmp/.X11-unix/X99 ] && break; sleep 0.1; done\nexec "$@"\n' \
      > /usr/local/bin/with-xvfb && chmod +x /usr/local/bin/with-xvfb
ENTRYPOINT ["/usr/local/bin/with-xvfb"]
