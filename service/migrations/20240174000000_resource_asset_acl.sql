-- Consolidate resources & assets into the ONE object-ACL model (docs/20 + the
-- Phase-3 object_grants spine). Two parts:
--
-- 1. Extend the `object_kind` enum so a grant can bind a resource or an asset,
--    exactly like folders/templates/instances. The resolver
--    (`service/src/auth/grants.rs`) gains Resource/Asset branches that read the
--    row's existing `(scope_kind, scope_id)` as the inheritance parent (folder
--    path / owning template / workspace) — so placement stays the SAME data,
--    it just now ALSO drives the ACL chain instead of a separate
--    membership-only visibility check.
--
-- 2. A `restricted` opt-out flag. The Phase-1..5 rule is "workspace role is a
--    FLOOR" — every member is at least their ws role on every object, so
--    nothing is ever truly hidden. `restricted = true` turns the floor OFF for
--    that object: access then comes solely from explicit grants + container
--    inheritance, with workspace Owner/Admin still bypassing. Set on a folder
--    it cascades to everything in the subtree (a private area). Default false
--    keeps today's behaviour byte-for-byte.
--
-- `ALTER TYPE ... ADD VALUE` is committed here and only USED at runtime (later,
-- separate transactions), so the PG12+ "can't use a new enum value in the same
-- transaction" rule is satisfied.

ALTER TYPE object_kind ADD VALUE IF NOT EXISTS 'resource';
ALTER TYPE object_kind ADD VALUE IF NOT EXISTS 'asset';

-- Privacy opt-out. Resources & assets are the primary target; folders carry it
-- too so "make this folder private" cascades to its whole subtree (resolved by
-- a path-prefix check in grants.rs, the same shape as folder-grant inheritance).
ALTER TABLE resources ADD COLUMN IF NOT EXISTS restricted BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE assets    ADD COLUMN IF NOT EXISTS restricted BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE folders   ADD COLUMN IF NOT EXISTS restricted BOOLEAN NOT NULL DEFAULT false;
