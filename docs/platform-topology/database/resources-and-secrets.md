---
type: Database Domain
title: Resources & Secrets
description: Resource definitions, versioned Vault-backed configuration, per-resource ACLs, and the usage audit trail.
tags: [database, resources, secrets, vault, acl, audit]
timestamp: 2026-06-18T00:00:00Z
---

# Resources & Secrets

A **resource** is a named, versioned configuration object — a datacenter, a
container registry, a connected credential, an LLM provider, etc. The
**non-secret** config lives in Postgres (`resource_versions.public_config`); the
**secret** payload lives in Vault, and Postgres only stores the `vault_path`
pointer. Resources carry the [`(scope_kind, scope_id)`](well-known-ids.md) axis.

# Schema

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `resources` | `id`, `workspace_id` → `workspaces.id`, `path`, `resource_type`, `display_name`, `latest_version`, `scope_kind` (`workspace` default) / `scope_id`, `restricted`, `deleted_at` | The resource definition. `path` is the addressable key; `resource_type` selects the schema/adapter. |
| `resource_versions` | (`resource_id` → `resources.id`, `version`), `vault_path`, `public_config` (jsonb), `created_by` | One immutable version. `vault_path` points at `secret/data/aithericon/resources/{ws}/{resource}/v{n}`; **only the pointer is in Postgres**. |
| `resource_acl` | (`resource_id` → `resources.id`, `principal_id`, `principal_kind`), `permission`, `granted_by` | Per-resource access grants (the `resource` arm of object sharing). |
| `resource_audit` | `id` (seq), `resource_id` → `resources.id`, `resource_version`, `instance_id`, `step_id`, `principal_id`, `action`, `site`, `occurred_at` | Append-only audit of resource use (which instance/step consumed which version, where). |

# Notes

- Dev Vault is in-memory and ephemeral: the `resources` / `resource_versions`
  rows survive a `just dev down`, but `vault_path` then points at empty entries
  until the secret is rewritten.
- Resource CRUD flow: mekhan writes the secret to Vault via `VaultResourceStore`,
  the engine wraps it into a single-use token at job submit, and the executor
  unwraps it — none of those secret bytes ever land in this database.
- `workspaces.default_datacenter_resource_id` and several fleet tables reference
  a datacenter [resource](fleet-and-compute.md).

# Citations

[1] `service/migrations/20240120_create_resources.sql`,
    `20240121_resource_audit.sql`,
    `20240122_resource_oauth_tokens.sql`,
    `20240126_backfill_resources_workspace.sql`,
    `20240136_generalize_resource_scope.sql`,
    `20240170_audit_provenance.sql`,
    `20240174_resource_asset_acl.sql`.
[2] `service/src/resources/`, `VaultResourceStore`.
