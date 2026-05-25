-- Template tests: fixed start_tokens + human-task answers + structured
-- assertions, attached to a logical template family (the family root id,
-- which is `base_template_id` when set, else the template row's own `id` —
-- see `service::handlers::template_tests::family_root`). Float across
-- versions; never re-author when a template is edited.
--
-- Runs are recorded per-test with the version they ran against, so the
-- publication gate can detect "stale" passes (test passed v3 but we're
-- publishing v4).
CREATE TABLE template_tests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Family root (matches the logical template; see module doc above).
    template_id UUID NOT NULL,

    name TEXT NOT NULL,

    -- Disabled tests are skipped by the runner and ignored by the publish
    -- gate. Useful for WIP / known-flaky / intentionally-stale tests.
    enabled BOOLEAN NOT NULL DEFAULT TRUE,

    -- Vec<StartToken> — exactly what `CreateInstanceRequest.start_tokens`
    -- would carry for a manual run of this template.
    start_tokens JSONB NOT NULL,

    -- { "<node_slug>": { ...form_data } } — fixture answers keyed by the
    -- HumanTask node's author slug (see `WorkflowNode::slug`). The runner
    -- auto-completes each `human.request.<net_id>.>` from this map; a
    -- missing slug fails the test immediately (no hangs).
    human_answers JSONB NOT NULL DEFAULT '{}',

    -- Vec<Assertion> = [{ path, op, value }]. `path` is dot-pathed into a
    -- synthetic scope object `{ result, steps.<slug>.output }` the runner
    -- builds from the terminal instance.
    assertions JSONB NOT NULL DEFAULT '[]',

    -- Updated by the runner after every run; the publish gate reads these
    -- to detect stale passes (`last_run_against_version != current_version`).
    last_run_at TIMESTAMPTZ,
    last_run_against_version INT,
    last_run_passed BOOLEAN,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_by UUID NOT NULL,

    UNIQUE (template_id, name)
);

CREATE INDEX idx_template_tests_template ON template_tests(template_id);
CREATE INDEX idx_template_tests_enabled ON template_tests(template_id) WHERE enabled = TRUE;

-- Per-execution log. One row per run; `instance_id` links to the synthetic
-- workflow_instances row spawned with `mode = 'test_run'`.
CREATE TABLE template_test_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    test_id UUID NOT NULL REFERENCES template_tests(id) ON DELETE CASCADE,

    -- Synthetic instance the runner spawned. Not a FK so a later cascade
    -- delete of the instance row (manual cleanup) doesn't take the run log
    -- with it — the run record stands on its own as audit.
    instance_id UUID NOT NULL,

    -- The version the test ran against (matches workflow_instances.template_version).
    template_version INT NOT NULL,

    -- 'passed' = every assertion held. 'failed' = an assertion did not hold.
    -- 'error' = runner-internal failure (timeout, missing human_answers
    -- slug, instance failed for reasons other than an assertion).
    status TEXT NOT NULL CHECK (status IN ('passed', 'failed', 'error')),

    -- { assertion_idx, path, op, expected, actual, message } for the
    -- offending check on `failed`; { reason, detail } for `error`. NULL on
    -- pass.
    failure_detail JSONB,

    -- Captured synthetic scope for debugging: same shape the assertion
    -- evaluator saw. NULL only for `error` runs that never reached scope.
    final_scope JSONB,

    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finished_at TIMESTAMPTZ,
    duration_ms INT
);

CREATE INDEX idx_test_runs_test ON template_test_runs(test_id, started_at DESC);

-- Categorize an instance: 'live' is the historical default and matches
-- every existing row (the NOT NULL default takes care of backfill).
-- 'draft' is a user-initiated experimental run that should stay out of
-- production dashboards. 'test_run' is spawned by the template-test runner
-- and is auto-cleaned by a later retention pass.
ALTER TABLE workflow_instances
    ADD COLUMN mode TEXT NOT NULL DEFAULT 'live'
        CHECK (mode IN ('live', 'draft', 'test_run')),
    ADD COLUMN test_id UUID REFERENCES template_tests(id) ON DELETE SET NULL,
    -- "Promoted from this instance" — set when a `template_tests` row was
    -- created by scooping an existing instance's event log. Audit-only.
    ADD COLUMN source_instance_id UUID REFERENCES workflow_instances(id) ON DELETE SET NULL;

-- Most instance-list queries default to mode = 'live'; this partial index
-- keeps the common path on the small slice and only indexes the rarer
-- non-live rows for the explicit "show drafts/test runs" filter.
CREATE INDEX idx_instances_mode_nonlive ON workflow_instances(mode) WHERE mode <> 'live';
CREATE INDEX idx_instances_test_id ON workflow_instances(test_id) WHERE test_id IS NOT NULL;
