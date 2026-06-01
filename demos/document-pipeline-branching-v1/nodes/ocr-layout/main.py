# OCR layout — reconstruct the document's table ROWS from Surya's per-word
# geometry, so the downstream extractor reads column-aligned lines instead of
# Surya's flattened `full_text` reading order. That reading order interleaves
# cells across columns/rows (e.g. a row's value lands several lines away from
# its test name, and a missing value column makes the reference range read as
# the value) — which is what causes shifted rows and range/value swaps in the
# extraction. Grouping words by their row (top-y band) and ordering each row
# left-to-right by x restores "test-name  value  unit  reference-range" lines.
#
# This is the structured-OCR representation the extractor consumes — the OCR
# step detects the geometry, this step turns it into aligned text, medgemma
# reads aligned text. No clinic-domain vocabulary: it is generic table-row
# reconstruction over OCR word boxes.
#
# Upstream borrow (read-arc synthesized from the bare slug.field access):
#   ocr.words   # Surya per-word geometry [{text, bbox{x,y,w,h}, page, word_index}]
#
# Outputs:
#   layout_text   # column-aligned, row-ordered plain text (pages delimited)

define_phases(["Group rows", "Emit layout"])


def _to_num(v):
    if isinstance(v, bool):
        return 0.0
    if isinstance(v, (int, float)):
        return float(v)
    return 0.0


def _to_idx(v):
    if isinstance(v, bool):
        return 1
    if isinstance(v, int):
        return v
    if isinstance(v, float):
        return int(v)
    return 1


# Words whose top-y is within this band belong to the same visual row. Surya
# emits row-aligned tops (~0.001 apart within a row); rows in this dense lab
# table sit ~0.013 apart, so 0.006 groups a row without bleeding into its
# neighbours.
_ROW_Y_TOL = 0.006

update_phase("Group rows", "running")

words = ocr.words if isinstance(ocr.words, list) else []

# Normalize to (page, y, x, text); skip malformed entries.
items = []
for w in words:
    if not isinstance(w, dict):
        continue
    b = w.get("bbox")
    if not isinstance(b, dict):
        continue
    txt = w.get("text")
    if not isinstance(txt, str) or not txt.strip():
        continue
    items.append(
        (
            _to_idx(w.get("page")),
            _to_num(b.get("y")),
            _to_num(b.get("x")),
            txt.strip(),
        )
    )

# Group into rows by (page, top-y band). Sort first by (page, y, x) so words
# stream in reading-ish order, then bucket by y proximity within a page.
items.sort(key=lambda t: (t[0], t[1], t[2]))
rows = []  # each: [page, anchor_y, [(x, text), ...]]
for page, y, x, txt in items:
    placed = False
    for row in rows:
        if row[0] == page and abs(row[1] - y) <= _ROW_Y_TOL:
            row[2].append((x, txt))
            placed = True
            break
    if not placed:
        rows.append([page, y, [(x, txt)]])

update_phase("Group rows", "completed")

update_phase("Emit layout", "running")

# Emit one line per row, cells ordered left-to-right by x and joined with a
# double space so column boundaries stay visible. Pages are delimited so the
# extractor can attribute rows to pages if needed.
lines = []
current_page = None
for page, _y, cells in rows:
    if page != current_page:
        lines.append(f"--- page {page} ---")
        current_page = page
    cells.sort(key=lambda c: c[0])
    lines.append("  ".join(c[1] for c in cells))

layout_text = "\n".join(lines)

log_info("reconstructed OCR layout", words=len(items), rows=len(rows), chars=len(layout_text))
log_metric("layout_rows", float(len(rows)))
set_output("layout_text", layout_text)
update_progress(1.0, f"{len(rows)} rows reconstructed")
update_phase("Emit layout", "completed")
