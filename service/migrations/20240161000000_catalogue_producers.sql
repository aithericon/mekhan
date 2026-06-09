-- Producer edges for the content-addressed catalogue.
--
-- The catalogue is content-addressed: `catalogue_entries` has one logical row
-- per `content_hash` (INSERT ... ON CONFLICT (content_hash) DO NOTHING in the
-- causality projector). That collapses PROVENANCE to the first run that
-- produced a given content: a re-run emitting byte-identical artifacts gets no
-- new catalogue row, so its `source_net`/`process_id`/`signal_key` are lost and
-- the re-run's instance/process view shows zero artifacts even though the
-- content is catalogued. Causality cross-links recover the link for at most one
-- run (whichever signal_key landed on the surviving row), not reliably.
--
-- This table records EVERY (content, producing-run) edge so any run can resolve
-- the content it produced, independent of which run "won" the catalogue row.
CREATE TABLE catalogue_producers (
    content_hash          TEXT NOT NULL,
    -- Producing net (the event's net_id, i.e. `mekhan-{instance_uuid}`). Always
    -- set on a fresh registration; the reliable join key for instance views.
    source_net            TEXT,
    execution_id          TEXT NOT NULL,
    job_id                TEXT,
    -- Resolved from causality at register time; may be NULL if tags lag.
    process_id            TEXT,
    process_step          TEXT,
    signal_key            TEXT,
    source_event_sequence BIGINT,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- One edge per (content, run). execution_id is unique per workflow run.
    PRIMARY KEY (content_hash, execution_id)
);

CREATE INDEX idx_cat_producers_source_net ON catalogue_producers (source_net);
CREATE INDEX idx_cat_producers_process ON catalogue_producers (process_id);
CREATE INDEX idx_cat_producers_hash ON catalogue_producers (content_hash);

-- Backfill the first/surviving producer recorded on each catalogue row.
INSERT INTO catalogue_producers
    (content_hash, source_net, execution_id, job_id, process_id,
     process_step, signal_key, source_event_sequence, created_at)
SELECT content_hash, source_net, execution_id, job_id, process_id,
       process_step, signal_key, source_event_sequence, created_at
FROM catalogue_entries
WHERE content_hash IS NOT NULL
  AND execution_id IS NOT NULL
  AND execution_id <> ''
ON CONFLICT (content_hash, execution_id) DO NOTHING;

-- Backfill any additional producer still recoverable from inventory provenance
-- (idempotent inventory upsert keeps only the latest writer's provenance, so
-- this recovers at most one extra run per content — best effort for pre-existing
-- data; new registrations record their edge directly via the projector).
INSERT INTO catalogue_producers (content_hash, source_net, execution_id, created_at)
SELECT content_hash,
       provenance->>'source_net',
       provenance->>'execution_id',
       now()
FROM file_inventory
WHERE content_hash IS NOT NULL
  AND COALESCE(provenance->>'execution_id', '') <> ''
ON CONFLICT (content_hash, execution_id) DO NOTHING;
