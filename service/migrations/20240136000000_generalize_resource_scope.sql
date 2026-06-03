-- docs/20 §2/§7 — Generalize resource ownership from a bare `workspace_id` to a
-- polymorphic `(scope_kind, scope_id)` owner, plus a free `display_path` virtual
-- folder column (§3).
--
-- Transitional: we KEEP `resources.workspace_id` (denormalized) and backfill the
-- new scope columns from it. Reads / the picker / the compiler switch to the new
-- columns; the old `(workspace_id, path)` unique stays in place but the new
-- `(scope_kind, scope_id, path)` partial-unique is the forward-looking constraint.
-- Dropping `workspace_id` is deferred (see docs/20 §7 implementation note).

ALTER TABLE resources
    ADD COLUMN IF NOT EXISTS scope_kind   TEXT NOT NULL DEFAULT 'workspace',
    ADD COLUMN IF NOT EXISTS scope_id     UUID,
    ADD COLUMN IF NOT EXISTS display_path TEXT;

-- Backfill: existing rows are workspace-scoped, owner = old workspace_id.
UPDATE resources
   SET scope_id = workspace_id
 WHERE scope_id IS NULL;

-- The generalized uniqueness from docs/20 §2: (scope_kind, scope_id, ref_key)
-- unique among live rows. `path` is the ref-key here (the flat identifier).
CREATE UNIQUE INDEX IF NOT EXISTS idx_resources_scope_path
    ON resources (scope_kind, scope_id, path)
    WHERE deleted_at IS NULL;

-- Downward-visibility list queries filter by (scope_kind, scope_id).
CREATE INDEX IF NOT EXISTS idx_resources_scope
    ON resources (scope_kind, scope_id)
    WHERE deleted_at IS NULL;
