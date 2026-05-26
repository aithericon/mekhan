# Verify citations — mekhan analogue of online-clinic's `provenance` step kind.
#
# Each extracted field carries a citations[] array; each citation has a
# supporting_text that *should* appear verbatim (modulo whitespace + case) in
# the OCR text. We do a normalized substring match per citation and aggregate
# verified vs. unverified counts. The downstream `gate` Decision uses
# `verify.any_unverified` to route low-confidence runs to a medical-assistant
# review.
#
# Not in scope here:
#   - Citation::EmbeddingChunk (no RAG primitive yet)
#   - Citation::ImageBbox (kreuzberg doesn't emit per-word bboxes today;
#     would need executor-surya from sub-phase 2.2b)
#   - Cross-stage attribution traces
#
# Upstream borrows:
#   ocr.content   # kreuzberg's native ExtractionResult key
#   persist.fields
#
# Outputs:
#   verified_count, unverified_count, any_unverified, unverified_keys

import re

define_phases(["Normalize OCR", "Match citations", "Aggregate"])

def normalize(s: str) -> str:
    # Lowercase + collapse whitespace. Mirrors hybrid_auto matcher's
    # cheapest path before falling back to fuzzy / embedding match.
    return re.sub(r"\s+", " ", (s or "").lower()).strip()

update_phase("Normalize OCR", "running")
hay = normalize(ocr.content)
log_info("ocr text normalized", chars=len(hay))
update_phase("Normalize OCR", "completed")

update_phase("Match citations", "running")
verified_count = 0
unverified_count = 0
unverified_keys: list[str] = []
total_citations = 0

for field in persist.fields or []:
    key = field.get("key", "<unknown>")
    citations = field.get("citations") or []
    if not citations:
        # No citations at all — treat as unverified.
        unverified_count += 1
        unverified_keys.append(key)
        log_warn("field has no citations", key=key)
        continue

    field_ok = False
    for c in citations:
        total_citations += 1
        needle = normalize(c.get("supporting_text", ""))
        if needle and needle in hay:
            field_ok = True
            break
        else:
            log_debug(
                "citation did not match OCR",
                key=key,
                kind=c.get("kind"),
                supporting_text_excerpt=c.get("supporting_text", "")[:80],
            )

    if field_ok:
        verified_count += 1
    else:
        unverified_count += 1
        unverified_keys.append(key)

log_metric("citations_total", float(total_citations))
log_metric("fields_verified", float(verified_count))
log_metric("fields_unverified", float(unverified_count))
update_phase("Match citations", "completed")

update_phase("Aggregate", "running")
any_unverified = unverified_count > 0
if any_unverified:
    log_warn(
        "unverified fields — routing to MA review",
        unverified_count=unverified_count,
        keys=unverified_keys,
    )
else:
    log_info("all citations verified", verified_count=verified_count)
update_progress(1.0, f"{verified_count} verified / {unverified_count} unverified")
update_phase("Aggregate", "completed")
