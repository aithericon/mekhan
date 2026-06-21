# Aithericon Platform — Design & Architecture Docs

This directory is the platform's design archive: numbered design docs (roughly
chronological), a few how-to guides, and reference material. It began as SOP
migration planning (docs 01–03) and grew into the running design record for the
Petri-net engine, the capacity/allocation model, AI workloads, and the control
plane.

For build/run instructions start at the repo [`README.md`](../README.md) and
[`CLAUDE.md`](../CLAUDE.md); the engine and executor have their own
`CLAUDE.md` + `docs/` under `engine/` and `executor/`.

## Status conventions

Each doc carries a status near the top: **design** / **exploration** /
**implementation plan** / **implemented** / **partially realized** /
**superseded**. **Historical docs are never rewritten** — when a later doc
supersedes part of an earlier one, a banner at the top names what supersedes
which sections, and the prior text stays as design history. So a "design" doc
may describe something since built; trust the banner and the index status
column below over the body's tense.

The capacity model is the most-evolved thread:
[35](./35-allocation-and-traffic-planes.md) is the current spine;
[23](./23-unified-capacity-model.md)/[24](./24-capacity-unification-impl-plan.md)
are its history (35 §11 is the explicit supersession map).

## Numbered design docs

| Doc | Status | What it covers |
|-----|--------|----------------|
| [01 — legacy SOP requirements](./01-legacy-sop-requirements.md) | reference | Requirements capture of the legacy web-platform SOP system (templates/instances/phases/steps, batch controller, business rules, API surface). |
| [02 — migration strategy](./02-migration-strategy.md) | reference | Feature mapping, gap analysis, phased migration plan, risk assessment, open questions (§5). |
| [03 — MVP architecture](./03-mvp-architecture.md) | reference | First mekhan architecture after investigating petri-lab, human-ui, executor, and the legacy platform. |
| [04 — causality projector](./04-causality-projector.md) | implemented | The single JetStream consumer that projects net events into the causality/provenance tables. |
| [05 — typed ports](./05-typed-ports.md) | proposal | Typed input/output ports for the editor (parts superseded by 10). |
| [06 — triggers](./06-triggers.md) | proposal | Trigger nodes — workflow instantiation & signal sources. |
| [07 — runtime port enforcement](./07-runtime-port-enforcement.md) | proposal | Typed tokens enforced inside the net (parts superseded by 10). |
| [08 — multi-start nodes](./08-multi-start-nodes.md) | not implemented | Multiple Start nodes — blocked only at the compiler's start-count guard; wire/API/UI already plural-aware. |
| [09 — AI workload architecture](./09-ai-workload-architecture.md) | exploration | Decoupling model hosting from job processing, GPU serving pool, engine flow-control gaps for AI, LLM token streams as net semantics. |
| [10 — control/data token model](./10-control-data-token-model.md) | **implemented** | Control/data token split: write-once parked places, read-arc borrows (compiler-as-borrow-checker), producer-namespaced `<slug>.field` scope, `Data__*`/`Ctrl__*` schema enforcement, `/api/v1/analyze`. Supersedes parts of 05 & 07. |
| [11 — inference router](./11-inference-router.md) | design spec | Inference router service spec (sequel to the model-pool docs). |
| [12 — agent node](./12-agent-node-design.md) | design | Agent node subsuming single-shot LLM via two lowering paths; tools as tagged child nodes; S3 JSONL message log; replay safety. |
| [13 — schedulers as resources](./13-scheduler-as-resource-design.md) | partially realized | The datacenter connection layer — schedulers modeled as resources. |
| [14 — resource-pool net](./14-resource-pool-net-design.md) | implemented | Contended infrastructure on the Petri substrate (claim/grant/register/release). Carries the 2026-06-03 capacity-naming refactor banner. |
| [14 — loop carried-state](./14-loop-carried-state.md) | design | Loop carried-state lifecycle (carried-state envelope). |
| [15 — config form unification](./15-config-form-unification.md) | design | Collapsing the editor's three parallel node-config form systems into one. |
| [16 — multi-cluster scheduling](./16-multi-cluster-scheduling.md) | design → impl | Per-resource connections + the engine `ClusterRegistry`. |
| [17 — lease scope](./17-lease-scope.md) | design (slurm-lease shipped) | `LeaseScope` — decoupling "hold an allocation" from "loop". |
| [18 — streaming redesign](./18-streaming-redesign.md) | **superseded by 25** | Dissolving the `StreamConsumer` container. |
| [19 — shape as typed IR](./19-shape-as-typed-ir.md) | exploration | Shape as a typed affine IR for the compiler. |
| [20 — control-plane gaps](./20-control-plane-gaps.md) | implemented | Allocation visibility, cluster metrics, job-template management, staging (four phases built 2026-06-02). |
| [20 — resources & assets](./20-resources-and-assets.md) | design + initial impl | Hierarchical resource scoping/folders + a curated, schema-validated asset layer. |
| [21 — lab runner fleet](./21-lab-runner-fleet.md) | design (runner groups shipped) | Wiring physical lab stations into the platform via presence-driven admission nets. |
| [22 — container staging](./22-container-staging.md) | **built / live-proven** | Materialize OCI → Apptainer `.sif`, run the executor inside it on HPC Slurm. |
| [23 — unified capacity model](./23-unified-capacity-model.md) | design (partially superseded by 35) | One substrate for workers, instruments, HPC, LLMs, and humans: axes table, eligibility spectrum (§4), the §6 keystone. |
| [24 — capacity unification impl plan](./24-capacity-unification-impl-plan.md) | impl plan (superseded by 35 §1–§2) | First buildable slice of 23 (telemetry & model planes). |
| [25 — streaming channels](./25-streaming-channels.md) | implemented (with gaps) | One emission primitive, control/data split, pluggable transport. Supersedes 18. |
| [26 — motion planning](./26-motion-planning.md) | design only | MoveIt motion-planning integration — design dialogue. |
| [27 — motion-planning impl plan](./27-motion-planning-impl-plan.md) | build spec | MoveIt build spec (Path C); design rationale lives in 26. |
| [28 — model pool control plane](./28-model-pool-control-plane.md) | design spec | Model pool control plane (companion to 11). |
| [29 — model pool impl plan](./29-model-pool-impl-plan.md) | impl plan | File-level executable plan for the model-pool control plane. |
| [30 — autoscaler load/unload gap](./30-autoscaler-load-unload-gap.md) | gap analysis | Model-pool autoscaler ↔ load/unload reconciliation gap. |
| [30 — finalizer transitions](./30-finalizer-transitions.md) | **implemented** | `Transition.finalizer` flag + `drain_finalizers` hook that releases a held resource even when the net fails. Builds on 14 & 17. |
| [31 — model pool reconciliation impl plan](./31-model-pool-reconciliation-impl-plan.md) | impl plan | Autoscaler ↔ node-fleet + placement reconciliation. |
| [32 — legacy file migration](./32-legacy-file-migration.md) | impl prep | Cataloging ~3.96M legacy files (~76 TB across 4 NAS servers). |
| [33 — human capacity roster](./33-human-capacity-roster.md) | design | Humans as a capacity — the `offer` dispatch, the roster, capability-matched self-claim. |
| [34 — human-task offer wiring](./34-human-task-offer-wiring.md) | build spec | Wiring `HumanTask` through the offer handshake: offered → claimed → completed, engine-authoritative, inbox as projection. |
| [35 — allocation & traffic planes](./35-allocation-and-traffic-planes.md) | **design — current spine** | The consolidated capacity model; partially supersedes 23/24 (§11 supersession map). |
| [36 — output data plane](./36-output-data-plane.md) | implemented (Phase 1) | `set_output` never inlines bytes; `log_artifact` owns the warehouse. |
| [37 — runner zero-secret enrollment](./37-runner-zero-secret-enrollment.md) | design + phased build | Single-origin broker so an enrolled runner needs only an enrollment token + URL. |

## Guides & reference

| Doc | What it covers |
|-----|----------------|
| [setup.md](./setup.md) | Per-OS native build deps (HDF5/NetCDF/protoc/cmake, …) and toolchain prerequisites. |
| [adding-a-new-backend.md](./adding-a-new-backend.md) | End-to-end recipe for adding a new executor backend. |
| [authoring-constants-and-seeds.md](./authoring-constants-and-seeds.md) | Authoring constants & seed values when porting a hand-coded net (no compiler feature needed). |

## Subdirectories

- [`platform-topology/`](./platform-topology/) — reference map of the running system: NATS subjects/streams/consumers/KV and the Postgres data model. Start at [`platform-topology/index.md`](./platform-topology/index.md).
- [`plans/`](./plans/) — multi-doc implementation plans kept as design history (e.g. [`plans/iam-granular/`](./plans/iam-granular/), [`plans/library-nodes.md`](./plans/library-nodes.md)).
- [`refactor/`](./refactor/) — refactor audits and post-mortems.
- [`releases/`](./releases/) — release notes ([`v0.1.0-alpha`](./releases/v0.1.0-alpha.md)).

## Key concepts

### Engine & services
- **petri-lab / core-engine** — Petri-net workflow engine (event-sourced, NATS-streamed).
- **mekhan-service** — BFF + control plane (compiler, templates/instances/triggers, Yjs collaboration).
- **aithericon-executor** — the long-running job worker (Python / Docker / HTTP backends).

### Control/data token model (doc 10)
Each node's business output is **parked write-once** in a `p_{id}_data` place; only a slim **control token** moves by-value through the net. Guards/loops/End mappings that need an upstream field get a non-consuming **read-arc**. References are producer-namespaced `<slug>.<field>`. This is what the service-side compiler enforces — it is the platform's borrow-checker.

### Plane vocabulary (doc 35)
The **allocation plane** decides who may hold a capacity — identity, enrollment, capability advertisement, liveness, eligibility, grant/hold/release, provenance. The **traffic plane** is the bytes of work flowing to an allocated capacity (inference requests, NATS jobs, file records, human-task interaction), never engine-mediated; the seam between them is the grant's dispatch address (`executor_namespace`) — see [35](./35-allocation-and-traffic-planes.md) §1–§2.

### Legacy SOP lineage (docs 01–02)
- **SOP Template → Phase Template → Step Template** (3-tier hierarchy); **SOP Instance → Phase Instance → Step Instance** at runtime; **Batch Controller** groups instances for parallel phase processing. See [02 §5](./02-migration-strategy.md#5-open-questions) for the open migration questions.
