-- Retire the auto-seeded per-workspace `default` / `model_serving` capacity
-- groups, now superseded by the single platform-scoped groups (Phase 2).
--
-- These rows were created by the OLD `ensure_default_worker_group_all_workspaces`
-- / `ensure_model_serving_group_all_workspaces` boot seeders (one per workspace).
-- Phase 2 replaced those with a single platform-scoped group each and removed the
-- all-workspaces seeders. Migration 20240188 deliberately LEFT the per-workspace
-- rows live to avoid disrupting in-flight workers — but because resource
-- resolution is most-specific-wins (a workspace row shadows a platform row of the
-- same path), every workspace that still has its own `default` / `model_serving`
-- row keeps using THAT instead of the shared platform pool. This soft-delete
-- removes the shadowing so the reserved `default` / `model_serving` aliases
-- resolve to the platform groups for every workspace.
--
-- Fresh databases never ran the all-workspaces seeders, so this matches 0 rows
-- there (idempotent). A workspace that wants a dedicated group can still create
-- one explicitly (any path other than these reserved aliases is unaffected).
--
-- NOTE: any worker/runner currently bound to one of these per-workspace
-- partitions must re-enroll — its routing_partition UUID changes to the platform
-- group's. In dev there are none; in a live deployment, drain/rebind first.
UPDATE resources
SET deleted_at = NOW()
WHERE path IN ('default', 'model_serving')
  AND scope_kind = 'workspace'
  AND deleted_at IS NULL;
