---
type: Database Domain
title: Identity & Tenancy
description: Workspaces, members, user profiles, auth sessions / OAuth flow state, and the object-grant / invite sharing model.
tags: [database, workspaces, auth, sharing, invites, multi-tenancy]
timestamp: 2026-06-18T00:00:00Z
---

# Identity & Tenancy

The isolation boundary of the platform. A **workspace** is the tenant; every
domain object hangs off one (or off the [platform scope](well-known-ids.md)).
Authentication is OIDC (Zitadel) in BFF mode or `dev_noop` locally.

# Schema — tenancy

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `workspaces` | `id`, `slug`, `display_name`, `zitadel_org_id`, `is_system`, `default_datacenter_resource_id` → `resources.id`, `archived_at` | The tenant. Soft-deleted via `archived_at`. See [well-known IDs](well-known-ids.md). |
| `workspace_members` | (`workspace_id` → `workspaces.id`, `user_id`), `role`, `added_at` | Membership + role (`owner` / `editor` / `viewer`). |
| `user_profiles` | `user_id`, `email`, `display_name`, `avatar_url` | Cached profile for display; keyed by the OIDC subject UUID. |

# Schema — auth & session state

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `auth_sessions` | `id` (cookie), `subject`, `access_token`, `refresh_token`, `id_token`, `access_expires_at`, `user_json`, `last_seen_at` | Server-side session store; the cookie carries only `id`. |
| `auth_login_flows` | `state`, `pkce_verifier`, `nonce`, `return_to`, `created_at` | In-flight OAuth login (PKCE) state, consumed on callback. |
| `oauth_state` | `state`, `provider`, `pkce_verifier`, `nonce`, `principal_id`, `workspace_id`, `resource_path`, `return_to`, `expires_at` (default `now()+10m`) | OAuth state for **resource** OAuth tokens (e.g. a connected third-party resource), distinct from user login. |

# Schema — sharing & invites

These implement object-level sharing for the [`object_kind`](well-known-ids.md)
types (folder / template / instance / resource / asset).

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `object_grants` | `id`, `workspace_id` → `workspaces.id`, `object_type` (`object_kind` enum), `object_id`, `user_id`, `role`, `granted_by`, `granted_at` | A direct per-object role grant to a user. |
| `pending_invites` | `id`, `workspace_id` → `workspaces.id`, `email`, `role`, `token_hash`, `invited_by`, `expires_at`, `accepted_at`, `accepted_user_id`, `revoked_at`, `status` | Email invite into a workspace; hashed token. |
| `invite_object_grants` | `invite_id` → `pending_invites.id`, `object_type`, `object_id`, `role` | Object grants that are materialized when an invite is accepted. |

# Notes

- `resource`-typed grants are enforced through [`resource_acl`](resources-and-secrets.md);
  `object_grants` covers the folder/template/instance/asset kinds.
- A fresh Zitadel user with no org and no invite resolves to `workspace_id = None`
  and must self-create or be invited (see auto-join isolation work).

# Citations

[1] `service/migrations/20240123_create_workspaces.sql`,
    `20240183_workspace_archived.sql`.
[2] `service/migrations/20240113_create_auth_sessions.sql`,
    `20240122_resource_oauth_tokens.sql`.
[3] `service/migrations/20240171_object_grants.sql`,
    `20240172_pending_invites.sql`,
    `20240174_resource_asset_acl.sql`.
[4] `service/src/auth/`, `service/src/handlers/{workspaces,fork,object_grants,invites}.rs`.
