-- Unified projection of resource allocations — both datacenter (Slurm/Nomad/HTTP)
-- LEASE/SUBMIT grants and token_pool admission grants — materialized by the
-- allocations consumer from `petri.events.>` on the PETRI_GLOBAL JetStream
-- stream plus the accounting signals the engine emits for held allocations.
--
-- One row per `(net_id, grant_id, kind)`: the engine `grant_id` is the lease
-- grant key `instance_id:node_id` and doubles as the accounting `signal_key`.
-- For datacenter leases `kind = 'datacenter_lease'`; for our own worker-pool
-- admission grants `kind = 'token_pool_grant'`.
--
-- The control-plane allocations view reads this table to show per-cluster
-- queue waits, placements, exit codes, and TRES accounting; the upsert is
-- sequence-guarded like `step_execution` (replayed events with
-- sequence <= last_sequence are no-ops).
CREATE TABLE allocations (
    -- Deterministic identity: uuid_v5 of (net_id, grant_id, kind). The
    -- (net_id, grant_id, kind) unique business key is what the consumer
    -- upserts on.
    id                  UUID        NOT NULL DEFAULT gen_random_uuid(),

    -- 'datacenter_lease' | 'token_pool_grant'
    kind                TEXT        NOT NULL CHECK (kind IN ('datacenter_lease','token_pool_grant')),

    net_id              TEXT        NOT NULL,

    -- Resolved owning instance; NULL for pool-management nets.
    instance_id         UUID,

    -- Workflow node / LeaseScope container id. NULL when not node-scoped.
    node_id             TEXT,

    -- Engine grant key (instance_id:node_id); also the accounting signal_key.
    grant_id            TEXT        NOT NULL,

    -- Datacenter resource; NULL for token_pool_grant.
    cluster_resource_id UUID,

    -- 'slurm' | 'nomad' | 'http'; NULL for pool.
    scheduler_flavor    TEXT,

    -- Slurm jobid / Nomad dispatched job id.
    alloc_id            TEXT,

    -- Placement host.
    node                TEXT,

    -- lease-<sanitized grant_id>.
    executor_namespace  TEXT,

    -- 'pending' | 'held' | 'released' | 'failed' | 'expired'
    status              TEXT        NOT NULL CHECK (status IN ('pending','held','released','failed','expired')),

    requested_at        TIMESTAMPTZ,
    acquired_at         TIMESTAMPTZ,
    released_at         TIMESTAMPTZ,
    expiry              TIMESTAMPTZ,

    exit_code           INT,

    queue_wait_ms       BIGINT,
    elapsed_ms          BIGINT,

    -- Stored as rounded whole seconds (payload float -> round -> i64).
    cpu_seconds         BIGINT,
    gpu_seconds         BIGINT,
    peak_rss_bytes      BIGINT,

    requested_tres      JSONB,
    allocated_tres      JSONB,

    last_error          TEXT,

    -- Engine event sequence number of the last event folded into this row.
    -- Idempotency cursor: replayed events with sequence <= last_sequence are
    -- no-ops at upsert time.
    last_sequence       BIGINT      NOT NULL DEFAULT 0,

    UNIQUE (net_id, grant_id, kind)
);

-- Per-cluster allocations view, ordered by acquisition time.
CREATE INDEX allocations_cluster_acquired_idx
    ON allocations (cluster_resource_id, acquired_at);

-- Instance-scoped lookup for the instance allocations overlay.
CREATE INDEX allocations_instance_idx
    ON allocations (instance_id);

-- "What is currently pending/held" filter.
CREATE INDEX allocations_status_idx
    ON allocations (status);
