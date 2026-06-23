# Demos

Built-in demo workflows shipped with mekhan. Each subdirectory is a single
template, ready to publish.

## Layout

```
demos/<name>/
  demo.json                   # stable templateId + name + description
  graph.json                  # the WorkflowGraph (JSON), small + structural
  nodes/<node-id>/
    main.py                   # per-node source (real .py — IDE, ruff, type-check all work)
    task.json                 # HumanTask form definition (overlay onto data.steps)
```

`nodes/<id>/task.json` is the HumanTask sidecar: each HumanTask is a
node like any other, so its form definition (the verbose `steps`
block tree with all the form fields and instructions) lives next to
the executable files of other node types. `graph.json` carries
`steps: []` for those nodes and the loader merges the sidecar before
returning. Identifying metadata (label, taskTitle, instructionsMdsvex)
stays inline in `graph.json` so the graph still reads at a glance.

Same on-disk shape as the GitOps `mekhan pull/apply` flow modulo the
sidecar split — a hand `mekhan apply demos/<name>/` against the
shipped fixture would fail at publish ("HumanTask rejects empty
steps") because that path doesn't merge sidecars. Use the in-process
loader (or the startup seeder, which calls the loader) for the
shipped demos.

## Loading

- **From Rust**: `mekhan_service::demos::load_demo(path)` returns the
  `(metadata, graph, files)` triple any `/api/v1/templates` consumer
  expects. `list_demo_dirs(root)` enumerates the directory.
- **From the CLI**: `mekhan apply demos/<name>/` (see
  `service/src/bin/cli/apply.rs`).
- **From tests**: same `load_demo` call — tests against the literal
  shipped demo, no hand-rolled graph drift.

## Currently bundled — learning path

The numbered demos are a progression: each step adds one new capability
on top of the previous one. Read them in order — by `06-` you have
seen every primitive the editor exposes. `invoice-processing/` is the
capstone that ties them together.

| # | Demo | What's new |
|---|------|-----------|
| 01 | `01-hello-world/` | The minimal shape: Start → AutomatedStep → End. One Python step, implicit output sweep. |
| 02 | `02-human-form/` | HumanTask with a `task.json` sidecar. End reads a HumanTask form field. |
| 03 | `03-decision-routing/` | AutomatedStep produces a derived field; Decision branches on it via a `<slug>.<field>` guard. |
| 04 | `04-loop-counter/` | Loop with body wired through `body_in` / `body_out`. Stop condition lives in `loopCondition`. |
| 05 | `05-parallel-fanout/` | ParallelSplit fans two AutomatedSteps; Join (`mode: all`) merges them back. |
| 06 | `06-subworkflow/` | Flow-in-flow: parent embeds `01-hello-world` via a `sub_workflow` node + `inputMapping`. |
| 07 | `07-ocr-classify-extract/` | LLM + Kreuzberg consume upstream-producer fields via `{{ <slug>.<field> }}` placeholders — same convention HumanTask uses. Start uploads a PDF, Kreuzberg reads `{{ start.document }}`, LLM classifier reads `{{ extract_text.content }}` (kreuzberg's native ExtractionResult key — declarations match 1:1, no remap). |
| 08 | `08-failure-handling/` | AutomatedStep's red `error` handle: a Python `raise` (or `sys.exit(<nonzero>)`) routes out the error port once `retryPolicy.maxRetries` is exhausted. Wired to a Failure node + dedicated End so the run completes with a structured `{ ok: false, error: { reason, value } }` envelope. |
| 08a | `08a-order-lookup/` | Tool child for demo 09. A tiny `Start{order_id} → Python → End{status,eta}` workflow. Its Start contract becomes the agent tool's input schema when referenced via a `tools` handle — the same way `01-hello-world` is the child behind `06-subworkflow`. Seeds before 09 (alphabetical). |
| 09 | `09-agent-tool-loop/` | Agent node whose `lookup_order` tool is a **SubWorkflow** (the 08a child) — the canonical "tool with an explicit Start/End contract" shape. Multi-turn LLM (Ollama, native `/api/chat` tools) reads a customer message, calls the tool (spawning the child net), reads `status`/`eta`, then composes a final reply. Exercises the full agent loop topology (parked state, dispatch/collect per tool, route guards) AND sub-workflow spawn from inside a tool dispatch. Requires a tool-capable Ollama model — `up-ollama`'s default `qwen3.5:9b` works (any qwen2.5+/qwen3+/llama3.1+). |
| 11 | `11-http-call/` | HTTP executor backend with borrowed `{{ slug.field }}` references in URL/headers/query/body, plus `output_mapping` back out of the echoed response. Pair with `just dev httpbin-up`. Needs no workspace resource. |
| 19 | `19-postgres-node/` | Postgres executor backend (`backendType: postgres`), resource-bound via ConfigOverlay. `Start → Postgres(read) → Postgres(write RETURNING) → End`: a read selects from a dedicated `demo_pg.widgets` fixture table (`$1` bound from `{{ start.min_id }}`, projection `[id,name]`), a write INSERTs `RETURNING id, name`. Binds the `demo_pg` resource (auto-seeded from `demos/resources/demo_pg.json`); seed the fixture table once with `just dev pg-demo-seed`. Never touches mekhan's own tables. |
| 20 | `20-loki-query/` | Loki/LogQL executor backend (`backendType: loki`), resource-bound via ConfigOverlay. `Start{app, since} → Loki query_range → End`: runs `{job="varlogs", app="{{ start.app }}"}` over the `{{ start.since }}` lookback window with the `app` value spliced through a LogQL-escaping render (the LogQL analog of a `$1` bind). Binds the `demo_loki` resource (auto-seeded from `demos/resources/demo_loki.json`) at `http://localhost:3100`; push test lines into Loki first (see the demo README). |
| 25 | `25-prometheus-query/` | Prometheus/PromQL executor backend (`backendType: prometheus`), resource-bound via ConfigOverlay. `Start{} → Prometheus query → End`: runs the instant query `up` (the per-target scrape-health metric) with no inputs and no setup — stock `prom/prometheus` self-scrapes so `up` is available immediately. Binds the `demo_prometheus` resource (auto-seeded from `demos/resources/demo_prometheus.json`) at `http://localhost:9090`; metrics-shaped envelope (`result_type`, `series`, `samples`, `sample_count`, `scalar`, `stats`). |
| 37 | `37-internal-pool-agent/` | Model-pool P1 (docs/28 + docs/29) — `Start{question} → Agent → End{reply}`, a single-shot (degenerate, no-tools, `maxTurns 1`) Agent bound to a self-hosted model served through the in-cluster inference router. Inference bypasses the engine Petri net + the presence net: the Agent's degenerate executor job makes a conventional OpenAI HTTP call to `base_url → router`, never net-admitted. Binds the `internal_pool_router` (`internal_llm`, auto-seeded from `demos/resources/internal_pool_router.json`, `base_url=http://localhost:11434` — the Ollama OpenAI-compat shim host, the LLM backend appends `/v1/chat/completions`; the buildable fallback, repoint at the Router-MVP `:13200` once up) whose `base_url` overlays the router endpoint. The model id `llama3.2:1b` (small/fast so the degenerate inference fits the test's 60s wall clock on a CPU-only box; `up-ollama` pulls it alongside the main model) is curated in `internal_pool_registry` (`model_registry`) and seeded `loaded` in `model_states` (`demos/model_states/internal_pool_llama.json`), so `GET /api/v1/models` reports it loaded. The compiled wire `provider` is `openai` (OpenAI-compatible router path); the editor surfaces this as provider `internal` with a loaded-set Model picker + locked base_url/api_key overrides (GDPR). |
| 54 | `54-streaming-echo/` | Workflow **as** a streaming endpoint (docs/25 §9 Phase 3): a `StreamSource` node ("feed") whose data Out channel is fed by an EXTERNAL client through the mekhan ingress endpoint (`POST /api/v1/instances/{id}/sources/feed/channels/feed/data`), a channel-triggered AutomatedStep ("shout") that uppercases each chunk onto its own "shouted" channel, and a `StreamSink` node ("out") terminating it at the egress endpoint (`GET /api/v1/instances/{id}/sinks/out/data`, `?follow=1` to live-tail). Source/Sink carry no control handles; Start has no outgoing edge (the StreamSource is the real entry point — a reachability root) and the step's completion token is the only End input, so the instance stays Running while bytes stream and completes when the external feed closes. |
| 59 | `59-firing-curve-bo/` | Real-physics BO campaign: demo 12's Loop+Map+gather topology where the objective is an actual OpenFOAM thermo-mechanical simulation (`solidDisplacementFoam`, zirconia sintering puck) run via `docker run opencfd/openfoam-default:2506` from a python-backend step. Searches (ramp, cool, hold) for the fastest firing cycle that neither cracks the puck nor under-sinters the core; every evaluation emits a firing-curve/stress PNG via `log_artifact` (media cards on the process Overview). `solver_mode=surrogate` runs the identical topology without Docker (closed-form objective calibrated against cluster-validated runs); `cluster/` carries the same case as an sbatch template for Slurm (RPTU Elwetritsch). |
| 60 | `60-closed-loop-firing/` | Flow-in-flow capstone: the whole simulate → review → physically-verify arc as ONE workflow. Embeds `59-firing-curve-bo` as a child (campaign + human review gate), a Python step joins the reviewer's per-curve verification picks (`verification_queue`, index-aligned with `top_curves` by the review Repeater) back to curve parameters, and a Decision dispatches the `40-sample-handling` robot child only when the reviewer approved, picked curves, AND the `robot` start field is `auto` (default `skip` so it completes without the ROS/Isaac fleet). Gotchas this demo pins: `go` is a Rhai reserved keyword (guard fields can't use it), and optional Start fields raise AttributeError in Python steps when omitted (guard literal read-arc anchors with try/except). |
| ★ | `invoice-processing/` | Capstone: trigger → human review → Python extract → decision → scope[split + join] → end. Exercises every editor node type plus direct slug access in Python. |
| ★ | `aithericon-loki-error-alert/` | Ops capstone: cron trigger → Loki `query_range` → Agent summary → SMTP email → end. Scheduled error-log alert against the Aithericon cluster Loki (`aithericon_loki` resource); applies from the CLI (no HumanTask sidecars). |
| ★ | `suessco-dev-loki-error-alert/` | Cross-cluster twin of the above: same chain, but the Loki step pins `deploymentModel.group = "suessco"` so it dispatches to a worker enrolled from the Suessco **dev** cluster, reading that cluster's in-cluster Loki. Level-based error filter; agent on `hf_inference`; `send_alert.maxRetries 0`. |
| ★ | `suessco-prod-loki-error-alert/` | Same as the dev twin but `deploymentModel.group = "suessco_prod"` → the Suessco **prod** cluster's worker + Loki. |

The seeder loads directories in lexical order, so `01-` … `06-` seed
before `invoice-processing/`. `06-subworkflow/` references
`01-hello-world/`'s templateId — alphabetical order guarantees the
child template is published before the parent resolves its
`sub_workflow` reference at publish time.

## Seeding

The service-side seeder publishes every demo at startup, idempotent
by `demo.json::templateId`. Toggled via env:

- `MEKHAN__DEMOS__SEED=true` — enable (default in `just dev::up-mekhan`)
- `MEKHAN__DEMOS__DIR=<path>` — override the search root (default `demos`)

A seeded demo whose templateId already exists on the server is left
alone — users can hand-edit through the web editor without the seeder
clobbering their changes.

## Folders

Every demo is filed into a category folder under the `/demos` root in
the default workspace. The category is declared in `demo.json`:

```json
{ "templateId": "…", "folder": "streaming", "name": "…", … }
```

`folder` is a path **relative to `/demos`**: a bare slug nests one level
(`"streaming"` → `/demos/streaming`); slashes nest deeper
(`"robotics/xarm"` → `/demos/robotics/xarm`). Absent ⇒ filed directly in
`/demos`. The curated categories (display name + blurb) live in
`DEMO_CATEGORIES` in `service/src/demos.rs`; an unknown slug still gets a
folder with a title-cased name. `private` demos are never filed (they're
hidden from the catalogue), so they omit `folder`. The unit test
`service::demos::tests::every_seeded_public_demo_declares_a_known_folder`
enforces that every public demo names a known category.

## Adding a demo

1. Drop a new directory under `demos/`.
2. Mint a stable templateId (UUID) — bake it into `demo.json` so
   re-seeding is idempotent and tests can refer to it. Set `folder` to
   the category it belongs in (see [Folders](#folders)).
3. Author `graph.json` either by hand or by exporting from a published
   template: `mekhan pull <template-id> demos/<new-name>/ --format json`.
4. Drop per-node sources into `nodes/<id>/`.
5. The Rust unit test `service::demos::tests::invoice_processing_demo_loads`
   is a template for writing a per-demo "this parses" smoke test.

## Editing a demo

The on-disk fixture is canonical. To edit:

- **Through the web editor**: load the seeded demo, edit, then
  `mekhan pull <templateId> demos/<name>/ --format json` to round-trip
  the change back to disk. The seeder will leave the now-edited row
  alone on next restart (idempotent), so re-seed via DB reset or by
  deleting the row first.
- **On disk**: edit `graph.json` / `nodes/<id>/main.py` directly. To
  publish: `mekhan apply demos/<name>/` (uses the same templateId so
  it bumps a new version), or wipe the existing row and let the
  seeder republish.
