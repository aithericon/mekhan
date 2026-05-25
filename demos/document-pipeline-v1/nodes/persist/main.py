# Persist — stamp extracted fields with patient/class/date metadata, emit a
# unified field list. The five extractor branches all funnel through the
# `merge-extraction` node (slug `extraction`), so there is exactly one
# upstream data borrow: extraction.fields. No OR-merging in user code.
#
# Upstream borrows (read-arcs synthesized by the compiler):
#   start.patient_id
#   classify.document_class, classify.document_date, classify.confidence
#   ocr.page_count
#   extraction.fields
#
# Outputs:
#   patient_data_kind, field_count, fields

define_phases(["Stamp metadata", "Emit"])

# Map document_class → patient_data_kind. Mirrors online-clinic's
# `persist_extraction_to_patient_data` function dispatch.
kind_by_class = {
    "lab_result":         "lab_result",
    "prescription":       "medication",
    "referral_letter":    "clinical_note",
    "imaging_report":     "clinical_note",
    "discharge_summary":  "clinical_note",
    "consultation_note":  "clinical_note",
    "insurance_form":     "admin",
    "invoice":            "admin",
    "consent_form":       "admin",
}

update_phase("Stamp metadata", "running")
patient_data_kind = kind_by_class.get(classify.document_class, "generic")
incoming = extraction.fields or []
field_count = len(incoming)

fields = []
for f in incoming:
    fields.append({
        **f,
        "_patient_id":     start.patient_id,
        "_document_class": classify.document_class,
        "_document_date":  classify.document_date,
        "_page_count":     ocr.page_count,
    })

log_info(
    "persisting extraction",
    document_class=classify.document_class,
    patient_data_kind=patient_data_kind,
    field_count=field_count,
)
log_metric("field_count", float(field_count))
log_metric("classify_confidence", float(classify.confidence or 0))
update_phase("Stamp metadata", "completed")

update_phase("Emit", "running")
update_progress(1.0, f"Persisted {field_count} {patient_data_kind} fields")
update_phase("Emit", "completed")
