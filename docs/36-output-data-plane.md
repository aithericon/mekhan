# Output Data Plane — `set_output` Never Inlines Bytes, `log_artifact` Owns the Warehouse

Status: **Implemented (Phase 1)** — producer-side hard error landed in `executor-worker`. Phase 2 (compiler publish-time guard) is an optional follow-up; see §7.
Author: design discussion record.
Related code:
- `executor/crates/executor-worker/src/executor.rs` — `oversized_inline_output()` + the guard in `execute()`; `promote_file_output_to_store()` (the existing handle path); `DEFAULT_MAX_OUTPUT_INLINE_BYTES`.
- `executor/crates/executor-worker/src/config.rs` — `max_output_inline_bytes` (env `EXECUTOR_MAX_OUTPUT_INLINE_BYTES`).
- `executor/crates/executor-worker/src/staging.rs` — `StageInputsHook` materializes `InputSource::StoragePath` handles into `{run_dir}/inputs/`.
- `executor/packages/aithericon-sdk/src/aithericon/{_outputs.py,_artifacts.py,_files.py}` — `set_output`, `log_artifact`, `File`.
- `service/src/compiler/token_shape/types.rs` — `FileRef` shape; `borrow/planners/guard.rs` — `guard_readarc_plan` (borrow analysis).
Builds on: [`10-control-data-token-model.md`](./10-control-data-token-model.md).

## 1. The problem

`set_output(name, value)` inlines the value **by-value** into the net token: it
is parked write-once in the producer's `p_{id}_data` place and rides the
executor status update over NATS. NATS `max_payload` is **8 MiB**, and there was
**no size guard** on output values (`ExecutionResult.outputs:
HashMap<String, serde_json::Value>`). A large value bloated every parked token
and every downstream `<slug>.field` read-arc, and past the ceiling the status
message **dead-lettered silently** — the step appeared to hang with no
actionable signal.

`log_artifact`, by contrast, uploads (or registers by-reference) to the object
store, content-addresses, and catalogues — only a reference is ever small enough
to flow through a token. So a transport decision (inline vs. reference) was
being pushed onto the author as a naming decision (`set_output` vs.
`log_artifact`), and the author has no size information at authoring time.

## 2. The model — two roles, type-driven

Both verbs survive with one job each, and `set_output` no longer does naive file
handling.

| Verb | Role | Moves through the net? | Catalogued? |
|---|---|---|---|
| **`set_output`** | Control-plane return value the next node consumes: scalars, small JSON, and file **handles** | ✅ — values/handles, **never bytes** | — |
| **`log_artifact`** | The warehouse: preserve a file (upload/by-ref), content-address, provenance, catalogue. May be a pure side-product no port reads | reference only, if referenced at all | ✅ always |

**The invariant:** `set_output` never puts file bytes in a token. The only bytes
that travel are the ones the file-handle path or `log_artifact` push to the
object store.

The deciding signal is the **declared output type**, not a runtime heuristic:

- **Declared `file` output** → the producer writes the file and returns its path;
  the executor uploads it and rewrites the value to a small `{key:…}` handle
  (this is the "reference + upload" path).
- **A value** (`set_output` of a scalar/JSON) → inlined, unless it exceeds the
  inline byte limit, which is a **hard error** (§3). We do **not** silently
  auto-spill an untyped value to a handle: that would change a value into a
  reference behind the author's back and surprise both downstream borrowers and
  the consumer.

## 3. The decision (per output, at the producer)

In `JobExecutor::execute` (`executor-worker/src/executor.rs`), after output
collection + file-promotion and before the terminal status is built:

| Output kind | Serialized size | → Action |
|---|---|---|
| declared `file` | any | **Upload + return a `{key:…}` handle** via `promote_file_output_to_store`. Always small after promotion. |
| value (scalar/JSON) | under `max_output_inline_bytes` | **Inline** (parked by-value, as today). |
| value (scalar/JSON) | over `max_output_inline_bytes` | **Hard error** (`ExecutionOutcome::BackendError`): the step fails before publish with a message naming the remedies (declare the output a file, or use `log_artifact`). |

`oversized_inline_output()` reports the **largest** offender (deterministic,
not `HashMap`-iteration-order dependent). The guard runs *after* promotion, so
declared `file` outputs are already small handles and never trip it — only
genuinely inline values can.

**The failure status must itself be deliverable.** The terminal status detail
re-embeds `outputs`, so a naïve guard would leave the oversized value in the
`BackendError` status and that status would overflow the NATS payload ceiling
and dead-letter — the exact silent-hang failure mode this guard exists to
remove. `redact_oversized_inline_outputs()` therefore **drops** every
over-limit value (replacing it with a small `{"__omitted__": …}` placeholder)
before the outcome is set, so the actionable error always reaches the caller
regardless of how large the offending output was. Small sibling outputs are
preserved.

**Threshold:** `DEFAULT_MAX_OUTPUT_INLINE_BYTES = 1 MiB`, overridable via the
`max_output_inline_bytes` config field (env `EXECUTOR_MAX_OUTPUT_INLINE_BYTES`).
Distinct from `max_output_bytes`, which is the 64 KiB stdout/stderr **tail** cap.
1 MiB leaves ample headroom under the 8 MiB NATS ceiling for the rest of the
status detail (tails, `artifact_manifest`, `metrics`, `logs`).

## 4. Why this is the right shape — the round-trip already exists

The file-handle round-trip is **not new machinery** — it is load-bearing today:

- **Producer:** `promote_file_output_to_store()` uploads any `kind:"file"`
  output's local file to the shared `ArtifactStore` and rewrites its value to a
  `{key:<shared-path>}` handle.
- **Consumer:** `StageInputsHook` + `InputSource::StoragePath` downloads handles
  back into `{run_dir}/inputs/` before the next task runs (round-trip proven by
  `promotes_file_ref_object_and_key_is_downloadable`).

So the file path is the "reference + upload" leg of a clean data-plane /
control-plane split: **producers emit references for files; consumers
materialize them on the way in.** Phase 1 only adds the missing guard that keeps
*values* from impersonating that path.

## 5. The role overlap, resolved

A file that is both a downstream input *and* worth preserving sits on two axes:
a file-typed `set_output` emits the **handle** that wires the graph;
`log_artifact` decides whether the file is also **shelved** in the catalogue.
The upload + content-hash machinery is shared; only the catalogue row differs.
So the verbs are orthogonal, not redundant — one moves data between nodes, one
preserves it — and the only bytes in flight are the ones explicitly pushed to
the store.

## 6. Back-compat

- A `path` output of a small JSON value: unchanged.
- A `path` output that is genuinely a file used as a file downstream: declare the
  port `file`-kind so the handle flows instead of the bytes (a contract change
  the upgrade-preview `derive_child_io` diff already surfaces).
- A `set_output` of a large blob that "worked" only because it stayed under
  8 MiB: now fails loud at the producing step instead of risking a silent
  dead-letter — which is the intended behavior change.

## 7. Phase 2 (optional follow-up) — compiler publish-time guard

A latent silent bug remains: the runtime file handle is `{key, filename,
media_type}` while the compile-time File shape is `{url, filename,
content_type}`, and `FileRef` lowers to a permissive `{}` schema
(`token_shape/types.rs`), so a guard borrowing `<slug>.<filefield>.url`
resolves to `undefined` at runtime instead of being rejected.

`guard_readarc_plan()` already yields, per producer, the borrowed
`producer_path`s, and `node.output_ports()` exposes declared field kinds — so
the compiler can raise a `CompileError` (pre-publish) when a guard / loop / End
mapping borrows a **subfield of a `file`-typed output**. Borrowing the handle
scalar is fine; borrowing its *contents* is the error. This reinforces "files
are handles, values are borrowable" and is separable from Phase 1.

## 8. What this does and doesn't change

- **Does:** make `set_output` byte-safe by construction; remove the silent
  NATS-overflow failure mode; keep the value/file roles explicit (no silent
  value→handle conversion); document the threshold.
- **Doesn't:** change the control/data token model, read-arc synthesis, or the
  borrow-checker. `log_artifact` keeps its catalogue role; the file-handle
  round-trip (`promote_file_output_to_store` ↔ `StageInputsHook`) is unchanged.
