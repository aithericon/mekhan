-- Process tracking: Mekhan-native HPI with trace_id as primary identity.
-- TimescaleDB hypertables are used when available; falls back to regular tables.

DO $$ BEGIN
    CREATE EXTENSION IF NOT EXISTS timescaledb;
EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'TimescaleDB not available — using regular tables for metrics/logs';
END $$;

-- ── Core process table ──────────────────────────────────────────────────────
-- trace_id (from W3C traceparent) is the primary identity.
-- Auto-created when Mekhan sees events with an unknown trace_id.

CREATE TABLE IF NOT EXISTS hpi_processes (
    trace_id        TEXT        PRIMARY KEY,
    name            TEXT,
    kind            TEXT,
    status          TEXT        NOT NULL DEFAULT 'active',
    owner           TEXT,
    hpi_process_id  TEXT,          -- legacy compat (nullable)
    config          JSONB       NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_hpi_proc_status    ON hpi_processes (status);
CREATE INDEX idx_hpi_proc_kind      ON hpi_processes (kind);
CREATE INDEX idx_hpi_proc_created   ON hpi_processes (created_at DESC);
CREATE INDEX idx_hpi_proc_legacy    ON hpi_processes (hpi_process_id) WHERE hpi_process_id IS NOT NULL;

-- ── Human tasks ─────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS hpi_tasks (
    id              TEXT        PRIMARY KEY DEFAULT gen_random_uuid()::text,
    trace_id        TEXT        NOT NULL REFERENCES hpi_processes(trace_id),
    span_id         TEXT,
    title           TEXT        NOT NULL,
    status          TEXT        NOT NULL DEFAULT 'pending',
    assignee        TEXT,
    detail          JSONB       NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at    TIMESTAMPTZ
);

CREATE INDEX idx_hpi_task_trace  ON hpi_tasks (trace_id);
CREATE INDEX idx_hpi_task_status ON hpi_tasks (status);

-- ── Time-series metrics ─────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS hpi_metrics (
    trace_id        TEXT        NOT NULL,
    span_id         TEXT,
    key             TEXT        NOT NULL,
    value           FLOAT8      NOT NULL,
    timestamp       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Convert to hypertable if TimescaleDB is available
DO $$ BEGIN
    PERFORM create_hypertable('hpi_metrics', 'timestamp');
EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'hpi_metrics: hypertable creation skipped (TimescaleDB not available)';
END $$;

CREATE INDEX idx_hpi_metric_trace ON hpi_metrics (trace_id, timestamp DESC);
CREATE INDEX idx_hpi_metric_key   ON hpi_metrics (trace_id, key, timestamp DESC);

-- ── Structured process logs ─────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS hpi_logs (
    id              BIGINT GENERATED ALWAYS AS IDENTITY,
    trace_id        TEXT        NOT NULL,
    span_id         TEXT,
    level           TEXT        NOT NULL DEFAULT 'info',
    source          TEXT,
    message         TEXT        NOT NULL,
    detail          JSONB       NOT NULL DEFAULT '{}',
    timestamp       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Convert to hypertable if TimescaleDB is available
DO $$ BEGIN
    PERFORM create_hypertable('hpi_logs', 'timestamp');
EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'hpi_logs: hypertable creation skipped (TimescaleDB not available)';
END $$;

CREATE INDEX idx_hpi_log_trace ON hpi_logs (trace_id, timestamp DESC);
CREATE INDEX idx_hpi_log_level ON hpi_logs (level, timestamp DESC);

-- ── Add trace_id to catalogue for joining ───────────────────────────────────

ALTER TABLE catalogue_entries ADD COLUMN IF NOT EXISTS trace_id TEXT;
CREATE INDEX IF NOT EXISTS idx_cat_trace_id ON catalogue_entries (trace_id) WHERE trace_id IS NOT NULL;
