# Resolve bbox — per extracted field, union the OCR word boxes whose
# word_index falls in the field's word_range into a visual_ref
# {page (1-based), bbox{x,y,w,h ∈ 0..1}}. Falls back to a fuzzy value
# match against the OCR words when word_range is absent.
#
# Ports the clinic's deleted `visual_references::bbox_from_word_range`
# cascade (recovered from git a23a12b^ compute_functions.rs::BboxResolution
# + the di_extraction_v1.json t_resolve_bbox Rhai effect): union_range +
# find_value + the word_range → source_span → fuzzy fallback ladder.
#
# Upstream borrows (read-arcs synthesized from the bare slug.field accesses
# below — Python's ref_scanner stages each producer envelope):
#   ocr.words                 # Surya per-word geometry (read-arc; not consumed)
#   extract_bloodwork.fields  # OCR-text extractor output
#
# Outputs:
#   fields   # the same fields, each with a resolved visual_ref where possible

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


def _norm_text(v):
    if isinstance(v, str):
        return " ".join(v.lower().split())
    return ""


def _union_range(words, start, end):
    # Union every word box whose word_index ∈ [start, end] (inclusive).
    # Returns {page, bbox{x,y,w,h}} or None when no word matched.
    min_x, min_y = 2.0, 2.0
    max_x, max_y = -1.0, -1.0
    page = None
    for w in words:
        if not isinstance(w, dict):
            continue
        wi = _to_idx(w.get("word_index"))
        if wi < start or wi > end:
            continue
        bbox = w.get("bbox")
        if not isinstance(bbox, dict):
            continue
        bx = _to_num(bbox.get("x"))
        by = _to_num(bbox.get("y"))
        bw = _to_num(bbox.get("w"))
        bh = _to_num(bbox.get("h"))
        if page is None:
            page = _to_idx(w.get("page"))
        if bx < min_x:
            min_x = bx
        if by < min_y:
            min_y = by
        if bx + bw > max_x:
            max_x = bx + bw
        if by + bh > max_y:
            max_y = by + bh
    if page is None or max_x < 0:
        return None
    if page < 1:
        page = 1
    return {
        "page": page,
        "bbox": {"x": min_x, "y": min_y, "w": max_x - min_x, "h": max_y - min_y},
    }


def _find_value(words, value):
    # Fuzzy fallback: find the (multi-word) span whose concatenated normalized
    # text equals the field value, then union that span's boxes. Mirrors the
    # cascade's find_value: exact single-word match, else greedy up-to-5-word
    # window join.
    target = _norm_text(value)
    if not target:
        return None
    n = len(words)
    for i in range(n):
        wi_word = words[i]
        if not isinstance(wi_word, dict):
            continue
        wt = _norm_text(wi_word.get("text"))
        if wt == target:
            wi = _to_idx(wi_word.get("word_index"))
            return _union_range(words, wi, wi)
        combined = wt
        end_j = min(i + 5, n)
        for j in range(i + 1, end_j):
            jw = words[j]
            if not isinstance(jw, dict):
                break
            combined = combined + " " + _norm_text(jw.get("text"))
            if combined == target:
                si = _to_idx(wi_word.get("word_index"))
                ei = _to_idx(jw.get("word_index"))
                return _union_range(words, si, ei)
    return None


update_phase("Index OCR words", "running")

ocr_words = ocr.words if isinstance(ocr.words, list) else []
in_fields = extract_bloodwork.fields if isinstance(extract_bloodwork.fields, list) else []

log_info("resolving visual refs", ocr_words=len(ocr_words), fields=len(in_fields))
update_phase("Index OCR words", "completed")

update_phase("Resolve visual refs", "running")
out_fields = []
resolved_count = 0
for f in in_fields:
    if not isinstance(f, dict):
        continue
    field = dict(f)
    vref = None

    # 1. word_range [start, end] from the extractor.
    wr = field.get("word_range")
    if isinstance(wr, list) and len(wr) >= 2:
        wr_start = _to_idx(wr[0])
        wr_end = _to_idx(wr[1])
        if wr_start >= 0 and wr_end >= wr_start:
            vref = _union_range(ocr_words, wr_start, wr_end)

    # 2. source_span {start, end} fallback (some extractors emit this name).
    if vref is None and isinstance(field.get("source_span"), dict):
        ss = field["source_span"]
        ss_start = _to_idx(ss.get("start"))
        ss_end = _to_idx(ss.get("end"))
        if ss_start >= 0 and ss_end >= ss_start:
            vref = _union_range(ocr_words, ss_start, ss_end)

    # 3. fuzzy value match against the OCR words.
    if vref is None and isinstance(field.get("value"), str):
        vref = _find_value(ocr_words, field["value"])

    if vref is not None:
        field["visual_ref"] = vref
        resolved_count += 1
    out_fields.append(field)

log_metric("fields_total", float(len(out_fields)))
log_metric("fields_with_visual_ref", float(resolved_count))
set_output("fields", out_fields)
update_progress(1.0, f"{resolved_count}/{len(out_fields)} fields grounded")
update_phase("Resolve visual refs", "completed")
