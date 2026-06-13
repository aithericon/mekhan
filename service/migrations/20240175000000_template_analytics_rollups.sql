-- Per-template usage analytics — data layer.
--
-- Three pre-aggregated rollup tables plus a structural-metrics column on the
-- template row. The rollups are maintained incrementally by the transactional
-- terminal hook (`service/src/lifecycle.rs`) and the step-executions projector
-- (`service/src/projections/step_executions/consumer.rs`); the metrics column
-- is computed once at publish time from the `WorkflowGraph`
-- (`service/src/process/publish.rs`). The template-analytics handlers read
-- these tables directly — no per-instance fan-out at query time.

-- Structural metrics of the published graph (node/edge/kind counts, nesting
-- depth, has-loops). NULL on pre-migration rows and on any version published
-- before the metrics computation shipped; the analytics reader treats NULL as
-- "not yet computed" and falls back to an empty summary.
ALTER TABLE workflow_templates ADD COLUMN IF NOT EXISTS metrics JSONB;

-- Run-outcome rollup, pre-bucketed by hour. One row per
-- (template, version, hour, mode, outcome); the terminal hook increments
-- `run_count` and folds the run's wall-clock duration into the sum/count pair
-- (so the reader can derive a mean without storing per-run rows). Plain table
-- on purpose — already hour-bucketed, low cardinality, not a hypertable.
CREATE TABLE template_run_rollup (
    template_id       UUID        NOT NULL,
    template_version  INT         NOT NULL,
    bucket_hour       TIMESTAMPTZ NOT NULL,
    -- Instance mode: live | draft | test_run (mirrors workflow_instances.mode).
    mode              TEXT        NOT NULL,
    -- Derived terminal outcome: success | failure | cancelled.
    outcome           TEXT        NOT NULL,
    run_count         BIGINT      NOT NULL DEFAULT 0,
    duration_ms_sum   BIGINT      NOT NULL DEFAULT 0,
    duration_ms_count BIGINT      NOT NULL DEFAULT 0,
    PRIMARY KEY (template_id, template_version, bucket_hour, mode, outcome)
);

-- Timeseries reads scan a template's whole history across versions/modes by
-- time; the PK leads with (template_id, template_version) which doesn't serve
-- a (template_id, bucket_hour) range scan.
CREATE INDEX idx_template_run_rollup_time
    ON template_run_rollup (template_id, bucket_hour);

-- Per-(template, user) run tally for the "who runs this template" view. One
-- row per distinct caller; the terminal hook bumps `run_count` and widens the
-- [first_run, last_run] window.
CREATE TABLE template_user_runs (
    template_id  UUID        NOT NULL,
    user_id      UUID        NOT NULL,
    run_count    BIGINT      NOT NULL DEFAULT 0,
    first_run    TIMESTAMPTZ,
    last_run     TIMESTAMPTZ,
    PRIMARY KEY (template_id, user_id)
);

-- Per-node outcome rollup, aggregated across every instance of a template
-- version. One row per (template, version, node, status); the step-executions
-- projector increments `count` and folds each step's duration into
-- `duration_ms_sum` as it materializes terminal step rows. Drives the
-- "hot/slow node" overlay on the template canvas.
CREATE TABLE template_node_rollup (
    template_id       UUID    NOT NULL,
    template_version  INT     NOT NULL,
    node_id           TEXT    NOT NULL,
    -- Mirrors step_execution.status (completed | failed | skipped | ...).
    status            TEXT    NOT NULL,
    count             BIGINT  NOT NULL DEFAULT 0,
    duration_ms_sum   BIGINT  NOT NULL DEFAULT 0,
    PRIMARY KEY (template_id, template_version, node_id, status)
);
