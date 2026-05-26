# Shape-coerce the raw vision extraction into a defaults-filled
# ExtractionResult. Mirrors the canary's Rhai `t_validate` pass-through
# transition (online-clinic `services/document_intelligence/processor.rs`).
#
# Upstream borrows:
#   extract.document_type, extract.document_date, extract.fields,
#   extract.suggested_actions, extract.confidence_score, extract.page_count
#
# Outputs:
#   document_type, document_date, fields, suggested_actions, confidence_score,
#   page_count, ai_model, processing_time_ms

define_phases(["Coerce types", "Stamp run metadata"])

ALLOWED_TYPES = {
    "lab_result", "referral_letter", "prescription", "insurance_form",
    "imaging_report", "discharge_summary", "consultation_note", "invoice",
    "consent_form", "other",
}

update_phase("Coerce types", "running")

doc_type = extract.document_type if isinstance(extract.document_type, str) else None
if doc_type not in ALLOWED_TYPES:
    log_warn("document_type fell back to 'other'", got=doc_type)
    doc_type = "other"

doc_date = extract.document_date if isinstance(extract.document_date, str) else None

raw_fields = extract.fields if isinstance(extract.fields, list) else []
coerced_fields = []
for f in raw_fields:
    if not isinstance(f, dict):
        continue
    key = f.get("key")
    value = f.get("value")
    conf = f.get("confidence", 0.0)
    if not isinstance(key, str) or not isinstance(value, str):
        log_debug("dropping malformed field", field=f)
        continue
    try:
        conf = float(conf)
    except (TypeError, ValueError):
        conf = 0.0
    coerced_fields.append({"key": key, "value": value, "confidence": max(0.0, min(1.0, conf))})

raw_actions = extract.suggested_actions if isinstance(extract.suggested_actions, list) else []
suggested_actions = [a for a in raw_actions if isinstance(a, str)]

try:
    confidence_score = float(extract.confidence_score)
except (TypeError, ValueError):
    confidence_score = 0.0
confidence_score = max(0.0, min(1.0, confidence_score))

try:
    page_count = int(extract.page_count)
except (TypeError, ValueError):
    page_count = 1
if page_count < 1:
    page_count = 1

log_metric("fields_count", float(len(coerced_fields)))
log_metric("actions_count", float(len(suggested_actions)))
update_phase("Coerce types", "completed")

update_phase("Stamp run metadata", "running")
# Mirrors the canary's hard-coded `ai_model = "qwen3.6:35b-a3b"`. We use the
# llama3.2-vision substitute locally — keep this in sync with the LLM step's
# `model` config field if you change it there.
ai_model = "llama3.2-vision:11b"
processing_time_ms = 0

set_output("document_type", doc_type)
set_output("document_date", doc_date or "")
set_output("fields", coerced_fields)
set_output("suggested_actions", suggested_actions)
set_output("confidence_score", confidence_score)
set_output("page_count", page_count)
set_output("ai_model", ai_model)
set_output("processing_time_ms", processing_time_ms)

update_progress(1.0, f"{len(coerced_fields)} fields, type={doc_type}")
update_phase("Stamp run metadata", "completed")
