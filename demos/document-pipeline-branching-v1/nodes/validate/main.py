# Validate — shape-coerce the merged fields into a defaults-filled list and
# stamp the document_type from the classifier. Mirrors the canary's Rhai
# `t_validate` pass-through, but preserves any per-field `visual_ref` the
# bloodwork branch's resolve-bbox step produced.
#
# The two branches converge at the `extraction` join (mode=any), so there is
# exactly one upstream data borrow for the field list: extraction.fields.
# document_type comes straight from the classifier.
#
# Upstream borrows (read-arcs synthesized from the bare slug.field accesses):
#   extraction.fields        # whichever branch fired (bloodwork or generic)
#   classify.document_type   # the vision classification
#
# Outputs:
#   fields, document_type

define_phases(["Coerce fields", "Stamp document_type"])

update_phase("Coerce fields", "running")

raw_fields = extraction.fields if isinstance(extraction.fields, list) else []
coerced_fields = []
for f in raw_fields:
    if not isinstance(f, dict):
        log_debug("dropping non-dict field", field=f)
        continue
    key = f.get("key")
    value = f.get("value")
    if not isinstance(key, str) or not isinstance(value, str):
        log_debug("dropping malformed field", field=f)
        continue

    conf = f.get("confidence", 0.0)
    try:
        conf = float(conf)
    except (TypeError, ValueError):
        conf = 0.0
    conf = max(0.0, min(1.0, conf))

    unit = f.get("unit") if isinstance(f.get("unit"), str) else None
    ref_range = f.get("reference_range") if isinstance(f.get("reference_range"), str) else None

    coerced = {
        "key": key,
        "value": value,
        "unit": unit,
        "reference_range": ref_range,
        "confidence": conf,
    }
    # Preserve the bloodwork branch's per-field grounding when present.
    if isinstance(f.get("visual_ref"), dict):
        coerced["visual_ref"] = f["visual_ref"]
    coerced_fields.append(coerced)

log_metric("fields_count", float(len(coerced_fields)))
update_phase("Coerce fields", "completed")

update_phase("Stamp document_type", "running")
ALLOWED_TYPES = {
    "lab_result", "laborergebnis", "befund", "referral_letter", "imaging_report",
    "discharge_summary", "consultation_note", "insurance_form", "invoice",
    "consent_form", "other",
}
doc_type = classify.document_type if isinstance(classify.document_type, str) else None
if doc_type not in ALLOWED_TYPES:
    log_warn("document_type fell back to 'other'", got=doc_type)
    doc_type = "other"

set_output("fields", coerced_fields)
set_output("document_type", doc_type)

update_progress(1.0, f"{len(coerced_fields)} fields, type={doc_type}")
update_phase("Stamp document_type", "completed")
