# Summarize a bound curated asset (docs/20 §5).
#
# The `materials` asset binding stages the asset's whole record collection as
# `materials.json`; the runner exposes every `<alias>.json` staged input as a
# top-level global, so `materials` here is the list of record dicts — pure
# business data that rode `job_inputs` staging, never the control token.
#
# `material_count` / `densest_material` are implicit outputs: the runner sweeps
# globals matching this step's declared output port at the end of execution.

mats = materials  # injected global: list[dict] of material rows

material_count = len(mats)
densest = max(mats, key=lambda m: m["density"])
densest_material = densest["name"]

log_info("summarized materials asset", count=material_count, densest=densest_material)
