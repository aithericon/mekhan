-- Model-pool reconciliation (docs/31): idle-eviction (sleep/wake) plumbing.
--
-- Loop 2 of the node-fleet reconciler can put an `active` model replica to SLEEP
-- when it has been idle past the policy's idle window — freeing the per-node
-- concurrency budget `C` without tearing the replica down — and WAKE it on the
-- next routed request. Two schema changes back this:
--
--   1. `model_states.idle_evict` — operator opt-in (per-model policy flag). When
--      FALSE (the default) the model is pinned hot and the reconciler never sleeps
--      it; when TRUE it is eligible for idle eviction.
--
--   2. A new `sleeping` terminalish status on `model_replicas` — a replica that has
--      been idle-evicted: still tracked, holds NO live `C` slot, re-activates on
--      wake. The original inline CHECK in `20240146000000_model_replicas.sql` is
--      PG-auto-named `model_replicas_status_check`; widen it in place (drop + re-add)
--      following the no-back-compat convention (no data migration needed — existing
--      rows already satisfy the wider set).
ALTER TABLE model_states
    ADD COLUMN idle_evict BOOLEAN NOT NULL DEFAULT FALSE;

ALTER TABLE model_replicas
    DROP CONSTRAINT model_replicas_status_check;

ALTER TABLE model_replicas
    ADD CONSTRAINT model_replicas_status_check
        CHECK (status IN ('provisioning', 'active', 'scaling', 'draining', 'stopped', 'failed', 'sleeping'));
