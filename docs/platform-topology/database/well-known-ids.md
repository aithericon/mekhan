---
type: Reference
title: Well-Known IDs & Scopes
description: Sentinel workspace/scope UUIDs, the workspace-vs-platform scope axis, and the object_kind enum used across the schema.
tags: [database, multi-tenancy, scopes, workspaces, enums]
timestamp: 2026-06-18T00:00:00Z
---

# Well-Known IDs & Scopes

Several rows and IDs are fixed sentinels seeded by migrations. They are
human-readable hex: `de` = demos, `506c6174` = ASCII `"Plat"`.

# Sentinel workspaces

These are the rows that exist in `workspaces` on a fresh slot-0 boot.

| UUID | slug | `is_system` | Role |
|------|------|-------------|------|
| `00000000-0000-0000-0000-000000000000` | `default` | yes | The nil-UUID default workspace; the fallback tenant before ADR-09 multi-tenancy. Demoted from "everyone joins" in migration `20240189`. |
| `00000000-0000-0000-0000-0000000000de` | `demos` | yes | Read-only system workspace holding the seeded demo templates; visitable, fork-from to your own workspace. |
| `00000000-0000-0000-0000-0000506c6174` | `platform` | yes | The **platform scope** anchor (`ScopeKind::Platform`). NOT a normal tenant — shared infra (LLM model pool, default worker/runner groups) lives here, globally readable/runnable. |
| `00000000-0000-0000-0000-000000000001` | `dev-user` | no | Dev-only personal workspace (dev_noop fixture). |
| `00000000-0000-0000-0000-000000000002` | `acme-labs` | no | Dev-only second org for testing tenant isolation (migration `20240184`). |

Many projection tables default `workspace_id` to the nil UUID
(`'00000000-0000-0000-0000-000000000000'`) so pre-ADR-09 rows and untenanted
writes land in `default` rather than NULL.

# The scope axis

Tenancy is a two-axis `(scope_kind, scope_id)` pair on objects that can live
either inside a workspace or in shared platform space:

| `scope_kind` | `scope_id` points at | Visibility |
|--------------|----------------------|------------|
| `workspace` | a `workspaces.id` | members of that workspace |
| `platform` | the platform anchor `…506c6174` | every workspace (read + run), curated by platform admins |

`resources`, `assets`, and `asset_types` carry an explicit
`(scope_kind, scope_id)`. Most other tables carry a bare `workspace_id` and are
workspace-scoped only.

# The `object_kind` enum

A single Postgres enum names the object types that can be shared via
[object grants & invites](identity-and-tenancy.md):

```
object_kind = folder | template | instance | resource | asset
```

Used by `object_grants.object_type` and (as text) by
`invite_object_grants.object_type`.

# Citations

[1] `service/migrations/20240188_platform_shared_infra.sql`,
    `20240189_demote_default_workspace.sql`,
    `20240190_platform_scope_anchor_workspace.sql` — platform scope.
[2] `service/migrations/20240184_seed_dev_user_two.sql` — dev second org.
[3] `service/migrations/20240171_object_grants.sql` — `object_kind` enum.
[4] Live: `SELECT id, slug, is_system FROM workspaces;` on slot-0, 2026-06-18.
