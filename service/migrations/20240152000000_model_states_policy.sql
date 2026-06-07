-- Fold the model autoscale POLICY into the model SET (docs/31 follow-up).
--
-- `ModelAutoscalePolicy` stops being a resource KIND — it becomes a plain
-- in-memory DTO built from a `model_states` row. The per-model policy now lives
-- as nullable columns ON `model_states` (the single per-model config row), and
-- `model_replicas` is RE-KEYED from `policy_resource_id` (the deleted resource)
-- to UNIQUE (workspace_id, model_id) — one reconciliation row per (workspace,
-- model). The `node_pool` + `datacenter` resources STAY resources.
--
-- No data to preserve (model_policy is operator-curated; no seeds reference it).

-- 1) The policy columns land on the per-model config row (all nullable: a model
--    in the SET without an autoscale policy keeps them NULL; `dedicated` defaults
--    to FALSE so the fallback flag has a concrete value).
ALTER TABLE model_states
    ADD COLUMN autoscale_mode        TEXT,
    ADD COLUMN desired_replicas      INT,
    ADD COLUMN scale_up_threshold    DOUBLE PRECISION,
    ADD COLUMN scale_down_threshold  DOUBLE PRECISION,
    ADD COLUMN cooldown_secs         BIGINT,
    ADD COLUMN node_pool             TEXT,
    ADD COLUMN residency_zone        TEXT,
    ADD COLUMN dedicated             BOOLEAN NOT NULL DEFAULT FALSE;

-- 2) Re-key `model_replicas` from `policy_resource_id` → (workspace_id, model_id).
--    Identical to 20240146000000_model_replicas.sql EXCEPT: the `policy_resource_id`
--    column + its UNIQUE are gone, replaced by UNIQUE (workspace_id, model_id) —
--    one reconciliation row per (workspace, model) now that the policy lives on
--    `model_states` rather than its own resource. No data to preserve.
DROP TABLE model_replicas;

CREATE TABLE model_replicas (
    id                     UUID        NOT NULL DEFAULT gen_random_uuid(),
    workspace_id           UUID        NOT NULL,
    model_id               TEXT        NOT NULL,
    datacenter_resource_id UUID        NOT NULL,
    replica_slug           TEXT,
    desired_count          INT         NOT NULL DEFAULT 0,
    observed_count         INT         NOT NULL DEFAULT 0,
    status                 TEXT        NOT NULL
        CHECK (status IN ('provisioning', 'active', 'scaling', 'draining', 'stopped', 'failed')),
    residency_zone         TEXT,
    last_error             TEXT,
    last_actuated_at       TIMESTAMPTZ,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (id),
    UNIQUE (workspace_id, model_id)
);

CREATE INDEX idx_model_replicas_workspace ON model_replicas (workspace_id);

-- 3) Drop every `model_policy` resource (and its versions) — the kind is gone.
DELETE FROM resource_versions
    WHERE resource_id IN (SELECT id FROM resources WHERE resource_type = 'model_policy');
DELETE FROM resources WHERE resource_type = 'model_policy';
