---
type: Database Domain
title: Assets
description: Typed structured-data assets — user-defined asset types, asset instances, and their versioned tabular records.
tags: [database, assets, structured-data, scopes]
timestamp: 2026-06-18T00:00:00Z
---

# Assets

Assets are user-defined typed structured data — a lightweight tabular store that
workflows can read and write. An **asset type** defines the schema (fields and
cardinality); an **asset** is an instance of a type; **asset records** are its
rows. Assets carry the two-axis [`(scope_kind, scope_id)`](well-known-ids.md) so
they can be workspace- or platform-scoped.

# Schema

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `asset_types` | `id`, `scope_kind`/`scope_id`, `name`, `display_name`, `display_path`, `fields_json` (jsonb), `cardinality` (`collection` default / single), `version`, `deleted_at` | The schema of an asset: its fields and whether it holds one record or a collection. |
| `assets` | `id`, `scope_kind`/`scope_id`, `type_id` → `asset_types.id`, `ref_key`, `display_name`, `display_path`, `version`, `restricted`, `deleted_at` | An asset instance of a given type. `ref_key` is the stable handle workflows pin to. |
| `asset_records` | (`asset_id` → `assets.id`, `version`, `row_idx`), `data` (jsonb) | The rows. Versioned per asset; `row_idx` orders rows within a version. |

# Notes

- `restricted` on `assets` (and the `resource_acl`-style flag elsewhere) gates
  visibility within the scope.
- Workflow instances freeze which asset versions they bind to via
  [`workflow_instances.asset_pins`](instances-and-execution.md).
- Asset sharing is one of the [`object_kind`](well-known-ids.md) grant types.

# Citations

[1] `service/migrations/20240137_create_assets.sql`,
    `20240138_asset_pins_usage_index.sql`,
    `20240150_asset_scope_folder.sql`,
    `20240174_resource_asset_acl.sql`.
[2] `service/src/handlers/assets.rs`.
