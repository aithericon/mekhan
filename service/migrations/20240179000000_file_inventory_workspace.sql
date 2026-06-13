-- Multi-tenancy Phase 4: workspace-scope the physical file inventory.
--
-- file_inventory (20240153) is one row per physical copy, keyed
-- UNIQUE(file_server_id, path) GLOBALLY — it was explicitly documented as
-- "currently global/single-workspace". A file server is workspace-scoped
-- (file_servers.workspace_id, with UNIQUE(workspace_id, key)), and inventory
-- joins to it SOFTLY by `file_inventory.file_server_id == file_servers.key`.
--
-- Backfill: derive each inventory row's workspace from its owning file server.
-- Because `key` is unique only PER WORKSPACE, a key could in principle map to
-- several servers across workspaces; inventory has been single-workspace in
-- practice, so we pick the lowest workspace_id deterministically (MIN) to keep
-- the backfill one-row-in/one-row-out — no row duplication. Unknown servers
-- (a crawl observed a file before its server was registered) fall back to the
-- default workspace.

-- 1. Column with a default so existing rows are valid immediately.
ALTER TABLE file_inventory
    ADD COLUMN IF NOT EXISTS workspace_id UUID
        NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';

-- 2. Backfill from the owning file server (soft join key == file_servers.key).
--    Postgres has no MIN(uuid) aggregate, so aggregate on the text form and cast
--    back: deterministic (lexicographically smallest uuid) and, since key is
--    single-workspace in practice, a no-op collapse with no row fan-out.
UPDATE file_inventory fi
   SET workspace_id = fs.ws
  FROM (
        SELECT key, MIN(workspace_id::text)::uuid AS ws
          FROM file_servers
         GROUP BY key
       ) fs
 WHERE fi.file_server_id = fs.key;
-- Orphans (no registered server) keep the default workspace via the DEFAULT.

-- 3. Re-key uniqueness to include the workspace. The old constraint was created
--    by the inline `UNIQUE (file_server_id, path)` in the CREATE TABLE, whose
--    default name is `file_inventory_file_server_id_path_key`.
ALTER TABLE file_inventory
    DROP CONSTRAINT IF EXISTS file_inventory_file_server_id_path_key;
ALTER TABLE file_inventory
    ADD CONSTRAINT uq_inv_ws_server_path UNIQUE (workspace_id, file_server_id, path);

-- 4. Workspace-scoped variants of the hot crawl/reconcile lookups.
CREATE INDEX IF NOT EXISTS idx_inv_ws_server_status
    ON file_inventory (workspace_id, file_server_id, status);
CREATE INDEX IF NOT EXISTS idx_inv_workspace
    ON file_inventory (workspace_id);
