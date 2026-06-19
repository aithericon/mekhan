---
type: NATS Subject Family
title: Human-Task Subjects
description: The human.{ws}.… request/result protocol between engine and the UI, including the 5-part ADR-09 layout.
tags: [nats, subjects, human-tasks, multi-tenancy, adr-09]
timestamp: 2026-06-18T00:00:00Z
---

# Human-Task Subjects

A bidirectional protocol under the `human.` root (off the `petri.>` tree). Defined
in `engine/core-engine/crates/api-types/src/subjects.rs`; carried on the
[human-task streams](/platform-topology/streams/human-task-streams.md).

# Schema

| Subject | Direction | Meaning |
|---------|-----------|---------|
| `human.{ws}.request.{net}.{place}` | engine → UI | task request published |
| `human.{ws}.cancel.{net}.{place}` | engine → UI | cancellation request |
| `human.{ws}.completed.{net}.{place}` | UI → engine | task completed |
| `human.{ws}.cancelled.{net}.{place}` | UI → engine | cancellation confirmed |
| `human.{ws}.failed.{net}.{place}` | UI → engine | task failed |

Filters wildcard the tenant token, e.g. `human.*.request.>`,
`human.*.completed.>`.

# Note

These are 5-segment subjects (`human.{ws}.{kind}.{net}.{place}`). The ADR-09
multi-tenancy migration that inserted `{ws}` was the source of a "stuck-running"
bug where service still published 4-part subjects after the engine streams had
moved to 5-part — fixed by adding `Subjects::human_{completed,cancelled}` builders
and matching publish paths.

Human-capacity presence rides a parallel subject, `human.{capacity_id}.{member}.presence`
— see [fleet & presence](/platform-topology/subjects/fleet-presence.md).

# Citations

[1] `engine/core-engine/crates/api-types/src/subjects.rs`.
[2] `service/src/nats/subjects.rs`.
