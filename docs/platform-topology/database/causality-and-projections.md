---
type: Database Domain
title: Causality & Projections
description: The event-sourced read models fed from NATS — causality graph, human-process instances (HPI), and analytics rollups. All deliberately FK-free.
tags: [database, causality, projections, hpi, rollups, analytics]
timestamp: 2026-06-18T00:00:00Z
---

# Causality & Projections

These tables are **projections**: they are rebuilt from the engine's NATS event
stream by mekhan's [projection consumers](/platform-topology/consumers/mekhan-projections.md),
not authored directly. They carry no foreign keys to the rest of the schema on
purpose — they ingest by `net_id` / `process_id` / `signal_key` strings so an
out-of-order or replayed event never violates a constraint.

# Schema — causality graph

Tracks how tokens, events, and cross-net signals relate, for the causality
explorer.

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `causality_events` | (`net_id`, `event_seq`), `event_type`, `transition_name`, `effect_handler`, `effect_result` (jsonb), `bridge_target_net`/`bridge_target_place` | Per-net event log (the projection of `petri.{ws}.{net}.events.>`). |
| `causality_event_tokens` | (`net_id`, `event_seq`) → `causality_events`, `token_id`, `role`, `place_id`, `token_data` (jsonb) | Tokens produced/consumed by each event. The **only** FK in this domain (composite, to its own events table). |
| `causality_process_tags` | (`token_id`, `process_id`) | Associates tokens with a logical process. |
| `causality_signal_dispatches` | (`signal_key`, `dispatch_net`, `dispatch_seq`) | Records a signal being emitted. |
| `causality_signal_lineage` | (`ingress_net`, `ingress_seq`, `dispatch_net`, `dispatch_seq`, `signal_key`) | Links a received signal back to its dispatch. |
| `causality_cross_links` | `signal_key`, `egress_net`/`egress_seq`, `ingress_net`/`ingress_seq`, `link_type` | Cross-net edges (bridges/signals) for the graph view. |

# Schema — human-process instances (HPI)

The human-facing process / task model, keyed by `process_id`.

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `hpi_processes` | `process_id`, `name`, `kind`, `status`, `config` (jsonb), `instance_id`, `net_id` | A human process; links to a [workflow instance](instances-and-execution.md) by id (no FK). |
| `hpi_tasks` | `id`, `process_id` → `hpi_processes.process_id`, `title`, `status`, `assignee`, `workspace_id`, `claimed_at`, `completed_at`, `detail` (jsonb) | A human task (the relational side of the [human-task NATS protocol](/platform-topology/subjects/human.md)). |
| `hpi_logs` | `id`, `process_id`, `level`, `source`, `message`, `detail` (jsonb), `signal_key` | Process log stream. |
| `hpi_metrics` | (`process_id`, `key`), `value`, `timestamp`, `signal_key` | Numeric process metrics time series. |

# Schema — analytics rollups

Pre-aggregated counters for template dashboards.

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `template_run_rollup` | (`template_id`, `template_version`, `bucket_hour`, `mode`, `outcome`), `run_count`, `duration_ms_sum`/`_count` | Hourly run counts & durations per template. |
| `template_node_rollup` | (`template_id`, `template_version`, `node_id`, `status`), `count`, `duration_ms_sum` | Per-node execution aggregates. |
| `template_user_runs` | (`template_id`, `user_id`), `run_count`, `first_run`, `last_run` | Who ran a template, how often. |

# Notes

- `hpi_tasks` is the only HPI table with a `workspace_id` (added in
  `20240157` for the workspace task offer/inbox).
- Rollups are maintained by `20240175_template_analytics_rollups.sql` triggers /
  projection writers, not by the application on the hot path.

# Citations

[1] `service/migrations/20240106_create_causality_tables.sql`,
    `20240108`–`20240112` (rekey / signal lineage / payloads),
    `20240105_create_hpi_tables.sql`,
    `20240107_rekey_hpi_to_process_id.sql`,
    `20240157_hpi_tasks_workspace_offer.sql`,
    `20240175_template_analytics_rollups.sql`.
[2] `service/src/projections/`, `service/src/causality/`.
