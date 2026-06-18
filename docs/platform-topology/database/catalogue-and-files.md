---
type: Database Domain
title: Catalogue & Files
description: The data catalogue (run outputs as queryable entries), data-type registry, file inventory across file servers, snapshots, and legacy migration tables.
tags: [database, catalogue, files, inventory, data-types, snapshots]
timestamp: 2026-06-18T00:00:00Z
---

# Catalogue & Files

Two related concerns: the **catalogue** (workflow run outputs registered as
queryable, typed entries) and the **file inventory** (physical files crawled
across file servers, deduplicated by content hash). The catalogue is fed from
the [`catalogue.` NATS subjects](/platform-topology/subjects/catalogue.md); the
inventory is fed from the [inventory fold stream](/platform-topology/streams/inventory-fold.md).

All tables here default `workspace_id` to the nil UUID and are workspace-scoped
(migrations `20240177`–`20240181` added the ws column for tenant isolation).

# Schema — catalogue

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `catalogue_entries` | `entry_id` (uuid pk), `execution_id`, `job_id`, `name`, `category`, `filename`, `mime_type`, `size_bytes`, `storage_path`, `content_hash`, `source_net`/`source_place`, `signal_key`, `process_id`, `file_metadata`/`user_metadata` (jsonb), `workspace_id` | A registered run output. `user_metadata` is queryable via the `umeta.<key>` catalogue DSL. |
| `catalogue_producers` | `content_hash`, `execution_id`, `source_net`, `job_id`, `process_id`, `source_event_sequence`, `workspace_id` | Provenance: which execution produced a given content hash. |
| `catalogue_data_types` | `id`, `name`, `description`, `columns` (jsonb), `workspace_id` | Registered structured data-type schemas for catalogue entries. |
| `catalogue_data_type_digests` | `digest`, `data_type_id` → `catalogue_data_types.id`, `workspace_id` | Content digest → data-type binding (auto-typing). |
| `catalogue_saved_queries` | `id`, `name`, `q` (DSL text), `params` (jsonb), `workspace_id` | Saved catalogue DSL queries. |

# Schema — file inventory & servers

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `file_servers` | `id`, `workspace_id` → (logical), `key`, `display_name`, `status`, `config` (jsonb) | A registered file server (NAS / S3 / node). |
| `file_server_endpoints` | `id`, `file_server_id` → `file_servers.id`, `access_method`, `root`, `resource_ref`, `status`, `verification_status`, `priority`, `config` (jsonb) | How to reach a file server (multiple access methods per server). |
| `file_inventory` | `id`, `content_hash`, `file_server_id`, `path`, `status`, `is_canonical`, `copy_of` (self FK), `migration_target`, `size_bytes`, `mtime`, `provenance` (jsonb), `workspace_id` | One crawled file. `copy_of` links duplicates to the canonical copy. |
| `inventory_snapshots` | (`snapped_at`, `file_server_id`, `dim`, `key`, `workspace_id`), `file_count`, `total_bytes` | Periodic per-dimension size/count snapshots for analytics, partitioned per `(workspace, server)`. |

# Schema — legacy migration

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `legacy_file_index` | `legacy_key`, `file_server_id`, `path`, `hash`, `size`, `node_id`, `owner_id`, `raw` (jsonb) | Index of files from the pre-platform storage layout, for migration. |
| `legacy_delete_queue` | `key`, `hash`, `size`, `modified` | Legacy keys queued for deletion after migration. |

# Views

Three reconcile views compare catalogue vs. inventory to surface drift:

| View | Purpose |
|------|---------|
| `reconcile_duplicates` | content hashes with more than one inventory row |
| `reconcile_orphan_db` | catalogue entries with no matching inventory file |
| `reconcile_summary` | aggregate reconcile counts |

# Notes

- `catalogue_entries.id` (text) is legacy; `entry_id` (uuid) is the real PK.
- The catalogue trigger/subscription filtering converged onto a single query DSL
  compiler — see `service/src/catalogue/`.

# Citations

[1] `service/migrations/20240104_create_catalogue.sql`,
    `20240153_catalog_content_addressed.sql`,
    `20240161_catalogue_producers.sql`,
    `20240168_catalogue_query.sql`,
    `20240173_catalogue_data_types.sql`,
    `20240177`/`20240178`/`20240181` (workspace columns).
[2] `service/migrations/20240160_file_servers.sql`,
    `20240164_file_server_endpoints.sql`,
    `20240166_file_inventory_analytics.sql`,
    `20240167_inventory_snapshots.sql`,
    `20240154_reconcile_views.sql`,
    `20240179`/`20240180` (workspace columns).
[3] `service/src/catalogue/`, `service/src/handlers/data.rs`.
