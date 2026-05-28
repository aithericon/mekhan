-- Per-step projection of the engine event log. One row per
-- `(instance, template node, iteration)` materialized by the step-executions
-- consumer (`service/src/projections/step_executions/consumer.rs`) from
-- `petri.events.>` on the PETRI_GLOBAL JetStream stream.
--
-- The instance view reads this table to overlay per-step inputs/outputs/
-- duration/status onto the workflow canvas; the planned template-level
-- aggregation view will aggregate across rows by (template_id,
-- template_version, node_id).
--
-- See `service/src/compiler/interface.rs` (NodeInterface) for the source of
-- attribution: the consumer reverse-indexes `owned_transitions` →
-- `node_id` and `owned_places` → `node_id`, then folds `TransitionFired`
-- / `EffectCompleted` / `EffectFailed` payloads into these rows.
CREATE TABLE step_execution (
    instance_id        UUID        NOT NULL REFERENCES workflow_instances(id) ON DELETE CASCADE,
    node_id            TEXT        NOT NULL,
    iteration_index    INT         NOT NULL DEFAULT 0,

    template_id        UUID        NOT NULL,
    template_version   INT         NOT NULL,
    node_kind          TEXT        NOT NULL,

    -- pending: not yet entered. running: first owning-transition fired.
    -- completed: data_port deposit or terminal output reached.
    -- failed: EffectFailed for this node. skipped: instance terminated
    -- without entering the node.
    status             TEXT        NOT NULL CHECK (status IN ('pending','running','completed','failed','skipped')),

    -- { "<producer_node_id>": <envelope>, ... } grouped by upstream owner of
    -- the read-arc places. Populated from TransitionFired.read_tokens.
    inputs             JSONB,

    -- The envelope deposited at NodeInterface.data_port (or workflow_terminals
    -- for End nodes). Populated from TransitionFired.produced_tokens filtered
    -- to the node's own boundary places.
    outputs            JSONB,

    -- Decision nodes: the OutputKey::Edge(edge_id) of the output that
    -- received the token. NULL for non-Decision nodes.
    branch_taken       TEXT,

    started_at         TIMESTAMPTZ,
    completed_at       TIMESTAMPTZ,

    -- EffectFailed payload (error_message, retryable, input_data,
    -- tokens_consumed) for failed steps. NULL otherwise.
    error              JSONB,

    -- Engine event sequence number of the last event folded into this row.
    -- Idempotency cursor: replayed events with sequence <= last_sequence are
    -- no-ops at upsert time.
    last_sequence      BIGINT      NOT NULL,

    created_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    PRIMARY KEY (instance_id, node_id, iteration_index)
);

-- Instance view query: SELECT * FROM step_execution WHERE instance_id = $1
-- ORDER BY started_at NULLS LAST, node_id, iteration_index. Already covered
-- by the PK for filter; an explicit ordering index isn't needed at this scale.

-- Template-level aggregate query (deferred view): aggregate counts/durations
-- by (template_id, template_version, node_id) across all instances.
CREATE INDEX step_execution_template_node_idx
    ON step_execution (template_id, template_version, node_id);

-- Useful for "how many steps are currently running in this instance" queries
-- and for the instance-view "still pending" filter.
CREATE INDEX step_execution_instance_status_idx
    ON step_execution (instance_id, status);
