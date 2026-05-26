# Persist — stamp extracted fields with patient/class/date metadata, emit a
# unified field list. The five extractor branches all funnel through the
# `merge-extraction` node (slug `extraction`), so there is exactly one
# upstream data borrow: extraction.fields. No OR-merging in user code.
#
# Upstream borrows (read-arcs synthesized by the compiler):
#   start.patient_id
#   classify.document_class, classify.document_date, classify.confidence
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
    })

# Pre-rendered markdown table for the downstream HumanTask review. The
# mdsvex block in `nodes/review/task.json` embeds `{{ persist.summary_table }}`
# verbatim; rendering happens in the reviewer's UI. Beats dumping
# `{{ persist.fields }}` (which goes through Rhai's debug repr —
# `#{...}`, `()` for null — and is unreadable in the reviewer's pane).
def _md_escape(v):
    if v is None:
        return ""
    s = str(v)
    # Pipe + newline are the two table-breaking chars; escape pipe,
    # collapse newlines so a multi-line citation doesn't shatter the row.
    return s.replace("|", "\\|").replace("\n", " ")

def _first_citation_text(f):
    cits = f.get("citations") or []
    if not cits:
        return ""
    return cits[0].get("supporting_text") or ""

if fields:
    header = "| Field | Value | Unit | Reference range | Confidence | OCR span |\n"
    sep    = "| --- | --- | --- | --- | --- | --- |\n"
    rows   = "".join(
        f"| {_md_escape(f.get('key'))} "
        f"| {_md_escape(f.get('value'))} "
        f"| {_md_escape(f.get('unit'))} "
        f"| {_md_escape(f.get('reference_range'))} "
        f"| {_md_escape(f.get('confidence'))} "
        f"| {_md_escape(_first_citation_text(f))} |\n"
        for f in fields
    )
    summary_table = header + sep + rows
else:
    summary_table = "_No fields extracted._"

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
