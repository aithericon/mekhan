-- Starfish-style file analytics — Cut 2 (growth snapshots).
--
-- Periodic aggregate snapshots over file_inventory, written by the analytics
-- snapshot job and the manual POST /snapshot trigger. NO primary key on
-- purpose: a manual trigger landing inside the same time bucket just produces
-- a second batch; the timeseries reader dedupes (last batch per bucket) at
-- query time.
CREATE TABLE inventory_snapshots (
    snapped_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    file_server_id  TEXT        NOT NULL,
    dim             TEXT        NOT NULL,  -- total | extension | top_dir | status
    key             TEXT        NOT NULL DEFAULT '',
    file_count      BIGINT      NOT NULL,
    total_bytes     BIGINT      NOT NULL
);

-- Convert to hypertable if TimescaleDB is available
DO $$ BEGIN
    PERFORM create_hypertable('inventory_snapshots', 'snapped_at');
EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'inventory_snapshots: hypertable creation skipped (TimescaleDB not available)';
END $$;

CREATE INDEX idx_invsnap_lookup
    ON inventory_snapshots (dim, file_server_id, key, snapped_at DESC);
