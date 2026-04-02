-- Data catalogue: queryable registry of all artifacts produced by executor jobs.

CREATE TABLE IF NOT EXISTS catalogue_entries (
    id              TEXT        NOT NULL,
    execution_id    TEXT        NOT NULL,
    job_id          TEXT,
    name            TEXT        NOT NULL,
    category        TEXT        NOT NULL,
    filename        TEXT        NOT NULL,
    mime_type       TEXT,
    size_bytes      BIGINT,
    storage_path    TEXT,

    -- Denormalized provenance (from executor job metadata)
    source_net      TEXT,
    source_place    TEXT,
    correlation_id  TEXT,
    process_id      TEXT,
    process_step    TEXT,

    -- Flexible metadata (JSONB)
    file_metadata   JSONB       NOT NULL DEFAULT '{}',
    user_metadata   JSONB       NOT NULL DEFAULT '{}',

    -- Timestamps
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    catalogued_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- NATS dedup key
    nats_msg_id     TEXT        UNIQUE,

    PRIMARY KEY (execution_id, id)
);

-- Common query patterns
CREATE INDEX idx_cat_source_net    ON catalogue_entries (source_net);
CREATE INDEX idx_cat_category      ON catalogue_entries (category);
CREATE INDEX idx_cat_process_id    ON catalogue_entries (process_id);
CREATE INDEX idx_cat_created_at    ON catalogue_entries (created_at);
CREATE INDEX idx_cat_net_category  ON catalogue_entries (source_net, category);

-- JSONB containment queries (@> operator)
CREATE INDEX idx_cat_file_meta     ON catalogue_entries USING GIN (file_metadata jsonb_path_ops);
CREATE INDEX idx_cat_user_meta     ON catalogue_entries USING GIN (user_metadata jsonb_path_ops);
