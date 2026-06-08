-- Model-pool load-time instrumentation: measure how long a model takes to load.
--
-- Nothing today measures model COLD-START / load latency. The placement
-- controller (`service/src/autoscaler/placement.rs`) now stamps these columns on
-- the per-(workspace, model) `model_replicas` row so cold-start scenarios have
-- real data:
--
--   * `load_started_at`       — set to NOW() when the controller publishes a COLD
--     `LoadBase` (a base that was NOT already resident on the target runner, i.e.
--     loaded from `pulled`, not a warm wake of a resident/slept base). Stamped
--     ONLY when currently NULL, so an in-flight measurement is never reset.
--   * `load_finished_at`      — set to NOW() on the later reconcile where the base
--     is observed resident on a runner AND `load_started_at` was set.
--   * `last_load_duration_ms` — `load_finished_at - load_started_at` in ms,
--     computed in SQL at finish; `load_started_at` is then CLEARED back to NULL so
--     the next cold load re-measures.
--
-- No back-compat concerns (pre-production): all three default NULL.
ALTER TABLE model_replicas
    ADD COLUMN IF NOT EXISTS load_started_at       TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS load_finished_at      TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS last_load_duration_ms BIGINT;
