-- Multi-tenancy Phase 4: workspace-scope the catalogue query-layer tables —
-- saved queries (20240168), registered data types + their digest membership
-- (20240173).
--
-- These tables have NO provenance chain to backfill from (a saved query / data
-- type is authored directly, not produced by a run), so every existing row goes
-- to the default workspace (Uuid::nil()) via the column DEFAULT. The
-- name-uniqueness and digest-ownership constraints are re-cut per-workspace.

-- ---------------------------------------------------------------------------
-- catalogue_saved_queries: UNIQUE(name) -> UNIQUE(workspace_id, name)
-- ---------------------------------------------------------------------------
ALTER TABLE catalogue_saved_queries
    ADD COLUMN IF NOT EXISTS workspace_id UUID
        NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';

-- Inline `UNIQUE (name)` in the CREATE TABLE -> default name
-- `catalogue_saved_queries_name_key`.
ALTER TABLE catalogue_saved_queries
    DROP CONSTRAINT IF EXISTS catalogue_saved_queries_name_key;
ALTER TABLE catalogue_saved_queries
    ADD CONSTRAINT uq_cat_saved_queries_ws_name UNIQUE (workspace_id, name);

-- ---------------------------------------------------------------------------
-- catalogue_data_types: UNIQUE(name) -> UNIQUE(workspace_id, name)
-- ---------------------------------------------------------------------------
ALTER TABLE catalogue_data_types
    ADD COLUMN IF NOT EXISTS workspace_id UUID
        NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';

-- Inline `name TEXT NOT NULL UNIQUE` -> default name
-- `catalogue_data_types_name_key`.
ALTER TABLE catalogue_data_types
    DROP CONSTRAINT IF EXISTS catalogue_data_types_name_key;
ALTER TABLE catalogue_data_types
    ADD CONSTRAINT uq_cat_data_types_ws_name UNIQUE (workspace_id, name);

-- ---------------------------------------------------------------------------
-- catalogue_data_type_digests: digest membership.
--
-- The original PK is `digest` alone — "a digest is owned by AT MOST ONE data
-- type" GLOBALLY. With per-workspace content-addressing the same fingerprint
-- digest can legitimately exist in two workspaces, each promoting it to its own
-- data type, so the ownership scope becomes per-workspace:
-- PK (digest) -> PK (workspace_id, digest).
--
-- workspace_id is denormalized here (it also lives on the parent data type) so
-- the attach-time 409 check ("is this digest already owned in MY workspace?")
-- and the PK both key on it directly. The backfill copies it down from the
-- owning data type so the two never disagree.
-- ---------------------------------------------------------------------------
ALTER TABLE catalogue_data_type_digests
    ADD COLUMN IF NOT EXISTS workspace_id UUID
        NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';

-- Backfill from the parent data type (which we just defaulted to nil above, but
-- copy explicitly so this survives any future data-type backfill that assigns
-- real workspaces).
UPDATE catalogue_data_type_digests d
   SET workspace_id = t.workspace_id
  FROM catalogue_data_types t
 WHERE d.data_type_id = t.id;

ALTER TABLE catalogue_data_type_digests
    DROP CONSTRAINT IF EXISTS catalogue_data_type_digests_pkey;
ALTER TABLE catalogue_data_type_digests
    ADD PRIMARY KEY (workspace_id, digest);
