-- Fixture table for the 19-postgres-node demo.
--
-- A dedicated schema (demo_pg) so the demo NEVER reads or writes mekhan's own
-- application tables. Apply once against the dev Postgres before publishing the
-- demo:
--
--   just dev pg-demo-seed
--     (= psql postgres://mekhan:mekhan@localhost:15439/mekhan -f demos/19-postgres-node/seed.sql)
--
-- Idempotent: re-running truncates + re-seeds the three baseline rows so the
-- demo's READ assertions are stable, and resets the id sequence so the WRITE
-- step's RETURNING id is deterministic on a fresh seed.

CREATE SCHEMA IF NOT EXISTS demo_pg;

CREATE TABLE IF NOT EXISTS demo_pg.widgets (
    id   bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL
);

-- Reset to a known baseline (idempotent re-seed).
TRUNCATE demo_pg.widgets RESTART IDENTITY;

INSERT INTO demo_pg.widgets (name) VALUES
    ('alpha'),
    ('bravo'),
    ('charlie');
