---
type: Database Domain
title: Entity Relationships
description: The foreign-key graph tying the database domains together, and the deliberate split between constrained control-plane tables and FK-free NATS projections.
tags: [database, relationships, foreign-keys, erd, projections]
timestamp: 2026-06-18T00:00:00Z
---

# Entity Relationships

The schema has two halves with very different integrity philosophies:

1. **Control-plane tables** (identity, templates, instances, resources, fleet,
   assets) are normalized with real foreign keys and cascade rules.
2. **Projection tables** (causality, HPI, catalogue, rollups) are fed
   asynchronously from [NATS](/platform-topology/overview.md) and carry **almost no
   foreign keys** — they ingest by string keys (`net_id`, `process_id`,
   `signal_key`, `content_hash`) so replayed or out-of-order events never trip a
   constraint.

# `workspaces` is the tenancy root

Everything tenant-scoped points (directly or transitively) at `workspaces.id`.
Direct FK children:

```
workspaces ─┬─ workspace_members
            ├─ folders ──┬─ template_folders
            │            └─ pages
            ├─ resources ─┬─ resource_versions
            │             ├─ resource_acl
            │             └─ resource_audit
            ├─ workflow_templates ── webhook_slugs
            ├─ library_packs        (← workflow_templates.pack_id)
            ├─ object_grants
            ├─ pending_invites ── invite_object_grants
            └─ template_tags
workspaces.default_datacenter_resource_id → resources.id   (back-reference)
```

# Templates, instances, tests

```
workflow_templates ──< workflow_instances >── template_tests
        │  ▲ (base_template_id / parent_id self-FK)        │
        │                                                  │
        └─ webhook_slugs                    template_test_runs ─→ template_tests
                                                  ▲
workflow_instances ──< step_execution            │
workflow_instances ── (parent / root / source self-FK)  ── sub-workflow tree
workflow_instances.test_id ───────────────────────────────┘
```

Note `template_stagings.template_id` → **`job_templates`** (the scheduler job
spec), not `workflow_templates` — a common point of confusion.

# Assets & job templates

```
asset_types ──< assets ──< asset_records
job_templates ──< job_template_versions
job_templates ──< template_stagings
runners ──< runner_interfaces
file_servers ──< file_server_endpoints
file_inventory ── copy_of (self-FK to canonical copy)
catalogue_data_types ──< catalogue_data_type_digests
```

# Projection island (no outward FKs)

These tables reference instances/nets/processes by id-as-string only:

- **Causality**: `causality_events` ──< `causality_event_tokens` (the lone
  composite FK in the island); `causality_signal_*`, `causality_cross_links`,
  `causality_process_tags` stand alone.
- **HPI**: `hpi_processes` ──< `hpi_tasks` (FK on `process_id`); `hpi_logs`,
  `hpi_metrics` keyed by `process_id` with no FK.
- **Catalogue / inventory**: `catalogue_entries`, `catalogue_producers`,
  `inventory_snapshots` — joined by `content_hash` / `execution_id` / `workspace_id`,
  not constraints.
- **Rollups**: `template_run_rollup`, `template_node_rollup`, `template_user_runs`
  — aggregates keyed by `template_id` with no FK.

# Auth & collaboration islands

`auth_sessions`, `auth_login_flows`, `oauth_state`, `user_profiles`,
`yjs_documents`, `yjs_snapshots` are standalone — keyed by OIDC subject / cookie
id / `doc_id`, with no FK into the tenant graph.

# Citations

[1] Live: `information_schema.table_constraints` (FOREIGN KEY) on slot-0, 2026-06-18.
[2] Per-domain docs in this directory.
[3] ADR-09 multi-tenancy: `docs/09-ai-workload-architecture.md`.
