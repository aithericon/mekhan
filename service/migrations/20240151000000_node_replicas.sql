-- Model-pool reconciliation (docs/31 Phase 1): node-fleet reconciliation table.
--
-- Loop 1's durable target (clone of `20240146000000_model_replicas.sql`). ONE row
-- per `node_pool` capacity resource (UNIQUE(pool_resource_id)). The node-fleet
-- scaler (`service/src/autoscaler/node_actuate.rs`, Phase 2) reconciles it every
-- tick: it computes the desired NODE count from aggregate model demand routed to
-- the pool vs the per-node concurrency budget `C` (`node_pool.max_num_seqs`),
-- observes the live C-weighted capacity from FleetLiveness (`Σ present-node C`),
-- actuates via a generated `node-pool-<id>-<gen>` one-shot net that fires the
-- engine's `stage_template` effect with a model-AGNOSTIC engine spec, and upserts
-- this row.
--
-- Differences from `model_replicas` (docs/31 dossier §B):
--   * UNIQUE(pool_resource_id) — NOT policy_resource_id (the `node_pool` resource).
--   * node_slug — NOT replica_slug (the stable Nomad service-job id for the fleet).
--   * desired_nodes / observed_nodes — NOT desired_count / observed_count.
--   * NEW observed_slots — the C-weighted aggregate (`Σ present-node C`) from
--     FleetLiveness; observed_nodes is the head-count, observed_slots the capacity.
--
-- `observed_nodes` / `observed_slots` are FleetLiveness-driven (DERIVED-B) — the
-- outcome projector NEVER writes them, only status / node_slug / last_error.
--
-- `status` is a state machine (provisioning → active → scaling → draining → stopped,
-- plus `failed`), enforced by the same DB CHECK as `model_replicas`. `residency_zone`
-- is recorded for the Control-Plane read + audit; the HARD placement constraint rides
-- the seeded `stage_template` request token's `spec`. `last_actuated_at` anchors the
-- durable cooldown gate so flapping survives a mekhan restart.
--
-- NOTE: `datacenter_resource_id` here is the RESOLVED resource row UUID, whereas the
-- `NodePoolPolicy.datacenter_resource_id` config field is a String ALIAS the operator
-- types — the autoscaler resolves alias → uuid before the upsert.
CREATE TABLE node_replicas (
    id                     UUID        NOT NULL DEFAULT gen_random_uuid(),
    workspace_id           UUID        NOT NULL,
    pool_resource_id       UUID        NOT NULL,
    datacenter_resource_id UUID        NOT NULL,
    node_slug              TEXT,
    desired_nodes          INT         NOT NULL DEFAULT 0,
    observed_nodes         INT         NOT NULL DEFAULT 0,
    observed_slots         INT         NOT NULL DEFAULT 0,
    status                 TEXT        NOT NULL
        CHECK (status IN ('provisioning', 'active', 'scaling', 'draining', 'stopped', 'failed')),
    residency_zone         TEXT,
    last_error             TEXT,
    last_actuated_at       TIMESTAMPTZ,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (id),
    UNIQUE (pool_resource_id)
);

CREATE INDEX idx_node_replicas_workspace ON node_replicas (workspace_id);
