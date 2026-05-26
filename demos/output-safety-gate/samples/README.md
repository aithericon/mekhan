# output-safety-gate — samples

Two start-token payloads for manually exercising the gate:

| File | Expected verdict | Why |
|---|---|---|
| `safe-letter.json` | `pass` | Every clinical claim in `subject_text` is grounded in `evidence_text` (same diagnosis ICD code, same medication + dose, same follow-up interval). |
| `unsafe-letter.json` | `block` (or at least `warn`) | The subject invents a diagnosis (Diabetes) and a medication (Metformin 1000 mg) that are nowhere in the evidence. A well-calibrated critic should raise `unsupported_diagnosis` and/or `unsupported_claim`. |

Feed these as the Start node's input payload through `/api/instances` or the web editor. The critic's calibration depends on the model — a smaller LLM may need the prompt tightened to reliably catch the unsafe case.
