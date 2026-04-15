-- Signal lineage tables (ADR-19): track signal injections back to their dispatch events.
--
-- The existing `causality_cross_links` table is constrained to one row per
-- signal_key, which works for catalogue subscriptions, bridges, and 1:1
-- effect→signal flows. It does NOT work for the executor lifecycle, where:
--   * One executor_submit dispatch generates ONE signal_key
--   * The same signal_key flows through MANY status/event signals (sig_accepted,
--     sig_running, sig_log×N, sig_metric×N, sig_artifact, sig_completed, …)
--   * Downstream effects (e.g. catalogue_register) reuse the same signal_key
--
-- The two tables below decouple "where did this signal come from" (1:1 dispatch
-- per key) from "what tokens did it inject" (1:N lineage per key).

-- One row per signal_key, recording the FIRST EffectCompleted that emitted it.
-- ON CONFLICT DO NOTHING preserves the original dispatcher even when a later
-- effect (e.g. catalogue_register) reuses the same signal_key.
CREATE TABLE causality_signal_dispatches (
    signal_key   TEXT    NOT NULL PRIMARY KEY,
    dispatch_net TEXT    NOT NULL,
    dispatch_seq BIGINT  NOT NULL,
    recorded_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- One row per signal-injected TokenCreated event, pointing back to the
-- dispatching event. Read by the provenance CTE (Path 4) to walk from a
-- TokenCreated produced by signal injection back to the originating effect.
CREATE TABLE causality_signal_lineage (
    ingress_net  TEXT    NOT NULL,
    ingress_seq  BIGINT  NOT NULL,
    dispatch_net TEXT    NOT NULL,
    dispatch_seq BIGINT  NOT NULL,
    signal_key   TEXT    NOT NULL,
    PRIMARY KEY (ingress_net, ingress_seq)
);

CREATE INDEX idx_signal_lineage_dispatch ON causality_signal_lineage (dispatch_net, dispatch_seq);
CREATE INDEX idx_signal_lineage_signal_key ON causality_signal_lineage (signal_key);
