# Mekhan - SOP Migration Planning

Migration planning from the legacy web-platform SOP system to the new petri-lab / human-ui / aithericon-executor framework.

## Documents

| File | Description |
|------|-------------|
| [01-legacy-sop-requirements.md](./01-legacy-sop-requirements.md) | Comprehensive requirements capture of the legacy SOP system (templates, instances, phases, steps, batch controller, business rules, API surface) |
| [02-migration-strategy.md](./02-migration-strategy.md) | Unified migration strategy with feature mapping, gap analysis, phased migration plan, risk assessment, and open questions |
| [08-multi-start-nodes.md](./08-multi-start-nodes.md) | Design handoff: enabling multiple Start nodes — blocked only by the compiler's start-count guard / single-root analysis; wire, instance API, and UI are already plural-Start-aware |

## Key Concepts

### Legacy System (web-platform)
- **SOP Template** -> Phase Template -> Step Template (3-tier hierarchy)
- **SOP Instance** -> Phase Instance -> Step Instance (runtime)
- **Batch Controller** groups instances for parallel phase processing

### New Framework
- **petri-lab** - Petri net workflow engine (event-sourced, NATS-streamed)
- **human-ui** - SvelteKit operator interface for human task execution
- **aithericon-executor** - Backend execution engine for automated steps

### Migration Phases
0. Foundation (reference SOP scenario + token schemas)
1. Core SOP Execution (deploy service, missing UI blocks, PoC)
2. Batch/Campaign Support (campaign nets, campaign UI)
3. Template Management (registry, admin tooling, data migration)
4. Full Migration (parallel run, decommission legacy)

## Open Questions
See [02-migration-strategy.md](./02-migration-strategy.md#5-open-questions) for 7 open questions that need stakeholder input.
