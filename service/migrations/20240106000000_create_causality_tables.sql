-- Causality event log: one row per domain event that matters for lineage.
CREATE TABLE causality_events (
    net_id          TEXT        NOT NULL,
    event_seq       BIGINT      NOT NULL,
    event_type      TEXT        NOT NULL,
    transition_name TEXT,
    effect_handler  TEXT,
    timestamp       TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (net_id, event_seq)
);

-- Token roles within each causality event (consumed, produced, read).
CREATE TABLE causality_event_tokens (
    net_id      TEXT    NOT NULL,
    event_seq   BIGINT  NOT NULL,
    token_id    TEXT    NOT NULL,
    role        TEXT    NOT NULL,   -- 'consumed', 'produced', 'read'
    place_id    TEXT    NOT NULL,
    place_name  TEXT,
    FOREIGN KEY (net_id, event_seq) REFERENCES causality_events(net_id, event_seq)
);
CREATE INDEX idx_event_tokens_token ON causality_event_tokens(token_id);
CREATE INDEX idx_event_tokens_event ON causality_event_tokens(net_id, event_seq);
CREATE INDEX idx_event_tokens_role  ON causality_event_tokens(token_id, role);

-- Cross-net bridge links: correlate egress (bridge-out) with ingress (token-created with bridge_meta).
CREATE TABLE causality_cross_links (
    correlation_id  TEXT    NOT NULL,
    egress_net      TEXT,
    egress_seq      BIGINT,
    ingress_net     TEXT,
    ingress_seq     BIGINT,
    link_type       TEXT    NOT NULL,
    PRIMARY KEY (correlation_id)
);

-- Process tags: each token inherits the process_id(s) of the tokens that produced it.
-- Seed tokens self-tag (process_id = token_id).
CREATE TABLE causality_process_tags (
    token_id    TEXT    NOT NULL,
    process_id  TEXT    NOT NULL,
    PRIMARY KEY (token_id, process_id)
);
CREATE INDEX idx_process_tags_process ON causality_process_tags(process_id);
