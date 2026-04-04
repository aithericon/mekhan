-- Clean cut: drop and recreate HPI tables with process_id replacing trace_id.
-- No prod data to preserve.

DROP TABLE IF EXISTS hpi_logs;
DROP TABLE IF EXISTS hpi_metrics;
DROP TABLE IF EXISTS hpi_tasks;
DROP TABLE IF EXISTS hpi_processes;

CREATE TABLE hpi_processes (
    process_id      TEXT        PRIMARY KEY,
    name            TEXT,
    kind            TEXT,
    status          TEXT        NOT NULL DEFAULT 'active',
    owner           TEXT,
    config          JSONB       NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_hpi_proc_status  ON hpi_processes (status);
CREATE INDEX idx_hpi_proc_kind    ON hpi_processes (kind);
CREATE INDEX idx_hpi_proc_created ON hpi_processes (created_at DESC);

CREATE TABLE hpi_tasks (
    id              TEXT        PRIMARY KEY DEFAULT gen_random_uuid()::text,
    process_id      TEXT        NOT NULL REFERENCES hpi_processes(process_id),
    title           TEXT        NOT NULL,
    status          TEXT        NOT NULL DEFAULT 'pending',
    assignee        TEXT,
    detail          JSONB       NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at    TIMESTAMPTZ
);
CREATE INDEX idx_hpi_task_process ON hpi_tasks (process_id);
CREATE INDEX idx_hpi_task_status  ON hpi_tasks (status);

CREATE TABLE hpi_metrics (
    process_id      TEXT        NOT NULL,
    key             TEXT        NOT NULL,
    value           FLOAT8      NOT NULL,
    timestamp       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_hpi_metric_process ON hpi_metrics (process_id, timestamp DESC);
CREATE INDEX idx_hpi_metric_key     ON hpi_metrics (process_id, key, timestamp DESC);

DO $$ BEGIN
    PERFORM create_hypertable('hpi_metrics', 'timestamp');
EXCEPTION WHEN OTHERS THEN NULL; END $$;

CREATE TABLE hpi_logs (
    id              BIGINT GENERATED ALWAYS AS IDENTITY,
    process_id      TEXT        NOT NULL,
    level           TEXT        NOT NULL DEFAULT 'info',
    source          TEXT,
    message         TEXT        NOT NULL,
    detail          JSONB       NOT NULL DEFAULT '{}',
    timestamp       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_hpi_log_process ON hpi_logs (process_id, timestamp DESC);
CREATE INDEX idx_hpi_log_level   ON hpi_logs (level, timestamp DESC);

DO $$ BEGIN
    PERFORM create_hypertable('hpi_logs', 'timestamp');
EXCEPTION WHEN OTHERS THEN NULL; END $$;

-- Update catalogue_entries: drop trace_id, add process_id + source_event_sequence
ALTER TABLE catalogue_entries DROP COLUMN IF EXISTS trace_id;
ALTER TABLE catalogue_entries ADD COLUMN IF NOT EXISTS process_id TEXT;
ALTER TABLE catalogue_entries ADD COLUMN IF NOT EXISTS source_event_sequence BIGINT;
CREATE INDEX IF NOT EXISTS idx_cat_process_id ON catalogue_entries (process_id) WHERE process_id IS NOT NULL;
