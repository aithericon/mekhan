# `document-pipeline-branching-v1` — Branching Intake DAG

Phase 1 of the branching document pipeline. A vision-LLM classifies the raw
uploaded file, a `decision` routes by class into one of two branches, and the
branch tails fan into an XOR-`join` before a final shape-coerce. The bloodwork
branch is the bbox-grounded path (Surya OCR → OCR-text extract → per-field
visual-ref resolution); every other class takes the generic single-vision
fallback.

This is **generic scenario data** — graph.json (AIR), prompts, JSON schemas,
and Python scripts. The document classes (`lab_result`, `befund`, …) live as
JSON-schema enum values, never as mekhan engine/pool/backend vocabulary.

## Shape

```
trigger ─► start ─► classify (vision LLM, raw file) ─► route-by-class (decision)
                                                          │
        branch-bloodwork (lab_result / laborergebnis) ────┤
          ocr (Surya) ─► extract-bloodwork (LLM) ─► resolve-bbox (python) ──┐
                                                                            ├─► merge-extraction (join, mode:"any") ─► validate (python) ─► end-ok
        default (everything else, incl. befund) ──────────────────────────┘
          extract-generic (vision LLM) ─────────────────────────────────────┘
```

- **`classify`** runs on the **raw file** *before* OCR (Phase-1 Decision 3).
  No OCR text is available, so the vision model judges from layout alone.
  ⚠️ **needs-live-experimentation** — classify→ocr ordering is unvalidated.
- **`route-by-class`** routes only explicit lab classes
  (`lab_result` / `laborergebnis`) into the bloodwork branch. **`befund` is
  NOT pinned to bloodwork** in Phase 1; it (and every other class) falls
  through `defaultBranch: "default"` into the generic branch.
- **`merge-extraction`** is a `join` with `mode: "any"` (XOR-join, dual of the
  decision split) and slug `extraction`, so `validate` does a single clean
  borrow `extraction.fields`.

## Models / backends per step

| Step | Backend | Model | Notes |
|---|---|---|---|
| `classify` | llm (vision) | `llama3.2-vision:11b` | Image = `{{ start.document_file }}`. No OCR text. JSON-schema `{document_type, confidence}`. |
| `ocr` | surya | — | Surya IS the OCR engine (no model). Emits `full_text`, `words` (per-word boxes 0..1 + `word_index` + `page`), `pages`, `page_count`. |
| `extract-bloodwork` | llm | `qwen3.5:9b` | OCR-text-driven (`{{ ocr.full_text }}`). Per field returns `word_range:[start,end]`. Schema via top-level `definitions.ExtractionFields`. |
| `resolve-bbox` | python | — | Unions `ocr.words` boxes by each field's `word_range` → `visual_ref{page, bbox 0..1}`; fuzzy value fallback. Reads `ocr.words` (read-arc) + `extract_bloodwork.fields`. |
| `extract-generic` | llm (vision) | `llama3.2-vision:11b` | Generic fallback. Raw-file vision, no bbox grounding. |
| `merge-extraction` | join (`mode: "any"`) | — | XOR-join; re-parks the firing branch's payload under slug `extraction`. |
| `validate` | python | — | Shape-coerces `extraction.fields` (preserving `visual_ref`), stamps `document_type` from the classifier. |

## Data passing

- LLM image input: `images: [{ path: "{{ start.document_file }}" }]`
- Surya target: `file: "{{ start.document_file }}"`
- LLM prompt borrow: `{{ ocr.full_text }}`
- Decision guard: `classify.document_type == "lab_result" || classify.document_type == "laborergebnis"`
- Python borrows are bare `slug.field` accesses in `main.py` — the Python
  `ref_scanner` stages each producer envelope as a read-arc. `resolve-bbox`
  reads `ocr.words` + `extract_bloodwork.fields`; `validate` reads
  `extraction.fields` + `classify.document_type`.
- End mapping: `{ targetField: "document_type", expression: "classify.document_type" }`,
  `{ targetField: "fields", expression: "validate.fields" }`

## `resolve-bbox` cascade

Ports the clinic's deleted `visual_references::bbox_from_word_range` cascade
(recovered from git `a23a12b^ compute_functions.rs::BboxResolution` + the
`di_extraction_v1.json` `t_resolve_bbox` Rhai effect). Per field, the ladder is:

1. `word_range: [start, end]` → `union_range` over OCR words by `word_index`.
2. `source_span: {start, end}` fallback (alternate extractor field name).
3. Fuzzy `find_value` — locate the word span whose concatenated normalized
   text equals the field value (exact single-word, else greedy ≤5-word join),
   then union its boxes.

`union_range` returns `{page (1-based), bbox{x, y, w, h ∈ 0..1}}` or nothing
when no word matched.

## Phase-1 scope

- **One classifier branch decision**, two branches only: bloodwork (lab) +
  generic fallback. No prescription / clinical-note / form branches (those are
  the multi-branch shape `document-pipeline-v1` already demonstrates).
- **Single document, multi-page** — Surya `page_count` / per-word `page`
  carry pagination; `visual_ref.page` is 1-based.
- **classify→ocr ordering is unvalidated** — flagged needs-live-experimentation.

## Loading

- Seeded under templateId `00000000-0000-0000-0000-000000000054`.
- Trigger node id: `trg_document_pipeline_branching_v1`.
- `MEKHAN__DEMOS__SEED=true` (default in dev) publishes it at startup.

## This `graph.json` is the single source of truth for the clinic's DI net

The online-clinic ships the **compiled AIR** of this `graph.json` as
`server/data/petri-nets/document_pipeline_v1.json`. That file is a generated
artifact — it is **never hand-edited**. When this `graph.json` (or either
`nodes/*/main.py`, or the prompts / schemas) changes, regenerate the clinic AIR:

1. **Compile + export the AIR** (mekhan, this repo):

   ```sh
   DUMP_BRANCHING_AIR=/tmp/document_pipeline_v1.json \
     cargo test --jobs 8 -p mekhan-service --lib dump_document_pipeline_branching_v1_air -- --nocapture
   ```

   The `dump_document_pipeline_branching_v1_air` test (in
   `service/src/demos.rs`) compiles this demo through the same
   `compile_to_air_with_subworkflows_interfaces_and_configs` path the publish
   pipeline uses, then **inlines** every parked node config back into its
   `<node>/prepare` transition (`config_ref { storage_path }` → inline
   `config { … }`). The clinic ships the AIR through `apply-air`, which stores
   it verbatim and uploads nothing to S3, so the AIR must be self-contained:
   inline configs + the already-inlined Python `main.py` source (carried in
   `inputs[].source.content` by the lowering). The test asserts no
   `node-config.json` storage path survives in any transition logic.

2. **Copy the exported AIR into the clinic**:

   ```sh
   cp /tmp/document_pipeline_v1.json \
     ~/dev/online-clinic/server/data/petri-nets/document_pipeline_v1.json
   ```

3. **Re-apply** (clinic): `just apply-workflows` (needs a live mekhan-service)
   re-pushes every scenario and rewrites `_registry.json`.

### Naming reconciliation (clinic side)

- The clinic file is `document_pipeline_v1.json`; the clinic trigger convention
  is `node_id = trg_<basename>` → `trg_document_pipeline_v1`, and the trigger's
  `air_target_place_id` must exist in the AIR `places`.
- The compiled AIR has **no** conventional `p_input` place; its workflow entry
  place is **`p_start_ready`** (the never-produced place that feeds the lowered
  `start` node chain).
- So the clinic carries a trigger override
  `server/data/petri-nets/document_pipeline_v1.trigger.json` setting
  `node_id: trg_document_pipeline_v1` + `air_target_place_id: p_start_ready`.
  This keeps the registry ratchet (`trigger_node_id == trg_document_pipeline_v1`)
  and `processor.rs` scenario name (`document_pipeline_v1`) consistent while the
  AIR keeps its own compiler-minted trigger id (`trg_document_pipeline_branching_v1`)
  for the mekhan-seeded demo.
