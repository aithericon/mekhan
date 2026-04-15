-- Store full event/token payloads for rich provenance detail rendering.
--
-- Before this migration, Mekhan's causality ingest only persisted identifiers
-- (net_id, event_seq, token_id, place_id) and discarded the full token colour
-- and effect_result JSON carried by DomainEvent messages. This made the
-- provenance event-detail sheet unable to show what any transition actually
-- produced, what any effect returned, or what payload any signal carried —
-- it had to fall back to placeholder text or "tokens only" rendering.
--
-- We capture the payloads here. All columns are nullable because existing
-- rows pre-date this migration.

ALTER TABLE causality_events
    ADD COLUMN effect_result      JSONB,
    ADD COLUMN bridge_target_net  TEXT,
    ADD COLUMN bridge_target_place TEXT;

-- One row per token participation. For produced/read roles the ingest can
-- populate `token_data` directly from the DomainEvent. For consumed roles
-- we store NULL and callers join back to the producer's row by token_id.
ALTER TABLE causality_event_tokens
    ADD COLUMN token_data JSONB;
