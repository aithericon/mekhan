-- Unified worker dispatch (docs/23/24) — the per-workspace **default worker
-- group** + the worker routing-partition column.
--
-- The executor worker dispatch collapses to ONE model: there is no anonymous
-- worker path. EVERY worker enrolls and EVERY executor job routes through a
-- GROUP partition on the parallel `executor-<wire>-grp` stream. A step (or a
-- worker) that names no group lands on its workspace's always-seeded "default"
-- worker group. The routing PARTITION is the group's capacity-resource UUID
-- (workspace-safe by construction — two workspaces can both own a "default"
-- group without colliding on a queue).
--
-- Two changes:
--
--   1. Backfill the "default" worker group — a `capacity` resource at path
--      `default` with the `worker` preset axes (competing_consumer · pull ·
--      hold · fixed(1) · partition) — for every existing workspace that lacks
--      one. mekhan ALSO seeds this idempotently at startup
--      (`worker_groups::ensure_default_worker_group_all_workspaces`); this
--      migration covers installs that never reboot through that path.
--
--   2. Add `workers.routing_partition` — the capacity-resource UUID the worker
--      binds its grouped consumer to (vs. `worker_group`, the human alias kept
--      for display). Backfilled from each worker's `worker_group` alias (or the
--      workspace's "default" group) resolved against the capacity rows seeded
--      in step 1.

-- ── 1. Seed the default worker group capacity per workspace ─────────────────
--
-- One `resources` row + its v1 `resource_versions` row per workspace, matching
-- exactly what `create_resource_internal` writes for a `worker`-preset capacity:
-- `resource_type = 'capacity'`, the worker axes in `public_config`, scope =
-- workspace. The seeder principal is the fixed worker-group seeder UUID
-- (`…0bbb`), the same one `worker_groups::ensure_default_worker_group` uses.

WITH seeded AS (
    INSERT INTO resources
        (id, workspace_id, path, resource_type, display_name, latest_version,
         created_by, scope_kind, scope_id, created_at, updated_at)
    SELECT
        gen_random_uuid(),
        w.id,
        'default',
        'capacity',
        'Default workers',
        1,
        '00000000-0000-0000-0000-000000000bbb'::uuid,
        'workspace',
        w.id,
        NOW(),
        NOW()
    FROM workspaces w
    WHERE NOT EXISTS (
        SELECT 1 FROM resources r
        WHERE r.workspace_id = w.id
          AND r.path = 'default'
          AND r.resource_type = 'capacity'
          AND r.deleted_at IS NULL
    )
    RETURNING id, workspace_id
)
INSERT INTO resource_versions
    (resource_id, version, vault_path, public_config, created_by, created_at)
SELECT
    s.id,
    1,
    -- Deterministic Vault path shape `aithericon/resources/{ws}/{rid}/v1`. A
    -- worker capacity has NO secret fields, so this path is never read; it is
    -- written only to satisfy the NOT NULL column + match the create path.
    'aithericon/resources/' || s.workspace_id::text || '/' || s.id::text || '/v1',
    jsonb_build_object(
        'liveness',        'competing_consumer',
        'acceptance',      'auto',
        'capacity_kind',   'fixed',
        'capacity_amount', 1,
        'eligibility',     'partition'
    ),
    '00000000-0000-0000-0000-000000000bbb'::uuid,
    NOW()
FROM seeded s;

-- Grant the seeder principal `read` on each freshly-seeded default group so the
-- resolver behaves identically to a hand-created capacity (create grants the
-- creator `read`).
INSERT INTO resource_acl
    (resource_id, principal_id, principal_kind, permission, granted_by)
SELECT
    r.id,
    '00000000-0000-0000-0000-000000000bbb'::uuid,
    'user',
    'read',
    '00000000-0000-0000-0000-000000000bbb'::uuid
FROM resources r
WHERE r.path = 'default'
  AND r.resource_type = 'capacity'
  AND r.created_by = '00000000-0000-0000-0000-000000000bbb'::uuid
ON CONFLICT DO NOTHING;


-- ── 2. Worker routing-partition column ──────────────────────────────────────
--
-- The capacity-resource UUID the worker's grouped consumer binds. Added
-- nullable, backfilled, then set NOT NULL.

ALTER TABLE workers
    ADD COLUMN routing_partition UUID;

-- Backfill: resolve each worker's `worker_group` alias (or 'default' when NULL)
-- to the workspace's matching worker `capacity` resource UUID.
UPDATE workers wk
SET routing_partition = r.id
FROM resources r
JOIN resource_versions rv
  ON rv.resource_id = r.id AND rv.version = r.latest_version
WHERE r.workspace_id = wk.workspace_id
  AND r.path = COALESCE(wk.worker_group, 'default')
  AND r.resource_type = 'capacity'
  AND r.deleted_at IS NULL
  AND rv.public_config ->> 'liveness' = 'competing_consumer'
  AND rv.public_config ->> 'dispatch' = 'pull';

-- Any worker whose alias does NOT resolve to a backed worker group (e.g. a
-- legacy free-text group with no capacity row) falls back to the workspace's
-- 'default' group, which step 1 guarantees exists.
UPDATE workers wk
SET routing_partition = r.id
FROM resources r
WHERE wk.routing_partition IS NULL
  AND r.workspace_id = wk.workspace_id
  AND r.path = 'default'
  AND r.resource_type = 'capacity'
  AND r.deleted_at IS NULL;

ALTER TABLE workers
    ALTER COLUMN routing_partition SET NOT NULL;
