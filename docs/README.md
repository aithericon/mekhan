# Mekhan - SOP Migration Planning

Migration planning from the legacy web-platform SOP system to the new petri-lab / human-ui / aithericon-executor framework.

## Documents

| File | Description |
|------|-------------|
| [01-legacy-sop-requirements.md](./01-legacy-sop-requirements.md) | Comprehensive requirements capture of the legacy SOP system (templates, instances, phases, steps, batch controller, business rules, API surface) |
| [02-migration-strategy.md](./02-migration-strategy.md) | Unified migration strategy with feature mapping, gap analysis, phased migration plan, risk assessment, and open questions |
| [08-multi-start-nodes.md](./08-multi-start-nodes.md) | Design handoff: enabling multiple Start nodes — blocked only by the compiler's start-count guard / single-root analysis; wire, instance API, and UI are already plural-Start-aware |
| [09-ai-workload-architecture.md](./09-ai-workload-architecture.md) | Design exploration: decoupling AI model hosting from job processing, GPU serving pool (Ollama base), engine toolbox audit for AI workloads (4 flow-control gaps), and how LLM token streams map to net semantics |
| [10-control-data-token-model.md](./10-control-data-token-model.md) | **Implemented (on `main`).** Control/data token split: write-once parked data places, read-arc borrows (compiler-as-borrow-checker), producer-namespaced `<slug>.field` scope, runtime `Data__*`/`Ctrl__*` schema enforcement, editor `/api/v1/analyze` surface. Supersedes parts of 05 & 07 |
| [12-agent-node-design.md](./12-agent-node-design.md) | **Design (pre-implementation).** Agent node that subsumes single-shot LLM via two lowering paths (degenerate == today's Llm AutomatedStep, byte-identical); tools as tagged child nodes (SubWorkflow / Agent as tools compose for free); slim parked state + S3 JSONL message log; serial tools v1; replay safety via engine effect-event journaling; equivalence-test contract pinned |
| [23-unified-capacity-model.md](./23-unified-capacity-model.md) | Unified capacity model — one substrate for workers, instruments, HPC, LLMs, and humans: the axes table, the eligibility-strategy spectrum (§4), the §6 keystone. Partially superseded by 35 (see 35 §11) |
| [24-capacity-unification-impl-plan.md](./24-capacity-unification-impl-plan.md) | Implementation plan for the first buildable slice of 23 (telemetry & model planes); its §1 three-plane decomposition is superseded by 35 §1–§2 |
| [30-finalizer-transitions.md](./30-finalizer-transitions.md) | **Implemented.** Finalizer transitions: a `Transition.finalizer` flag + `SelectPhase`/`drain_finalizers` engine hook that fires a transition ONLY on permanent-failure teardown (before `NetFailed`), so a held resource is released even when the net fails. First consumer = the lease bridge's `t_<id>_finally` (releases a stranded runner/alloc on failure, event-sourced → survives restart). Builds on 14 & 17 |
| [33-human-capacity-roster.md](./33-human-capacity-roster.md) | Humans as a capacity — the `offer` dispatch, the roster, and capability-matched self-claim (its `Dispatch::Offer` is reclassified as `Acceptance::consent` by 35 §4) |
| [34-human-task-offer-wiring.md](./34-human-task-offer-wiring.md) | Build spec wiring `HumanTask` through the offer handshake: offered → member claims → completes, engine-authoritative, with the inbox a pure projection |
| [35-allocation-and-traffic-planes.md](./35-allocation-and-traffic-planes.md) | **Current spine — allocation/traffic planes, acceptance axis, hold-only engine; partially supersedes 23/24** (explicit supersession map in its §11) |

## Status conventions

Docs carry a status: **design** / **implementation plan** / **implemented** / **partially realized**. Historical docs are never rewritten — when a later doc supersedes part of an earlier one, a banner at the top names what supersedes which sections, and the prior text stays as design history. [35-allocation-and-traffic-planes.md](./35-allocation-and-traffic-planes.md) is the current spine for the capacity model; docs [23](./23-unified-capacity-model.md)/[24](./24-capacity-unification-impl-plan.md) are its history (35 §11 is the explicit supersession map).

## Key Concepts

### Legacy System (web-platform)
- **SOP Template** -> Phase Template -> Step Template (3-tier hierarchy)
- **SOP Instance** -> Phase Instance -> Step Instance (runtime)
- **Batch Controller** groups instances for parallel phase processing

### New Framework
- **petri-lab** - Petri net workflow engine (event-sourced, NATS-streamed)
- **human-ui** - SvelteKit operator interface for human task execution
- **aithericon-executor** - Backend execution engine for automated steps

### Plane vocabulary
The **allocation plane** decides who may hold a capacity — identity, enrollment, capability advertisement, liveness, eligibility, grant/hold/release, provenance. The **traffic plane** is the bytes of work flowing to an allocated capacity (inference requests, NATS jobs, file records, human task interaction), never engine-mediated; the seam between them is the grant's dispatch address (`executor_namespace`) — see [35-allocation-and-traffic-planes.md](./35-allocation-and-traffic-planes.md) §1–§2.

### Migration Phases
0. Foundation (reference SOP scenario + token schemas)
1. Core SOP Execution (deploy service, missing UI blocks, PoC)
2. Batch/Campaign Support (campaign nets, campaign UI)
3. Template Management (registry, admin tooling, data migration)
4. Full Migration (parallel run, decommission legacy)

## Open Questions
See [02-migration-strategy.md](./02-migration-strategy.md#5-open-questions) for 7 open questions that need stakeholder input.
