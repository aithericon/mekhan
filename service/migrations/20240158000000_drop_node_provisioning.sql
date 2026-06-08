-- Drop the Nomad node-provisioning subsystem from the LLM model pool.
--
-- The model-pool autoscaler used to provision generic vLLM *nodes* via Nomad
-- (Loop 1: a `node_pool` resource → `node_replicas` reconciliation → a
-- `stage_template` Nomad service job), plus a per-model `dedicated` fallback
-- that spun up a single-model Nomad job. That whole path is removed: autoscaling
-- now targets WHICH models are loaded and how they are spread across the
-- already-registered LLM runners (the placement controller publishing NATS
-- load/unload), never node provisioning.
--
-- Pre-production, no-back-compat: this is destructive. `just dev reset` re-applies
-- the chain clean; any dev node_pool / node_replicas rows are discarded by design.

-- The per-model autoscale policy (folded onto `model_states`) loses its pool
-- reference and the dedicated-job escape hatch.
ALTER TABLE model_states
    DROP COLUMN IF EXISTS node_pool,
    DROP COLUMN IF EXISTS dedicated;

-- The placement reconciliation/status row stays (it anchors the idle-evict
-- cooldown + feeds the Set-tab badges) but loses the Nomad-only columns: a
-- placed model is loaded onto a registered runner, not a Nomad allocation, so
-- there is no datacenter to provision on and no Nomad service-job slug to name.
ALTER TABLE model_replicas
    DROP COLUMN IF EXISTS datacenter_resource_id,
    DROP COLUMN IF EXISTS replica_slug;

-- The node-fleet reconciliation table (Loop 1) is gone entirely.
DROP TABLE IF EXISTS node_replicas;
