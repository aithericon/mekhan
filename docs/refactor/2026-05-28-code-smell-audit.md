# Code Smell & Duplication Audit — 2026-05-28

Cross-workspace audit of duplicated implementations and code smells across
`service/`, `engine/`, `executor/`, `app/`, and the build recipes. Produced by a
parallel five-way scan (one per workspace + one cross-cutting). Findings are
grouped by theme and ranked by impact within each group.

File:line references were accurate as of commit `c550a48` (main) on 2026-05-28.

## Status (round 2 landed on main `f82a2bd`, 2026-05-29)

> **Round 2 is merged into `main`** (`e02fa23`). The headline outcome table is in the
> next section; it is the authoritative tally. The per-workspace body sections further
> below were written as the **pre-round-2 baseline** and were NOT re-tagged after the
> merge — so a `⬜ OPEN`/`🟡 PARTIAL` tag in a body section is stale wherever the
> round-2 table lists that finding as resolved. Treat the **round-2 table + the
> "Still open on `main`" list** as ground truth; read the body sections for the
> original context only.

Tag legend (as originally scanned at `0f425a2`):

- **✅ FIXED** — resolved (or determined to be a non-issue / intentional).
- **🟡 PARTIAL** — improved or fixed in some call sites; copies remain.
- **⬜ OPEN** — unchanged since the baseline.

**Scoreboard (pre-round-2):** 5 fixed, 13 partial, 33 open (of 51 findings). The first round
was almost entirely service-side query plumbing; the 🔴 wire-type drift and the
canonical-builder cleanups were still untouched at that point. Line numbers in the body
sections are as of `0f425a2`; round 2 has since moved many of them.

---

## Round 2 outcome (2026-05-29) — ✅ MERGED TO MAIN at `e02fa23`

Round 2 ran as a 5-lane agent fan-out (one worktree per workspace) merged into an
integration branch (`refactor/code-smell-round-2-int`), which then **landed on `main`
at `e02fa23`**. **27 findings resolved this round** (mix of DONE / partial-by-design),
all verified compiling per workspace. See `ROUND-2-PROGRESS.md` for the orchestration log;
the concurrent-session / branch-move concern it raised is now resolved (the `stepsRef`
work also landed independently, at `f970467`).

| Lane | Resolved | Notes |
|---|---|---|
| **executor** | X1, X2, X3, X4, X5, X7, X8, X9, X10 | all 9; X5/X10 were correctness; X9 (bonus) done |
| **service** | S2, S4, S5, S9, S10, S11, S13, B1, B3 | S2 was already done by prior sweeps; **S11 partial** (3 of 5 ydoc sites unified — and it surfaced a real latent bug: publish/new_version silently served the stale DB graph column on Y.Doc reconstruct failure); **S6 SKIPPED** (CompileOptions collapse — 150+ caller surface, too risky) |
| **engine** | E2, E5, E6, E9, E11, B5 | all 6; E6/E9 correctness; E5 full `register_effect_handlers` extraction verified under all feature gates |
| **frontend** | P1, P2, P4, P5, P6, P7 | all 6; svelte-check 0 errors, 92 vitest pass |
| **build** | BR1, BR2 | shared `just/cargo.just`; NATS tag aligned |
| **cross-cut** | **B2** | `cancel_subject`/`cancel_subject_filter` in executor-domain, all 3 sites routed + unit test |

**Still open on `main` (re-verified at `f82a2bd`, 2026-05-29):**
- **🔴 Batch 4 wire-types (A1, A2, A3)** — confirmed still present:
  - **A1** — `ExecutionStatus` is still mirrored in both `engine/core-engine/crates/domain/src/executor.rs:30` and `executor/crates/executor-domain/src/status.rs:10`.
  - **A2** — `service`'s hand-mirrored `PhaseUpdateStatus` (`models/template.rs:1015`) and `StepStatus` (`projections/step_executions/projector.rs:55`) both still exist.
  - **A3** — two divergent `sanitize_subject_token` (`service/src/observability.rs:189` vs `executor/crates/executor-domain/src/status.rs:109`).
  - It's a 3-binary rebuild/restart/republish arc — see the handoff at the bottom of this doc.
- **S6** (compile_to_air → CompileOptions) — ✅ **DONE on unmerged branch `refactor/s6-compile-options`**
  (2026-05-29). The audit's "~150-site" fear was wrong: the simple `compile_to_air(graph,name,desc,files)`
  has 157 callers but is the *non-smell* case — left untouched. The actual smell was only the 3 additive
  `_with_subworkflows{_inline,_and_interfaces,_interfaces_and_configs}` wrappers (~17 call sites, mostly
  `demos.rs`), now collapsed into one `compile_to_air_with_options(.., CompileOptions)` returning a named
  `CompileArtifacts`. `compile_to_air_with_shapes` (`token_shape/annotate.rs:27`, 0 callers) left as-is
  (orthogonal axis). Verified: `cargo check --all-targets` clean; lib 467 / air_snapshots 12 (goldens
  byte-identical) / compiler_tests 104 pass. **Not yet merged to main.**
- **Engine internals not in round 2:** E7 (`std::sync::RwLock` → `parking_lot` across ~44 sites
  in `application/src/service.rs`), E8 (`block_in_place(block_on(...))` adapter callbacks),
  `notify_adapters` near-dup (`api/src/handlers.rs` vs `net_registry.rs`), and the `ExternalSignal`
  serde roundtrip test still duplicated across crates. (E2/E5/E6/E9/E11 all landed.)
- **Executor not in round 2:** `resolved_storage_owned` deser+fallback dup (`staging.rs`/`executor.rs`),
  the 339-line `LlmBackend::execute`, and the Docker prepare/execute config re-parse (X5 addressed the
  caching gap; the structural split did not land). (X1–X4, X7–X10 landed.)
- **Frontend follow-ups not in round 2:** P3 (LLM-panel `SchemaForm` migration / `useJsonDraft`),
  P8 (`text-sm font-medium text-muted-foreground` label still ~76×), P9 (`TriggerNodeSection` 707-line
  split + inline Rhai `rootRefs` parser), P10 (`worldPosOf` IIFE, `BlockTypePicker` 9 `add*` fns),
  P11 (Svelte-4-ism modernization). (P1/P2/P4/P5/P6/P7 landed.)
- **B4** (divergent NATS dev ports across executor/engine/service) — dev-ergonomics footgun, accept-as-is.
- Items previously marked PARTIAL whose *remaining* copies were intentional (S11's 2 divergent
  sites, S12, E1/E3/E4/E10) — no action, correct as-is.

---

## 🔴 Highest impact: wire-type drift across the 3 binaries

Types hand-mirrored across workspaces that the type system **won't** catch when
they drift. This is the "typed roundtrip silently drops fields" failure mode the
project has hit repeatedly (mekhan-service + core-engine + executor are three
separate binaries in three workspaces).

| Status | Type | Where it's duplicated | Risk |
|---|---|---|---|
| ⬜ OPEN | **`ExecutionStatus`** | `executor/crates/executor-domain/src/status.rs:10` vs `engine/core-engine/crates/domain/src/executor.rs:30` (engine still keeps its own copy; `engine/.../executor/Cargo.toml` depends on `executor-domain` for *other* types but mirrors this enum, comment at `executor.rs:18` admits it) | New variant → engine silently drops it from the NATS stream |
| 🟡 PARTIAL | **`Phase` / `PhaseStatus` / `Progress`** | canonical in `executor/crates/executor-domain/src/progress.rs:8`; `app/src/lib/types/process.ts:24` now documents itself as mirroring executor-domain via the ingest projector; but `service/src/models/template.rs:945` (`PhaseUpdateStatus`, still omits `Pending`) and `service/src/projections/step_executions/projector.rs:55` (`StepStatus`) remain **hand-mirrored Rust enums** | unknown variant → runtime deser error in the causality ingest projector |
| ⬜ OPEN | **`sanitize_subject_token`** | `executor/crates/executor-domain/src/status.rs:89` (replaces ` `, `>`, `*`) vs `service/src/observability.rs:189` (replaces everything non-`[A-Za-z0-9_-]`, incl. `.`) | *Different char sets* → a `.` in an execution ID misroutes one subject but not the other |

**Suggested direction:** lift the canonical status/progress types into a `shared/`
crate (or have engine depend on `executor-domain`, since it already consumes those
NATS subjects), and surface `Phase*` through OpenAPI so `process.ts` is generated
instead of mirrored. Single most valuable cleanup. Adopt the stricter
(observability) sanitizer ruleset as the canonical one.

> Gotcha: a change to shared executor-domain wire types needs **all three**
> binaries rebuilt + restarted + republished, and the engine's typed
> `ExecutionSpec` roundtrip silently drops unknown fields. Always
> `cargo check -p mekhan-service` after touching compiler-side literals.

---

## 🟠 Magic strings with no canonical builder

Subject / path / key formats typed out at many call sites — typo-silent breakage.

- 🟡 PARTIAL — **S3 template key** `templates/{tid}/v{v}/{node}/...`: `ArtifactStore::node_config_key()` exists at `service/src/s3.rs:170`, but the compiler still mints the same format inline via `ConfigStorage::key()` (`service/src/compiler/lower/mod.rs:258`), and tests re-derive the literal. No `paths` module routes all callers.
- ⬜ OPEN — **`executor.cancel.{id}`** built independently at `engine/core-engine/crates/executor/src/client.rs:402`, `executor/crates/executor-worker/src/cancel.rs:79`, and `executor/crates/executor-test-harness/src/context.rs:517`. No `cancel_subject()` in `executor-domain`.
- ⬜ OPEN — **Vault path** `aithericon/resources/{ws}/{r}/v{n}`: `vault_path_for` is still private (`service/src/handlers/resources.rs:220`); test files across two workspaces re-derive the literal (`service/tests/resources_handlers.rs:243,400,510`, etc.). Expose from `shared/`.
- ⬜ OPEN — **NATS dev ports diverge**: executor defaults `4222` (`executor/crates/executor-worker/src/config.rs:382`), engine `4333` (`engine/.../nats/src/config.rs:40`), service `14333` (`service/src/config.rs:258`). An executor launched by root `just dev` connects to the wrong port without an explicit override.
- ⬜ OPEN — **Timer KV key** `timer.{}.{}.{}` typed twice in `engine/.../nats/src/clockmaster.rs:60,88` (schedule vs cancel); plus dual `SIGNAL_PREFIX` consts (`clockmaster.rs:18` vs `subjects.rs:216`).

---

## 🟠 `service/` (mekhan) duplication

First refactor round landed here — three findings closed, several improved.

- 🟡 PARTIAL — **Template-by-ID query**: `require_template` helper extracted (`handlers/mod.rs:34`) and adopted by the common get/edit/delete paths. **3 inline copies remain** for non-standard queries: `handlers/yjs_sync.rs:50` (latest-by-id) and the versioned fetches at `triggers/dispatcher.rs:447` + `handlers/triggers.rs:565`.
- 🟡 PARTIAL — **`map_err(... ApiError::internal)`**: down from 53+ to **~28**. Commit `4d04351` swept the sqlx call sites; most survivors are legitimate non-sqlx conversions (serde_json, graph parse, borrow-check) where the boilerplate is real. Worth a final pass to confirm none are plain sqlx `?` candidates.
- ✅ FIXED — **List queries swallowing DB errors**: `.unwrap_or_default()`/`.unwrap_or((0,))` on query results are gone (`ac8fdfd`); `list_templates` and the resource/instance lists now propagate with `?`.
- ⬜ OPEN — **`validate_placeholders` byte-identical** in `backends/smtp.rs:207` and `compiler/backend_configs.rs:34` — the comment still admits the copy. Widen visibility, delete the copy. *(Cheapest open item in the repo.)*
- ⬜ OPEN — **Resource version-insert + vault-write + rollback** copy-pasted across `create_resource`/`update_resource`/`rotate_resource` (`handlers/resources.rs:485,679,826`). Extract `write_resource_version(...)`.
- ⬜ OPEN — **`compile_to_air` → 4-deep wrapper chain** (`compiler/compile.rs:207,234,261,287`), each adding one optional arg — collapse to a `CompileOptions` struct + two real entry points.
- 🟡 PARTIAL — **Pagination**: `query/pagination.rs` `PageQuery`/`Paginated<T>` is now wired into the new query path, but `default_page`/`default_per_page` are still defined 3× (`models/instance.rs:135`, `models/resource.rs:212`, `models/template.rs:1985`); CLI keeps its own `PaginatedResponse` for deser (acceptable, decoupled).
- ✅ FIXED — **`RefSite` construction**: `RefSite` is now a proper struct in `backends/mod.rs:305`; per-backend scanners construct it legitimately (different ref patterns per backend), so the remaining inline construction is dispatch, not duplication.
- 🟡 PARTIAL — **Identifier regexes**: `PATH_REGEX` (`handlers/resources.rs:59`) and `KV_KEY_REGEX` (`resources.rs:92`) are still two identical `^[a-z][a-z0-9_]*$` regexes in the same file; `is_rhai_ident` (`compiler/validate.rs:962`) is the hand-rolled equivalent. Collapse at least the two in `resources.rs`.
- ⬜ OPEN — **`base_template_id.unwrap_or(existing.id)` chain-root idiom ~12×** in `handlers/templates.rs` (+ `template_tests/mod.rs:416`, `process/publish.rs:550`) — add `WorkflowTemplate::chain_root_id()`.
- 🟡 PARTIAL — **Y.Doc fallback match still copy-pasted 5×** (`handlers/templates.rs:449,655,1116,1340,1445`) and **still has inconsistent error mapping** (`internal` vs `bad_request` vs silent `HashMap::new()`/`default_graph`). This is the correctness-relevant one — extract `graph_with_ydoc_fallback(...)` and pick one error contract.
- ✅ FIXED — **`list_templates` WHERE predicate built twice**: still two QueryBuilders (data + count) but now an acknowledged, commented trade-off given sqlx's limitations, not drift. Treat as intentional.
- 🟡 PARTIAL — **`serde_json::to_value(&graph).unwrap()`**: two bare unwraps remain in live handlers (`templates.rs:99,500`); a third was fixed to `map_err`. Crate-wide `#![allow(dead_code)]` in `lib.rs:1` is still present.

---

## 🟠 `engine/` duplication

- 🟡 PARTIAL — **`notify_adapters` near-duplicated** in `api/src/handlers.rs:536` and `api/src/net_registry.rs:1179`. The only difference (one fires `eval_notify`) is semantic — needs a documented-intent unify, not a blind merge.
- ⬜ OPEN — **Read-input extraction copy-pasted** between the live and replay paths in `application/src/firing.rs:497` / `:836`; `process_step` expression repeated. (map-reduce/loop-accumulator merges moved the line numbers but not the duplication.)
- ✅ FIXED — **`sig_key`/`dedup_id` normalization**: the contract is consolidated on the `ExternalSignal` type; the residual 2–3 lines per listener (`signal_listener.rs:250`, `global_signal_listener.rs:191`, `global_bridge_listener.rs:196`) are acceptable and not worth a forced extraction.
- ✅ FIXED — **`MarkingProjection::project`**: confirmed a correct wrapper (domain `project_marking` is the pure logic; the infra type is a `StateProjection` trait impl over it), not a real duplication.
- ⬜ OPEN — **`get_or_create` god function** (~438 lines now, `api/src/net_registry.rs:496`) doing store/TLS/scheduler/handler-registration/spawns. Extract `register_effect_handlers`.
- ⬜ OPEN — **`TransitionFired` always sets `transition_name: None`** for Rhai transitions (`firing.rs:420`) even though `transition.name` is in scope (`firing.rs:357`) — silent field drop into CLI/consumers; effect path populates it correctly. *(One-line correctness fix.)*
- ⬜ OPEN — **Pervasive `RwLock::...().unwrap()`** in `application/src/service.rs` (~44 sites; poisoned lock → engine panic). Consider `parking_lot` (the registry already uses it).
- ⬜ OPEN — **`block_in_place(block_on(...))` anti-pattern** in both adapter callbacks (`handlers.rs:579`, `net_registry.rs:1228`) — blocks a worker thread per call.
- ⬜ OPEN — Runtime `.unwrap()` on serialize/timestamp in the clockmaster fire path (`clockmaster.rs:233,234,261`).
- ✅ FIXED — **Watcher constructors**: both `nomad/src/watcher.rs` and `slurm/src/watcher.rs` now import `SignalPublisher`/`CheckpointStore` from the shared `petri-scheduler-bridge` crate — consolidation already in place. (Double read-lock in slurm `process_squeue_entry` not re-verified; treat as a minor leftover.)
- ⬜ OPEN — `ExternalSignal` serde roundtrip test duplicated in 3–4 crates (`nomad/watcher.rs:461`, `slurm/watcher.rs:705`, `nats/signal_listener.rs:295` + a second at `:317`).

---

## 🟠 `executor/` duplication

**No executor refactoring landed this round — every item below is unchanged.**

- ⬜ OPEN — **JetStream publish boilerplate triplicated** across `executor-worker/src/reporter.rs:131,171` and `event_emitter.rs:47` (serialize → headers → publish → ack → log). Extract `publish_event(...)`.
- ⬜ OPEN — **Cancelled/TimedOut `ExecutionResult` built from scratch** in 4 in-process backends (`executor-http/src/lib.rs:488,503`, `executor-llm/src/backend.rs:183,197`, `executor-file-ops/src/backend.rs`, `executor-kreuzberg/src/backend.rs:143,157`). Add `ExecutionResult::cancelled()/timed_out()` to `executor-domain/src/result.rs`.
- ⬜ OPEN — **`DEFAULT_MAX_OUTPUT_BYTES = 64*1024`** in `executor-docker/src/lib.rs:15`, `executor-python/src/lib.rs:21`, `executor-process/src/lib.rs:15` (+ a `65536` literal in `executor-worker/src/config.rs:403`). Move to the shared backend crate.
- ⬜ OPEN — **`from_spec` deser pattern copy-pasted into all backend configs** — now **6** files: `executor-backend-configs/src/{docker,http,python,smtp,process,file_ops}.rs`. Add a blanket `from_spec<T>(spec, name)`.
- ⬜ OPEN — **Docker breaks the prepare/execute contract**: `DockerConfig::from_spec` parsed in `prepare` then discarded (`backend_state` holds only `{"docker_prepared": true}`, `executor-docker/lib.rs:66,72`) and re-parsed in `execute` (`:85`), while http/file-ops/kreuzberg/llm cache the parsed config in `backend_state`.
- 🟡 PARTIAL — **`InjectEnvironmentHook`** still injects `AITHERICON_*` host paths (`staging.rs:188`) that Docker overrides with container paths (`container.rs:174`) — but Docker now *filters* the host entries before the container sees them (`container.rs:265`), so they no longer leak into `context.json`. Remaining waste: the host paths are computed then thrown away.
- ⬜ OPEN — **`subject_prefix` match arm repeated ~7×** in `reporter.rs`/`event_emitter.rs` — add `subject_for()`/`stream_name_for()` helpers.
- ⬜ OPEN — **`RunContext` test fixture spelled out (15 fields)** in every conformance kit (`executor-test-harness/conformance/*_kit.rs`, 6 kits) + inline helpers. Add `RunContext::for_test(...)`.
- ⬜ OPEN — **`resolved_storage_owned` deser + fallback** duplicated in `staging.rs` and `executor.rs:729`; two near-identical output-collection loops in `executor.rs:295,317`.
- ⬜ OPEN — `LlmBackend::execute` is a 339-line function (`executor-llm/backend.rs:97`); runtime `.unwrap()` on `target_file` in `kreuzberg/backend.rs:121` (no compile-time guarantee single-mode resolved config has it).

---

## 🟡 `app/` (Svelte) duplication

The `fe-dispatch` and `config-schema` merges resolved the architectural smells
(god component → dispatch registry; Docker/Process panels → generic `SchemaForm`).
Small util duplication is mostly untouched.

- ⬜ OPEN — **`portsEqual` copied** between `AutomatedStepSection.svelte:125` and `SubWorkflowSection.svelte:211`. Extract to `$lib/editor/port-utils.ts`.
- ⬜ OPEN — **`familyId` copied** in `ChildWorkflowBrowser.svelte:43` and `SubWorkflowSection.svelte:58`.
- 🟡 PARTIAL — **`setOrDelete` + JSON draft-sync**: Docker/Process config panels were replaced by the generic `SchemaForm.svelte` (draft-sync boilerplate gone there). `AgentNodeSection.svelte` and `automated/LlmConfigPanel.svelte` still carry their own `setOrDelete`; no shared `useJsonDraft` helper.
- ⬜ OPEN — **Debounced-derive pattern** (`deriveTimer`/`deriveSeq`) still duplicated in `AutomatedStepSection.svelte:81` and `SubWorkflowSection.svelte:168`. Extract `createDebouncedFetcher`.
- ⬜ OPEN — **`sanitizeSlug` re-inlined** in `NodePropertyPanel.svelte:63` even though `$lib/editor/sanitize-slug.ts` exists.
- ⬜ OPEN — **Raw `fetch('/api/v1/triggers/...')` bypassing the typed client** in `TriggerNodeSection.svelte:59`, `CronPreview.svelte:41`, `TriggerHistory.svelte:28` — skips the session-expiry middleware. Add the endpoints to `client.ts`.
- ⬜ OPEN — **"append snippet to field" boilerplate ~8×** across section components (`PhaseUpdateNodeSection`, `ProgressUpdateNodeSection`, `FailureNodeSection`, `HumanTaskSection`, `StartNodeSection`, `human-task/StepEditor`, `human-task/CalloutBlockEditor`, `SmtpConfigPanel` — the last with a divergent no-space variant). Add `appendSnippet(curr, snippet)`.
- 🟡 PARTIAL — **Tailwind strings**: empty-state class down to ~2 uses, mapping-row class gone; but `text-sm font-medium text-muted-foreground` label is still ~76×. (Keep ≥`text-sm` in editor sidebar/property panels.)
- 🟡 PARTIAL — **God components**: `NodePropertyPanel.svelte` shrank 366→315 lines via the `NODE_PROPERTY_SECTIONS` dispatch registry (`$lib/editor/node-property-sections.ts`). `TriggerNodeSection.svelte` is **unchanged at 707 lines** (inline Rhai `rootRefs` parser at `:158` still belongs in a util).
- ⬜ OPEN — `worldPosOf` re-inlined as an IIFE in `WorkflowCanvas.svelte` (named fn at `:138`); `BlockTypePicker.svelte` still has 9 near-identical `add*` fns (make data-driven).
- 🟡 PARTIAL — Svelte-4-isms (`onMount`+`onDestroy`) remain in `TriggerHistory.svelte`, `ConnectionStatus.svelte`, `AwarenessBar.svelte`, and `CronPreview.svelte` (the last now *mixes* `$effect` and `onMount`); `let cancelled = false` race-guard still hand-rolled in 4 effects.

---

## 🟡 Build / recipe duplication

- ⬜ OPEN — **`build`/`check`/`fmt`/`lint`/`test`/`clean` copy-pasted** across `executor/justfile`, `engine/justfile`, and root, still with **inconsistent scope** (engine `build` = `-p core-engine`, executor = `--workspace`). No shared `just/cargo.just` module yet.
- 🟡 PARTIAL — **NATS recipes**: engine + root now delegate to docker-compose (`nats:2.12-alpine`, ports `14333`/`4333`). `executor/justfile:38` still has a standalone inline `docker run nats:latest` on `4222` — image-tag and port divergence persists there.

---

## Next round — scoping

> **Status update (main `f82a2bd`):** Batches 1–3 below all **landed in round 2**
> (`e02fa23`). They are kept here for the historical plan-of-record. What actually
> remains is **Batch 4** (wire-types A1/A2/A3) + **S6** (compile_to_air collapse,
> deliberately skipped) + the leftover engine/executor/frontend items enumerated in
> the "Still open on `main`" list above. The Batch-4 handoff at the very bottom is the
> live next step, and its sequencing precondition ("after `stepsRef` and round-2 land")
> is now **satisfied** — both are on `main`.

The first round cleared the service-side query plumbing. The remaining work
sorts into four reviewable batches, ordered by value-per-risk. Batches 1–3 are
each a single self-contained PR; Batch 4 is a deliberate multi-binary arc.

### Batch 1 — Silent-correctness fixes ✅ LANDED in round 2

These drop or mis-handle data silently. Small diffs, high value, no API surface.

- **E6** — `firing.rs:420` set `transition_name: Some(transition.name.clone())`. One line; stops a silent field drop into CLI/consumers.
- **S11** — extract `graph_with_ydoc_fallback(...)` in `handlers/templates.rs` and pick **one** error contract (the 5 copies currently disagree: `internal` / `bad_request` / silent default).
- **X5** — cache `DockerConfig` in `backend_state` in `prepare` (match the other backends) instead of re-parsing in `execute`.
- **X10** — replace the `target_file.unwrap()` in `kreuzberg/backend.rs:121` with a checked error.
- **E9** + service `templates.rs:99,500` — turn the remaining runtime `.unwrap()`s on serialize/timestamp into `map_err`.

### Batch 2 — Canonical builders ✅ LANDED in round 2

Pure extraction, no behavior change. Kills the typo-silent magic strings.

- **B2** `cancel_subject()` in `executor-domain`, routed from all 3 call sites.
- **B3** make `vault_path_for` public (or move to `shared/`), delete the test re-derivations.
- **B5** dedupe `SIGNAL_PREFIX` (keep `subjects.rs`, drop `clockmaster.rs`); extract the `timer.{}.{}.{}` key builder.
- **B1** finish the S3 `paths` module so the compiler's `ConfigStorage::key()` and `s3.rs::node_config_key()` share one formatter.
- **S4** widen `compiler::backend_configs::validate_placeholders` visibility, delete the `smtp.rs` copy. (Trivial; the comment already asks for it.)

### Batch 3 — Mechanical dedup ✅ LANDED in round 2 (except S6, P3, P8–P11, and the leftover engine/executor items)

Lower risk ergonomic debt. The bulk landed; the exceptions are tracked in "Still open on `main`" above.

- **executor**: `publish_event(...)` (X1), `ExecutionResult::cancelled()/timed_out()` (X2), shared `DEFAULT_MAX_OUTPUT_BYTES` (X3), blanket `from_spec<T>` (X4), `subject_for()`/`stream_name_for()` (X7), `RunContext::for_test(...)` (X8).
- **service**: `write_resource_version(...)` (S5), `chain_root_id()` (S10), `CompileOptions` collapse (S6), finish the `map_err`→`?` sweep (S2), collapse the two `resources.rs` regexes (S9).
- **engine**: extract `register_effect_handlers` from `get_or_create` (E5); hoist the `ExternalSignal` roundtrip test into the domain crate (E11); `firing.rs` read-input helper (E2).
- **frontend**: `port-utils.ts` (P1), `familyId` util (P2), `createDebouncedFetcher` (P4), import `sanitizeSlug` (P5), `appendSnippet` (P7), typed trigger-client endpoints (P6).
- **build**: shared `just/cargo.just` (BR1); point executor's NATS recipe at the root compose (BR2).

### Batch 4 — Wire-type consolidation (🔴, schedule deliberately)

Highest impact but needs **all three binaries rebuilt + restarted + republished**,
so it's its own arc, not folded into the above.

- **A1/A2** — lift `ExecutionStatus` + `Phase`/`PhaseStatus`/`Progress` into `shared/` (or have engine depend on `executor-domain`), surface `Phase*` through OpenAPI so `process.ts` is generated, and delete the `PhaseUpdateStatus`/`StepStatus` hand-mirrors.
- **A3** — adopt the stricter `observability.rs` sanitizer as canonical, share it, delete the executor-domain variant.
- **E7** (`parking_lot` migration of `service.rs` locks) and **E8** (`block_in_place` removal) are engine-internal but riskier; pair them with this arc or defer.

### Deferred / accept-as-is

- **S12** (double QueryBuilder) and **E1/E3/E4/E10** are intentional or already
  consolidated — no action.
- **B4** (divergent NATS dev ports) is a dev-ergonomics footgun, not a bug; fold
  into BR2 if touching the recipes anyway.
- **P9 `TriggerNodeSection` 707-line split**, **P3 LLM-panel `SchemaForm` migration**,
  and **P11 Svelte-4-ism modernization** are larger frontend refactors — separate
  follow-ups once the small-util extractions land.

---

## Batch 4 handoff — wire-type consolidation (deferred from round 2)

Round 2 deliberately did NOT touch A1/A2/A3. This is the concrete plan for a dedicated PR.

> **Precondition now satisfied (main `f82a2bd`):** both the `stepsRef`/dynamic-human-task
> work (`f970467`) and round 2 (`e02fa23`) have landed on `main`, so the 3-binary state is
> stable and this PR is unblocked. All three mirrors re-confirmed present (see "Still open on
> `main`" above) — A1/A2/A3 are exactly as described below.

**Why it was deferred:** (1) it requires all three binaries rebuilt + restarted + republished
(the engine's typed `ExecutionSpec` roundtrip silently drops unknown fields, so a mismatch is a
*runtime* failure, not a compile error); (2) during round 2 a concurrent session was actively
editing `service/src/compiler/*` and `service/src/models/template.rs` — exactly where A2's
`PhaseUpdateStatus`/`StepStatus` consolidation lands — so merging blind risked collisions.

**Sequence (one PR, but stage the commits so each compiles):**
1. **A3 first (smallest, lowest blast radius).** Make `executor-domain` re-export a single
   `sanitize_subject_token` and have `service/src/observability.rs` use it (or move the canonical
   one to `shared/`). ⚠️ Behavior change: the executor-domain variant only strips ` `/`>`/`*`,
   while observability strips everything non-`[A-Za-z0-9_-]` (incl. `.`). Adopting the stricter
   one changes published subjects for any execution_id containing a `.` — audit live execution_id
   formats first (UUIDs are unaffected). This also finally lets **B2's `cancel_subject` sanitize**
   (it's currently intentionally unsanitized to preserve the publish path — see status.rs note).
2. **A1.** Delete `engine/core-engine/crates/domain/src/executor.rs`'s mirror `ExecutionStatus`;
   have `petri-domain` re-export `aithericon_executor_domain::ExecutionStatus` (the executor crate
   already depends on it for other types). Rebuild engine + executor; run the NATS status-stream
   integration test to confirm no variant drops.
3. **A2.** Surface `Phase`/`PhaseStatus`/`Progress` through mekhan's OpenAPI (`#[derive(ToSchema)]`
   + a handler/DTO reference so they appear in `openapi-mekhan.json`), regenerate
   (`just dev::openapi`), and replace `app/src/lib/types/process.ts`'s hand-mirror with the
   generated types. Then delete `service`'s `PhaseUpdateStatus` (re-add `Pending`) and fold
   `StepStatus` (`projections/step_executions/projector.rs`) onto the canonical enum.
   Verify `just ci::openapi-drift` is clean.
4. Validation gate: `just ci::check-rust` + `just ci::quality-frontend` + the causality ingest
   projector test (the consumer that would runtime-fail on an unknown `PhaseStatus` variant).

**Companion (optional, engine-internal, can ride along or defer):** E7 (`std::sync::RwLock` →
`parking_lot` across the ~44 sites in `application/src/service.rs`) and E8 (drop the two
`block_in_place(block_on(...))` adapter callbacks). Riskier; only with careful test coverage.
