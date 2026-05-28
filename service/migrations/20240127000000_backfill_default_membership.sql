-- Backfill: auto-join existing principals to the `default` workspace as
-- editors.
--
-- Migration 20240124 moved every non-demo template into `default`, but
-- 20240123 seeded only the dev-noop user as a member there. Real (Zitadel)
-- principals therefore land in `demos` alone (viewer) and hit
-- "not a member of this template's workspace" (403) when editing any migrated
-- template. The resolver now auto-joins on login (DbPrincipalResolver::
-- ensure_default_workspace_membership), but the cached BFF session only
-- refreshes at next login/token-refresh — so this backfills everyone already
-- known to the system to unblock live sessions immediately (the write gate
-- checks the membership row directly, not the cached session workspace_id).
--
-- "Already known" = anyone with an existing membership row (i.e. has logged in
-- since the workspace migration) plus every author of a template now living in
-- `default`. Idempotent via ON CONFLICT — safe to re-run.
INSERT INTO workspace_members (workspace_id, user_id, role)
SELECT d.id, u.user_id, 'editor'
  FROM (SELECT id FROM workspaces WHERE slug = 'default') d
  CROSS JOIN (
        SELECT DISTINCT user_id FROM workspace_members
        UNION
        SELECT DISTINCT author_id
          FROM workflow_templates
         WHERE workspace_id = (SELECT id FROM workspaces WHERE slug = 'default')
  ) u
ON CONFLICT (workspace_id, user_id) DO NOTHING;
