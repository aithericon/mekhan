-- Model-pool P1 (docs/29 §3): projection table for the loaded-state machine.
--
-- One row per (workspace, model_id). `state` is the operator-curated lifecycle
-- position (approved → loading → loaded → draining → unloaded). The state
-- machine is enforced in Rust (`ModelState::legal_transitions`), NOT a DB CHECK
-- — pre-production clean-change, no back-compat ceremony (no-back-compat).
--
-- This is a CONTROL/PROJECTION seam only: inference bypasses the engine Petri
-- net + the presence net, and P1 adds NO NATS subjects. The loaded-set read
-- AND-gates this `state == 'loaded'` against a live runner interface catalog
-- that advertises the model_id.
CREATE TABLE model_states (
    workspace_id         UUID        NOT NULL,
    registry_resource_id UUID,
    model_id             TEXT        NOT NULL,
    state                TEXT        NOT NULL,
    base                 TEXT,
    replicas             INT         NOT NULL DEFAULT 0,
    note                 TEXT,
    last_transition_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (workspace_id, model_id)
);

CREATE INDEX idx_model_states_workspace ON model_states (workspace_id);
