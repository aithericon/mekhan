-- Model-pool P4 (docs/29 §6'): replica-autoscaler projection / reconciliation table.
--
-- ONE row per `model_policy` resource (UNIQUE(policy_resource_id)). The autoscaler
-- control loop (`service/src/autoscaler`) reconciles it every tick: it computes the
-- desired replica COUNT from the policy + (L2) demand, observes the live count from
-- the fleet roster, actuates via a generated `model-replica-<id>` one-shot net that
-- fires the engine's `stage_template` effect, and upserts this row. It is ALSO the
-- Control-Plane read source (`GET /api/v1/models/replicas`).
--
-- `status` is a state machine (provisioning → active → scaling → draining → stopped,
-- plus `failed`). Enforced here with a DB CHECK following the `allocations` migration
-- convention (the `model_states` P1 table omits the CHECK and enforces in Rust — both
-- are valid under no-back-compat; the replica status enum keeps the CHECK).
--
-- `residency_zone` is recorded on the row for the Control-Plane read + audit. The HARD
-- placement constraint itself rides the seeded `stage_template` request token's `spec`
-- and is enforced engine-side (P3b Nomad renderer fails closed). `last_actuated_at`
-- anchors the durable cooldown gate so flapping survives a mekhan restart.
--
-- NOTE: `datacenter_resource_id` here is the RESOLVED resource row UUID, whereas the
-- `ModelAutoscalePolicy.datacenter_resource_id` config field is a String ALIAS the
-- operator types — the autoscaler resolves alias → uuid before the upsert.
CREATE TABLE model_replicas (
    id                     UUID        NOT NULL DEFAULT gen_random_uuid(),
    workspace_id           UUID        NOT NULL,
    policy_resource_id     UUID        NOT NULL,
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
    UNIQUE (policy_resource_id)
);

CREATE INDEX idx_model_replicas_workspace ON model_replicas (workspace_id);
