-- Multi-tenancy Phase 4: workspace-scope the producer-edge table.
--
-- catalogue_producers (20240161) keys producer edges by (content_hash,
-- execution_id) GLOBALLY. With per-workspace content-addressing the same
-- content_hash legitimately exists in two workspaces, so the edge key must
-- carry the workspace too: PK becomes (workspace_id, content_hash, execution_id).
--
-- Backfill uses the same authoritative provenance chain as the catalogue
-- entries: source_net (== the producing instance's net_id) -> the instance's
-- template -> workspace_id. Orphans fall back to the default workspace.

-- 1. Column with a default so existing rows are immediately valid.
ALTER TABLE catalogue_producers
    ADD COLUMN IF NOT EXISTS workspace_id UUID
        NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';

-- 2. Backfill via source_net -> workflow_instances.net_id -> template.workspace.
UPDATE catalogue_producers cp
   SET workspace_id = wt.workspace_id
  FROM workflow_instances wi
  JOIN workflow_templates wt ON wt.id = wi.template_id
 WHERE cp.source_net IS NOT NULL
   AND cp.source_net <> ''
   AND cp.source_net = wi.net_id;
-- Orphans keep the default workspace via the column DEFAULT.

-- 3. Re-key. Drop the old composite PK and add the workspace-qualified one.
--    The PK name is the table-name default (`catalogue_producers_pkey`).
ALTER TABLE catalogue_producers DROP CONSTRAINT IF EXISTS catalogue_producers_pkey;
ALTER TABLE catalogue_producers
    ADD PRIMARY KEY (workspace_id, content_hash, execution_id);

-- 4. Workspace-scoped lookup index for the instance/process views that resolve
--    producers by source_net (now always within a workspace).
CREATE INDEX IF NOT EXISTS idx_cat_producers_ws_source_net
    ON catalogue_producers (workspace_id, source_net);
