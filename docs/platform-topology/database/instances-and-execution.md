---
type: Database Domain
title: Instances & Execution
description: Workflow instances (the running net) and per-node step-execution records, including the sub-workflow hierarchy and test-mode runs.
tags: [database, instances, execution, sub-workflows, steps]
timestamp: 2026-06-18T00:00:00Z
---

# Instances & Execution

A **workflow instance** is one running Petri net, deployed from a template
version. The engine owns the live event stream over
[NATS](/platform-topology/streams/petri-global.md); mekhan keeps the relational
control-plane view here.

# Schema

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `workflow_instances` | `id`, `template_id` → `workflow_templates.id`, `template_version`, `net_id`, `status`, `mode`, `current_step`, `result` (jsonb), `metadata` (jsonb), `resource_pins` (jsonb), `asset_pins` (jsonb), `graph_snapshot` (jsonb), `interface_snapshot` (jsonb) | The instance. `net_id` is the engine's globally-unique net handle. `status` ∈ `created` / `running` / completed / failed / archived. |
| `step_execution` | (`instance_id` → `workflow_instances.id`, `node_id`, `iteration_index`), `template_id`, `template_version`, `node_kind`, `status`, `inputs`/`outputs`/`error` (jsonb), `execution_id`, `branch_taken`, `last_sequence`, `started_at`/`completed_at` | Per-node execution record; `iteration_index` distinguishes loop iterations. `last_sequence` tracks the highest NATS event applied (idempotent projection). |

## Sub-workflow hierarchy

`workflow_instances` carries a self-referential tree so a parent net that spawns
sub-workflows can be navigated:

| Column | → | Meaning |
|--------|---|---------|
| `parent_instance_id` | `workflow_instances.id` | immediate parent that spawned this instance |
| `parent_node_id` | — | the node in the parent that spawned it |
| `root_instance_id` | `workflow_instances.id` | top of the spawn tree |
| `spawn_seq` | — | ordering of spawns under the parent |
| `source_instance_id` | `workflow_instances.id` | the instance this was cloned/re-run from |

## Test & snapshot fields

- `mode` (`live` default / test) and `test_id` → [`template_tests`](templates-and-authoring.md)
  mark a run as a [template test run](templates-and-authoring.md).
- `graph_snapshot` / `interface_snapshot` freeze the template's graph and
  interface at launch so the instance renders correctly even after the template
  is edited (migration `20240185`).
- `resource_pins` / `asset_pins` freeze which [resource](resources-and-secrets.md)
  and [asset](assets.md) versions the run binds to.

# Notes

- `step_execution` is a **projection** fed from the engine's
  [executor status/events](/platform-topology/consumers/result-ingest.md) — it is
  reconstructable from NATS and carries `last_sequence` for at-least-once replay.
- Deeper execution observability (logs, metrics, causality) lives in the
  [causality & projections](causality-and-projections.md) domain.

# Citations

[1] `service/migrations/20240101_initial_schema.sql`,
    `20240115_add_instance_result.sql`,
    `20240117_step_execution.sql`,
    `20240130_subworkflow_instance_hierarchy.sql`,
    `20240185_instance_graph_snapshot.sql`.
[2] `service/src/handlers/instances.rs`, `service/src/projections/`.
