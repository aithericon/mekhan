# `document-pipeline-v1` — Intake DAG

Port of the online-clinic `document_pipeline_v1` workflow (see
`/Users/milanender/AithericonResearch/mekhanXonline-clinic/01-document-pipeline-v1.md`)
onto mekhan primitives. Patient-uploaded medical document → kreuzberg OCR
→ vision-LLM classify → class-discriminant extract → persist → citation
verifier → optional MA review.

## Shape

```
trigger ─► start ─► ocr ─► classify ─► route-by-class (decision)
                                           ├─ lab_result                                                  → extract-bloodwork    ─┐
                                           ├─ prescription                                                → extract-prescription ─┤
                                           ├─ referral/imaging/discharge/consultation                     → extract-clinical-note┼─► merge-extraction (join, mode: "any") ─► persist ─► verify ─► gate (decision)
                                           ├─ insurance_form/invoice/consent_form                         → extract-form-fields ─┤                                                                       ├─ unverified|low-conf → review ─► end-reviewed
                                           └─ default (other)                                             → extract-generic     ─┘                                                                       └─ clean                          ─► end-clean
```

`merge-extraction` is a `join` node with **`mode: "any"`** — it fires on the
first incoming control token (XOR-join, dual of decision). The same node type
with **`mode: "all"`** is the AND-join (wait for every branch, merge payloads).
One node type, one knob — the user picks "wait for all" or "fire on any" in
config; the type name doesn't pretend they're different things.

Lowering is structural either way: incoming edges feed a shared input place.
For `mode: "all"` the transition only fires when every input place has a
token. For `mode: "any"` the transition fires per arriving token (each
branch's deposit is independently sufficient).
On the data side, `merge-extraction` re-parks the inbound payload under slug
`extraction`, so downstream `persist` does a single clean borrow
`extraction.fields`.

## Models / backends per step

| Step | Backend | Model | Notes |
|---|---|---|---|
| `ocr` | kreuzberg | — | Tesseract `deu+eng`, table detection on. Reads `{{ start.document_file }}`. Emits kreuzberg's native `ExtractionResult` shape 1:1 — `content`, `mime_type`, `metadata`, `tables`, `detected_languages` (no remap). |
| `classify` | llm (Ollama, native `/api/chat` at `:11434`) | `qwen3.5:9b` | Vision-capable. Image = `{{ start.document_file }}`. OCR text injected via `{{ ocr.content }}`. JSON-schema response. Requires `just dev ollama-up`. |
| `extract-bloodwork` / `extract-prescription` / `extract-clinical-note` / `extract-form-fields` / `extract-generic` | llm (Ollama, native `/api/chat` at `:11434`) | `qwen3.5:9b` | Type-specific system prompts. JSON-schema response with mandatory `citations[]` per field. Schema factored via top-level `definitions.ExtractionFields` (inlined by `compiler::schema_refs::inline_refs` at lowering). |
| `merge-extraction` | join (`mode: "any"`) | — | Branches converge. Re-parks whichever branch's payload under slug `extraction`. |
| `persist` | python | — | Single borrow `extraction.fields`. Stamps patient/class/date, emits a unified field list. |
| `verify` | python | — | Citation matcher (mekhan analogue of online-clinic's `provenance` step kind). Normalized substring match of every `citations[].supporting_text` against `{{ ocr.content }}`. |
| `review` | human_task | — | Two-step form: shows the original doc image, asks for approve / edit / re-OCR / reject. Only triggered when `verify.any_unverified || classify.confidence < 0.85`. |

## Slug-ref usage

Every cross-step borrow uses the producer-namespaced `<slug>.<field>` model from
`docs/10-control-data-token-model.md`. Examples in this graph:

- LLM prompt: `prompt: "...\n{{ ocr.content }}\n..."` (kreuzberg's native key)
- LLM image input: `images: [{ path: "{{ start.document_file }}" }]`
- Kreuzberg target: `file: "{{ start.document_file }}"`
- Decision guard: `classify.document_class in ["referral_letter", ...]`
- HumanTask body: `"Classification confidence: **{{ classify.confidence }}**"`
- End mapping: `{ targetField: "ma_decision", expression: "review.decision" }`
- Gate guard combining two steps: `verify.any_unverified || classify.confidence < 0.85`

## What this demo needs from the platform

Surfaced as we wrote the graph. None of these are showstoppers individually,
but they're the deltas vs. what currently ships.

1. **Workflow file fields → LLM `images[]`.** Today the LLM backend resolves
   `images[].path` against `{{input:NAME}}` (staged-input pattern, executor-side).
   The compiler currently does **not** lower a slug-ref file field
   (`{{ start.document_file }}`) into a staged input for an `llm` step. The
   `llm-smoke` demo explicitly calls this out. **In flight.**

2. **Backend-config string interpolation with `{{ slug.field }}`.** Mustache
   interpolation already works for HumanTask content blocks and `processName`.
   We're writing the LLM `prompt`, `system_prompt`, and Kreuzberg `file`
   as if the same interpolation reaches into `executionSpec.config` strings.
   The natural extension of the existing slug system.

3. **Unified `join` node with explicit `mode`.** `WorkflowNodeData::Join`
   subsumes both the AND-join (`mode: "all"`) and the XOR-join
   (`mode: "any"`, dual of decision). One node type, one knob — the user
   picks "wait for all" or "fire on any" in config; the type name doesn't
   encode the semantics. Shape:

   ```
   type: "join"
   mode: "all" | "any"
   // when mode == "all":
   mergeStrategy: "shallow_last_wins" | "deep_merge"
   ```

   Lowering is structural in both modes: incoming control edges feed a
   shared input place. For `mode: "all"` the transition fires when
   *every* input place has a token; for `mode: "any"` per arriving
   token.

4. **`output.kind = "json"`.** Each extractor emits a `fields: [...]`
   array. The output port kind we'd want is something richer than
   `text` (so the UI can render it as a table) but not strict per-field
   typing. Today the `TaskFieldKind` enum has `file/text/number/bool/...` —
   we're assuming `json` (or `object`) exists or gets added.

## Out of scope (vs. online-clinic source)

- **Surya OCR.** Kreuzberg + Tesseract covers the text path; no per-page rendered images yet (planned: `aithericon-executor-surya`, sub-phase 2.2b).
- **`Citation::EmbeddingChunk`.** No RAG primitive in mekhan yet — citation matcher only handles `ocr_span`.
- **Attribution trace.** No `attribution_trace` step kind / steering sidecar in mekhan.
- **SAE-based PII screen + safety steering.** Same — research-grade primitives not in mekhan.
- **Tool calling** (e.g. `lookup_icd10`). LLM backend doesn't expose a tools[] interface yet.

## Loading

Identical to the other bundled demos:

- `mekhan_service::demos::load_demo("service/tests/fixtures/clinic-workflows/document-pipeline-v1")`
- The startup seeder publishes it under templateId `00000000-0000-0000-0000-000000000050`.
- `MEKHAN__DEMOS__SEED=true` (default in `just dev::up-mekhan`).
