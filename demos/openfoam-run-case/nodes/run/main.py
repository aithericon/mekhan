"""Generic OpenFOAM solver node — run ONE case, return RAW results.

A reusable, solver-agnostic building block with NO physics interpretation:
write the inbound case (a `{relpath: content}` dict) to the run directory, run
blockMesh (optional) + the named solver + foamToVTK (optional) in the
opencfd/openfoam-default Docker image, and return the raw solver log, the
exported field series (as an artifact key a downstream node can retrieve), and
the contents of every `postProcessing/**/*.dat` file. Whatever consumes this —
a firing-curve evaluator, a DOE sweep, a one-shot check — does its OWN
extraction from these raw outputs.

`dry_run=true` short-circuits the solve (`success=false`, `not_run=true`) so a
caller can conditionally skip it without a Decision branch. A missing Docker
image or a non-zero solver exit returns `success=false` rather than raising — a
generic node REPORTS failure; the caller decides whether a failed solve is
fatal (e.g. the firing evaluator falls back to a surrogate, or re-raises in a
docker-only mode).

Outputs (swept from globals matching the output port): success, returncode,
not_run, solver_log, field_key, postprocessing.
"""

import glob
import os
import shutil
import subprocess
import tarfile

DOCKER_IMAGE = "opencfd/openfoam-default:2506"

# --- Inputs (Start fields) ------------------------------------------------------
case_files = input.case_files or {}
solver = str(input.solver or "").strip()
run_blockmesh = bool(input.run_blockmesh)
export_vtk = bool(input.export_vtk)
dry_run = bool(input.dry_run)

_run_dir = os.environ.get("AITHERICON_RUN_DIR", os.getcwd())
case_dir = os.path.join(_run_dir, "case")

# --- Outputs (defaults; populated below) ----------------------------------------
success = False
returncode = -1
not_run = False
solver_log = ""
field_key = ""
postprocessing = {}


def _write_case():
    """Materialize the {relpath: content} bundle under case_dir."""
    if os.path.isdir(case_dir):
        shutil.rmtree(case_dir, ignore_errors=True)
    for rel, content in case_files.items():
        path = os.path.join(case_dir, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(content)


if dry_run:
    not_run = True
    log_info("run-case: dry_run — skipping solve (success=false, not_run=true)")
elif not solver:
    log_warn("run-case: no solver named — nothing to run")
elif not case_files:
    log_warn("run-case: empty case_files — nothing to run")
else:
    try:
        _write_case()
        # `bash -lc` is REQUIRED: the opencfd image sources the OpenFOAM
        # environment from the login profile. That profile also `cd`s to $HOME,
        # clobbering `docker -w` — so the explicit `cd /case` is load-bearing.
        steps = []
        if run_blockmesh:
            steps.append("blockMesh > log.blockMesh 2>&1")
        steps.append(f"{solver} > log.{solver} 2>&1")
        if export_vtk:
            # Non-fatal: a foamToVTK hiccup must not fail an otherwise-good solve.
            steps.append("{ foamToVTK > log.foamToVTK 2>&1 || true; }")
        script = "cd /case && " + " && ".join(steps)
        cmd = [
            "docker", "run", "--rm",
            "-v", f"{case_dir}:/case",
            DOCKER_IMAGE,
            "bash", "-lc", script,
        ]
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=900)
        returncode = proc.returncode
        success = proc.returncode == 0

        # Raw solver log (the named solver's; the blockMesh/foamToVTK logs stay
        # on disk in the case dir for debugging).
        log_path = os.path.join(case_dir, f"log.{solver}")
        if os.path.exists(log_path):
            with open(log_path) as f:
                solver_log = f.read()
        if not success:
            log_warn(f"run-case: {solver} rc={proc.returncode}: {proc.stderr[-400:]}")

        # Field series → portable artifact (best-effort; only if exported).
        if export_vtk:
            vtk_dir = os.path.join(case_dir, "VTK")
            if os.path.isdir(vtk_dir):
                try:
                    tar_path = os.path.join(_run_dir, "fields.tar.gz")
                    with tarfile.open(tar_path, "w:gz") as tf:
                        tf.add(vtk_dir, arcname="VTK")
                    log_artifact(
                        tar_path,
                        name="fields.tar.gz",
                        category="other",
                        mime_type="application/gzip",
                        blocking=True,
                        metadata={"kind": "openfoam_field_series", "solver": solver},
                    )
                    _exec_id = os.environ.get("AITHERICON_EXECUTION_ID", "")
                    if _exec_id:
                        field_key = f"artifacts/{_exec_id}/fields.tar.gz/fields.tar.gz"
                except Exception as exc:  # noqa: BLE001 — field transport is best-effort
                    log_warn(f"run-case: field persist failed (non-fatal): {exc!r}")

        # ALL postProcessing .dat contents, keyed by case-relative path. Generic:
        # the node has no knowledge of any function-object name — a consumer
        # picks the file(s) it cares about (e.g. minMaxT/.../fieldMinMax.dat).
        for dat in sorted(
            glob.glob(os.path.join(case_dir, "postProcessing", "**", "*.dat"), recursive=True)
        ):
            try:
                with open(dat) as f:
                    postprocessing[os.path.relpath(dat, case_dir)] = f.read()
            except Exception:  # noqa: BLE001
                pass

        log_info(
            f"run-case: {solver} rc={returncode} success={success} "
            f"vtk={'yes' if field_key else 'no'} dats={len(postprocessing)}"
        )
    except Exception as exc:  # noqa: BLE001 — generic node reports failure, never raises
        log_warn(f"run-case: docker path failed (non-fatal): {exc!r}")
        success = False
