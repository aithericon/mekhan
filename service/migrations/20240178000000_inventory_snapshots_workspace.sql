-- Multi-tenancy Phase 4: workspace-scope the analytics snapshot timeseries.
--
-- inventory_snapshots (20240167) is a TimescaleDB hypertable (partitioned on
-- `snapped_at`) of periodic file_inventory aggregates. It has NO primary key by
-- design (manual triggers append a second batch per bucket; the reader dedupes
-- at query time), so there is no key to re-cut — we only add the scoping column
-- + a workspace-aware lookup index. ALTER TABLE ADD COLUMN on a hypertable is
-- propagated to all chunks by TimescaleDB.
--
-- Backfill: snapshots carry file_server_id; resolve the workspace through the
-- owning file server exactly like file_inventory (#3). Unknown servers fall
-- back to the default workspace.

-- 1. Column with a default so existing chunks' rows are valid immediately.
ALTER TABLE inventory_snapshots
    ADD COLUMN IF NOT EXISTS workspace_id UUID
        NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';

-- 2. Backfill via file_server_id -> file_servers.key (MIN workspace, matching
--    the file_inventory backfill — deterministic, no row fan-out).
UPDATE inventory_snapshots ivs
   SET workspace_id = fs.ws
  FROM (
        SELECT key, MIN(workspace_id) AS ws
          FROM file_servers
         GROUP BY key
       ) fs
 WHERE ivs.file_server_id = fs.key;
-- Orphans keep the default workspace via the column DEFAULT.

-- 3. Workspace-scoped variant of the timeseries lookup index from 20240167.
--    snapped_at stays last (descending) — every timeseries read is now scoped
--    to a workspace first.
CREATE INDEX IF NOT EXISTS idx_invsnap_ws_lookup
    ON inventory_snapshots (workspace_id, dim, file_server_id, key, snapped_at DESC);
