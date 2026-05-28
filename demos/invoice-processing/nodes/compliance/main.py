# Compliance Check — sanctions & fraud screening (Python backend).
#
# Same runner-injected SDK as Extract. Each upstream producer is a Python
# global — 'extract' (the OCR step) and 'review' (the HumanTask) — typed
# in the editor via the generated _aithericon_io.pyi overlay. The compiler
# stages each referenced producer's envelope as <slug>.json with the
# business fields hoisted to the top level (HumanTask form fields out of
# .data, AutomatedStep output fields out of .detail.outputs) so attribute
# reads just work; missing fields read as None.
import time

# 'extract.amount' is guaranteed present here (we're inside the high-value
# branch, downstream of the Extract step), but kept defensively in case
# OCR ever emits a null amount.
amount = extract.amount if extract.amount is not None else (review.invoice_amount or 0)

# Process layout / definition surfaced to the user for this step.
define_phases(["Read context", "Sanctions screening", "Fraud scoring", "Decision"])

update_phase("Read context", "running")
log_info("starting compliance screening", amount=amount)
update_progress(0.1, "Reading upstream context")
time.sleep(0.3)  # demo pacing so the live phase/progress stream is visible
update_phase("Read context", "completed")

update_phase("Sanctions screening", "running")
log_info("checking vendor against sanctions / watch lists")
update_progress(0.4, "Sanctions list lookup")
time.sleep(0.6)
log_info("no sanctions match found")
update_phase("Sanctions screening", "completed")

update_phase("Fraud scoring", "running")
log_info("scoring fraud risk", model="rules-v2", amount=amount)
update_progress(0.75, "Running fraud risk model")
time.sleep(0.5)
risk_score = 0.12
log_metric("risk_score", risk_score)
update_phase("Fraud scoring", "completed")

update_phase("Decision", "running")
compliant = risk_score < 0.5
if not compliant:
    log_warn("invoice flagged as HIGH RISK — routing to manual review", risk_score=risk_score)
else:
    log_info("invoice cleared compliance", risk_score=risk_score)
update_progress(1.0, "Compliance complete")
update_phase("Decision", "completed")

# ── Outputs (implicit set_output via name match against the output port) ──
# `compliant` and `risk_score` are already locals above; just expose
# `checked_at` and the runner sweeps all three at exec end.
checked_at = "2024-01-01"
