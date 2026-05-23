# Extract Data — OCR + NLP extraction (Aithericon Python backend).
#
# Upstream borrows are plain Python globals — one per producer slug. The
# compiler scans every <slug>.<field> access in this file, synthesizes the
# read-arc, and stages the producer's parked envelope as <slug>.json. The
# runner promotes each staged file to a global, so 'review' below is the
# upstream HumanTask's full form token — no aithericon import, no
# token["field"], no SDK call to fetch it.
#
# Outputs are written *implicitly*: assign a name declared in this step's
# output port and the runner sweeps it from `globals()` at exec end. The
# editor / .pyi overlay types these names so a typo on the LHS is flagged
# at author time. `set_output(name, value)` still works for dynamic names.
#
# Runner-injected SDK helpers (no import needed):
#   log_info / log_warn / log_error / log_debug(msg, **fields)
#   log_metric(name, value), log_artifact(path, name=...)
#   define_phases([...]), update_phase(name, status)
#   update_progress(fraction, message=...)
# Live phases/progress/logs/metrics stream through the executor →
# causality → hpi_logs/hpi_metrics pipeline and surface in the process view.
import time

# Process layout / definition surfaced to the user for this step.
define_phases(["Load document", "OCR scan", "NLP extraction", "Validate", "Emit"])

update_phase("Load document", "running")
log_info("loading invoice", vendor=review.vendor_name, amount=review.invoice_amount)
update_progress(0.05, "Reading upstream invoice fields")
time.sleep(0.4)  # demo pacing so the live phase/progress stream is visible
update_phase("Load document", "completed")

update_phase("OCR scan", "running")
log_info("running OCR over the uploaded invoice image")
update_progress(0.3, "OCR scan in progress")
time.sleep(0.6)
log_info("OCR finished", pages=1, confidence=0.97)
log_metric("ocr_confidence", 0.97)
update_phase("OCR scan", "completed")

update_phase("NLP extraction", "running")
log_info("extracting structured fields: vendor, amount, line items")
update_progress(0.6, "NLP field extraction")
time.sleep(0.6)
update_phase("NLP extraction", "completed")

update_phase("Validate", "running")
if (review.invoice_amount or 0) <= 0:
    log_warn("extracted amount is non-positive — downstream review advised",
             amount=review.invoice_amount)
else:
    log_info("amount sanity check passed", amount=review.invoice_amount)
update_progress(0.85, "Validating extracted fields")
time.sleep(0.3)
update_phase("Validate", "completed")

update_phase("Emit", "running")

# ── Outputs (implicit set_output via name match against the output port) ──
vendor = review.vendor_name or ""
amount = review.invoice_amount or 0
extracted = True

log_metric("invoice_amount", float(amount))
log_info("extraction complete", vendor=vendor, amount=amount)
update_progress(1.0, "Extraction done")
update_phase("Emit", "completed")
