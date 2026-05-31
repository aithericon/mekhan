# Resolve bbox — ground each extracted field to a visual_ref
# {page (1-based), bbox{x,y,w,h ∈ 0..1}} using the OCR engine's word
# geometry DIRECTLY. The bounding boxes come from Surya (the only component
# that actually knows where text sits on the page), NEVER from the LLM: the
# extractor only reads the OCR *text*, so any word indices / spans it emits
# are guesses and are deliberately ignored here.
#
# Grounding strategy (Surya geometry only):
#   1. Match the field's test-name KEY against the OCR words as a 1..4-word
#      span (normalized). Lab test names are unique top-to-bottom, so the
#      first span match identifies the field's row.
#   2. Union every OCR word box on that row (same page, same top-y band) into
#      one bbox — i.e. highlight the whole source line (test name → value →
#      unit → reference range), which is what a reviewer wants to see.
#   3. Fallback when the key can't be located: a content match of the field
#      VALUE against the OCR words, then union that value's row. Still pure
#      Surya geometry — no LLM coordinates.
# A field that matches nothing is left WITHOUT a visual_ref (no box beats a
# wrong box).
#
# Upstream borrows (read-arcs synthesized from the bare slug.field accesses
# below — Python's ref_scanner stages each producer envelope):
#   ocr.words                 # Surya per-word geometry (read-arc; not consumed)
#   extract_bloodwork.fields  # OCR-text extractor output (text only; no bbox)
#
# Outputs:
#   fields   # the same fields, each with a Surya-grounded visual_ref where possible

define_phases(["Index OCR words", "Resolve visual refs"])


def _to_num(v):
    if isinstance(v, bool):
        return 0.0
    if isinstance(v, (int, float)):
        return float(v)
    return 0.0


def _to_idx(v):
    if isinstance(v, bool):
        return -1
    if isinstance(v, int):
        return v
    if isinstance(v, float):
        return int(v)
    return -1


def _norm(v):
    # Lowercase + collapse whitespace. Used for value matching.
    if isinstance(v, str):
        return " ".join(v.lower().split())
    return ""


def _norm_key(v):
    # Key normalization is looser than value: drop separators/glyphs the OCR
    # and the extractor disagree on (':' '%' and surrounding space) so e.g.
    # extractor "Segmentkernige%" matches OCR "Segmentkernige" + "%".
    s = _norm(v)
    for ch in (":", "%"):
        s = s.replace(ch, " ")
    return " ".join(s.split())


# OCR words within this fraction of the key word's TOP-y count as the same
# row. Surya emits row-aligned tops (same-row words share y to ~0.001); rows
# in this dense lab table sit ~0.013 apart, so 0.005 isolates a single row.
_ROW_Y_TOL = 0.005


def _row_union(words, page, row_y):
    # Union every OCR word box on (page, row_y±tol) into one bbox.
    min_x, min_y = 2.0, 2.0
    max_x, max_y = -1.0, -1.0
    for w in words:
        if not isinstance(w, dict):
            continue
        if _to_idx(w.get("page")) != page:
            continue
        bbox = w.get("bbox")
        if not isinstance(bbox, dict):
            continue
        wy = _to_num(bbox.get("y"))
        if abs(wy - row_y) > _ROW_Y_TOL:
            continue
        bx = _to_num(bbox.get("x"))
        bw = _to_num(bbox.get("w"))
        bh = _to_num(bbox.get("h"))
        if bx < min_x:
            min_x = bx
        if wy < min_y:
            min_y = wy
        if bx + bw > max_x:
            max_x = bx + bw
        if wy + bh > max_y:
            max_y = wy + bh
    if max_x < 0:
        return None
    return {
        "page": page if page >= 1 else 1,
        "bbox": {"x": min_x, "y": min_y, "w": max_x - min_x, "h": max_y - min_y},
    }


def _word_row(w):
    # (page, top-y) of a word, or None.
    if not isinstance(w, dict):
        return None
    bbox = w.get("bbox")
    if not isinstance(bbox, dict):
        return None
    page = _to_idx(w.get("page"))
    return (page if page >= 1 else 1, _to_num(bbox.get("y")))


def _find_key_row(words, key):
    # Locate the field's row by matching its test-name key as a 1..4-word span
    # of consecutive OCR words. Returns (page, row_y) of the first match.
    target = _norm_key(key)
    if not target:
        return None
    n = len(words)
    for i in range(n):
        w0 = words[i]
        if not isinstance(w0, dict):
            continue
        combined = _norm_key(w0.get("text"))
        if not combined:
            continue
        if combined == target:
            return _word_row(w0)
        for j in range(i + 1, min(i + 4, n)):
            wj = words[j]
            if not isinstance(wj, dict):
                break
            tj = _norm_key(wj.get("text"))
            if not tj:
                break
            combined = (combined + " " + tj).strip()
            if combined == target:
                return _word_row(w0)
            if len(combined) > len(target):
                break
    return None


def _find_value_row(words, value):
    # Fallback: locate a row by content-matching the field value against the
    # OCR words (single word, else greedy up-to-5-word join), then union that
    # word's row. Still pure Surya geometry.
    target = _norm(value)
    if not target:
        return None
    n = len(words)
    for i in range(n):
        wi = words[i]
        if not isinstance(wi, dict):
            continue
        wt = _norm(wi.get("text"))
        if not wt:
            continue
        if wt == target:
            return _word_row(wi)
        combined = wt
        for j in range(i + 1, min(i + 5, n)):
            jw = words[j]
            if not isinstance(jw, dict):
                break
            combined = combined + " " + _norm(jw.get("text"))
            if combined == target:
                return _word_row(wi)
            if len(combined) > len(target):
                break
    return None


update_phase("Index OCR words", "running")

ocr_words = ocr.words if isinstance(ocr.words, list) else []
in_fields = extract_bloodwork.fields if isinstance(extract_bloodwork.fields, list) else []

log_info("resolving visual refs (Surya geometry)", ocr_words=len(ocr_words), fields=len(in_fields))
update_phase("Index OCR words", "completed")

update_phase("Resolve visual refs", "running")
out_fields = []
resolved_count = 0
for f in in_fields:
    if not isinstance(f, dict):
        continue
    field = dict(f)
    # Strip any LLM-emitted span — bboxes come from Surya, not the extractor.
    field.pop("word_range", None)

    row = None
    key = field.get("key") or field.get("name") or field.get("label")
    if isinstance(key, str):
        row = _find_key_row(ocr_words, key)
    if row is None and isinstance(field.get("value"), str):
        row = _find_value_row(ocr_words, field["value"])

    if row is not None:
        vref = _row_union(ocr_words, row[0], row[1])
        if vref is not None:
            field["visual_ref"] = vref
            resolved_count += 1
    out_fields.append(field)

log_metric("fields_total", float(len(out_fields)))
log_metric("fields_with_visual_ref", float(resolved_count))
set_output("fields", out_fields)
update_progress(1.0, f"{resolved_count}/{len(out_fields)} fields grounded (Surya)")
update_phase("Resolve visual refs", "completed")
