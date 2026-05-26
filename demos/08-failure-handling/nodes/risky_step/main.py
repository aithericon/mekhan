# Risky Step — succeeds, raises, or exits depending on input.fail_mode.
#
# Two ways to "fail" an AutomatedStep from user code:
#   - raise an unhandled exception (Python tears down with a traceback +
#     non-zero exit code), or
#   - call sys.exit(<nonzero>) directly.
#
# Either way the executor reports a job-level failure. With
# retryPolicy.maxRetries == 0 (see graph.json) the token routes straight
# out the AutomatedStep's `error` handle — no retry attempts. Bump
# maxRetries to watch retries fire before exhaustion.
#
# `summary` is an implicit output: the runner sweeps any global matching a
# declared output field at the end of a successful run. On a raise/exit we
# never reach the assignment, so no output is produced and downstream
# success-path consumers never see this step.

import sys

mode = input.fail_mode

if mode == "raise":
    raise RuntimeError(f"Synthetic failure: rejecting payload {input.payload!r}")

if mode == "exit":
    log_warn("exiting non-zero on purpose", payload=input.payload)
    sys.exit(7)

summary = f"Processed payload of length {len(input.payload or '')}"
log_info("processed", chars=len(input.payload or ""))
