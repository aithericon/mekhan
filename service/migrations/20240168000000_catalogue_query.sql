-- Catalogue query layer (virtual meta.* fields + facets + saved queries).
--
-- 1. Expression indexes backing the hot virtual `meta.*` filter fields and the
--    `format` facet. `jsonb ->> text` and the text→bigint cast are IMMUTABLE,
--    so they are index-safe.
CREATE INDEX IF NOT EXISTS idx_cat_fmeta_format
    ON catalogue_entries ((file_metadata->>'format'));
CREATE INDEX IF NOT EXISTS idx_cat_fmeta_num_rows
    ON catalogue_entries (((file_metadata->>'num_rows')::bigint));
CREATE INDEX IF NOT EXISTS idx_cat_fmeta_schema
    ON catalogue_entries ((file_metadata->'schema_fingerprint'->>'digest'));

-- 2. Saved queries: named, shareable catalogue query strings (`q` is the raw
--    list-endpoint query string; `params` is a free-form UI side-car).
CREATE TABLE catalogue_saved_queries (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT        NOT NULL,
    description TEXT,
    q           TEXT        NOT NULL,
    params      JSONB       NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (name)
);
