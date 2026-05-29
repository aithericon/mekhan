"""Slim single-pass BO re-fit, fired by a catalog Trigger.

This is the body of the 12a catalog-trigger demo. Unlike the 12-bo-loop demo,
there is NO Loop and NO Map here — a single pass that reads the accumulated
observations off the Start token, fits a placeholder surrogate, and emits the
next candidate point. The closed-loop campaign (GP + acquisition + scatter/
gather) is the 12-bo-loop demo; here the point is the trigger wiring, not a
second BO loop.

IMPORTANT (compiler contract): this Python is SCANNED at compile time for slug
references, not executed. The literal reads of `start.observations` and
`start.last_z` below MUST remain verbatim so the compiler synthesizes the
read-arcs into the Start node's parked place. Removing or renaming either
breaks read-arc synthesis.
"""

# --- Read the borrowed seed state (literal slug reads — DO NOT remove) ------
observations = start.observations
last_z = start.last_z

# Catalogue `user_metadata` is a proto map<string,string>, so the trigger's
# payloadMapping lands these as STRINGS. Parse them back to runtime types.
# (Authoring the Start fields as kind:json keeps the strict Start-contract gate
# lenient — see token_shape/port.rs Json escape hatch — while still letting the
# producer ship complex values through string-only metadata.)
import json
if isinstance(observations, str):
    observations = json.loads(observations) if observations else []
if isinstance(last_z, str):
    last_z = float(last_z) if last_z else None

n_seen = len(observations) if observations else 0
log_info(f"refit: observations={n_seen} last_z={last_z}")

# Placeholder proposer: step toward the incumbent (best observed point) with a
# small deterministic nudge. The real GP/acquisition proposer lives in the
# 12-bo-loop demo; this single pass just demonstrates the trigger-fed re-fit.
if observations:
    best = min(observations, key=lambda o: float(o["z"]))
    ba, bd = float(best["a"]), float(best["d"])
else:
    ba, bd = 0.5, 0.5

next_a = round(min(max(ba + 0.05, 0.0), 1.0), 6)
next_d = round(min(max(bd - 0.05, 0.0), 1.0), 6)

log_info(f"refit: next candidate a={next_a} d={next_d}")
