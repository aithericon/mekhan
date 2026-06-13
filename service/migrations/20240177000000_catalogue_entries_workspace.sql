-- Multi-tenancy Phase 4: workspace-scope the content-addressed catalogue.
--
-- The catalogue was content-addressed GLOBALLY (`UNIQUE(content_hash)` from
-- 20240153). That collapses byte-identical artifacts ACROSS workspaces onto a
-- single row — a cross-tenant leak the moment two tenants produce the same
-- bytes. This makes content-addressing PER-WORKSPACE: identity becomes
-- `(workspace_id, content_hash)`.
--
-- Backfill is from the AUTHORITATIVE provenance chain, NOT by parsing net_id
-- (whose format is changing in this same multi-tenancy arc):
--   catalogue_entries.source_net == workflow_instances.net_id
--   workflow_instances.template_id -> workflow_templates.workspace_id
-- Rows that don't resolve (legacy/by-reference entries with no live instance,
-- or NULL source_net) fall back to the default workspace (Uuid::nil()).

-- ---------------------------------------------------------------------------
-- 1. Column. Add nullable-by-default-value so the existing rows are valid
--    immediately, then backfill, then it stays NOT NULL.
-- ---------------------------------------------------------------------------
ALTER TABLE catalogue_entries
    ADD COLUMN IF NOT EXISTS workspace_id UUID
        NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';

-- ---------------------------------------------------------------------------
-- 2. Backfill from the authoritative join. source_net is the producing
--    instance's net_id; the instance's template carries the workspace.
-- ---------------------------------------------------------------------------
UPDATE catalogue_entries ce
   SET workspace_id = wt.workspace_id
  FROM workflow_instances wi
  JOIN workflow_templates wt ON wt.id = wi.template_id
 WHERE ce.source_net IS NOT NULL
   AND ce.source_net <> ''
   AND ce.source_net = wi.net_id;
-- Orphans (no matching instance, or NULL/empty source_net) keep the default
-- workspace via the column DEFAULT above.

-- ---------------------------------------------------------------------------
-- 3. Per-workspace content-addressing. Drop the global UNIQUE(content_hash)
--    constraint (added by 20240153 as `uq_cat_content_hash`) and replace it
--    with a per-workspace one. content_hash stays NULLable (job-net artifacts
--    are not hashed), so many NULLs per workspace remain allowed; every
--    non-null (workspace, hash) pair is unique.
-- ---------------------------------------------------------------------------
ALTER TABLE catalogue_entries DROP CONSTRAINT IF EXISTS uq_cat_content_hash;
ALTER TABLE catalogue_entries
    ADD CONSTRAINT uq_cat_ws_content_hash UNIQUE (workspace_id, content_hash);

-- ---------------------------------------------------------------------------
-- 4. Indexes. The catalogue query layer (20240168) filters by the virtual
--    `meta.format` field; that filter is now always workspace-scoped, so a
--    composite expression index keeps the hot list path tight.
-- ---------------------------------------------------------------------------
CREATE INDEX IF NOT EXISTS idx_cat_ws_format
    ON catalogue_entries (workspace_id, (file_metadata->>'format'));
CREATE INDEX IF NOT EXISTS idx_cat_workspace
    ON catalogue_entries (workspace_id);
