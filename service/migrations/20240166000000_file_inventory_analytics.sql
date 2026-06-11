-- Starfish-style file analytics — Cut 1 substrate (promoted columns).
--
-- Aggregation dimensions (size / mtime / ownership / extension) were buried in
-- the `provenance` JSONB; group-by analytics need real columns + indexes.
-- Writers start populating these in the same change-set (ObservedFacts); this
-- migration promotes the columns and backfills whatever existing provenance
-- already knows.

ALTER TABLE file_inventory ADD COLUMN size_bytes BIGINT;
ALTER TABLE file_inventory ADD COLUMN mtime      TIMESTAMPTZ;
ALTER TABLE file_inventory ADD COLUMN uid        INTEGER;
ALTER TABLE file_inventory ADD COLUMN gid        INTEGER;

-- GENERATED so none of the inventory writers can ever drift from the
-- extension derivation — they never name this column in an INSERT.
ALTER TABLE file_inventory ADD COLUMN extension TEXT GENERATED ALWAYS AS
    (nullif(lower(substring(path from '\.([A-Za-z0-9_~#-]{1,16})$')), '')) STORED;

-- ---------------------------------------------------------------------------
-- One-time backfill from provenance JSONB.
-- ---------------------------------------------------------------------------
-- Exception-safe casts: provenance carries at least two mtime serializer
-- flavors plus arbitrary caller-supplied JSON (/register, /index). A value
-- that doesn't cast must become NULL — never abort the migration.
CREATE FUNCTION pg_temp.try_ts(t TEXT) RETURNS TIMESTAMPTZ
LANGUAGE plpgsql IMMUTABLE AS $$
BEGIN
    RETURN t::timestamptz;
EXCEPTION WHEN OTHERS THEN
    RETURN NULL;
END $$;

CREATE FUNCTION pg_temp.try_bigint(t TEXT) RETURNS BIGINT
LANGUAGE plpgsql IMMUTABLE AS $$
BEGIN
    RETURN t::bigint;
EXCEPTION WHEN OTHERS THEN
    RETURN NULL;
END $$;

UPDATE file_inventory SET
    size_bytes = COALESCE(
        pg_temp.try_bigint(provenance->>'observed_size'),
        pg_temp.try_bigint(provenance->>'size')
    ),
    mtime = COALESCE(
        pg_temp.try_ts(provenance->>'mtime'),
        pg_temp.try_ts(provenance->>'modified')
    )
WHERE provenance ?| ARRAY['observed_size', 'size', 'mtime', 'modified'];

-- ---------------------------------------------------------------------------
-- Indexes for the breakdown dimensions.
-- ---------------------------------------------------------------------------
CREATE INDEX idx_inv_extension ON file_inventory (extension);
CREATE INDEX idx_inv_mtime     ON file_inventory (mtime);
CREATE INDEX idx_inv_uid       ON file_inventory (uid);

-- Directory drill-down does `file_server_id = $1 AND path LIKE 'prefix/%'`;
-- text_pattern_ops makes the LIKE prefix scan index-driven regardless of the
-- database collation.
CREATE INDEX idx_inv_server_path_prefix
    ON file_inventory (file_server_id, path text_pattern_ops);

-- Substring path search rides pg_trgm when available; falls back to seq scan.
DO $$ BEGIN
    CREATE EXTENSION IF NOT EXISTS pg_trgm;
    CREATE INDEX idx_inv_path_trgm ON file_inventory USING GIN (path gin_trgm_ops);
EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'pg_trgm not available — path substring search will seq-scan';
END $$;
