-- Rename correlation_id → signal_key for consistency with petri-lab engine naming.
-- The value is the signal_key from TokenBridgedOut / EffectCompleted events.

-- causality_cross_links: primary key column
ALTER TABLE causality_cross_links RENAME COLUMN correlation_id TO signal_key;

-- catalogue_entries: nullable provenance column
ALTER TABLE catalogue_entries RENAME COLUMN correlation_id TO signal_key;

-- Recreate the catalogue index on the renamed column
DROP INDEX IF EXISTS idx_cat_correlation_id;
CREATE INDEX IF NOT EXISTS idx_cat_signal_key ON catalogue_entries (signal_key);
