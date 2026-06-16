-- Relocate the shared internal LLM model pool into the platform tier
-- (platform-tier rework, plan Phase 2).
--
-- The internal model pool is a GLOBAL inference data plane (the inference HTTP
-- router + competing-consumer queue are already cluster-wide), but its
-- control-plane projection rows (`model_states` and their reconciliation rows
-- in `model_replicas`) were historically seeded under the nil/`default`
-- workspace (`Uuid::nil()`). They now belong to the synthetic platform scope
-- `PLATFORM_SCOPE_ID` (= '00000000-0000-0000-0000-0000506c6174', the trailing
-- bytes spell `Platr`), so every tenant's picker — which widens to
-- `workspace_id IN ($caller, PLATFORM_SCOPE_ID)` — surfaces them globally while
-- a tenant row of the same `model_id` deterministically shadows the platform
-- default.
--
-- We move ONLY the known internal-pool model ids (the `internal_pool_registry`
-- fixtures in `demos/model_states/*.json`), so a genuine tenant-curated model
-- that happens to live in the nil workspace is NOT swept into the platform tier.
--
-- Idempotent: re-running this migration matches nothing because the moved rows
-- no longer satisfy the `workspace_id = nil` predicate (and the platform
-- workspace_id ≠ any tenant). The boot re-seed is likewise idempotent — the
-- seeders bind `PLATFORM_SCOPE_ID` directly (`demos::seed_model_states`,
-- `ensure_platform_default_worker_group`, `ensure_platform_model_serving_group`)
-- and upsert `ON CONFLICT (workspace_id, model_id) DO UPDATE`.

-- 1) Move the per-model config/state row (carries the folded autoscale-policy
--    columns from 20240152) from nil → platform. `model_states` is keyed
--    (workspace_id, model_id); the destination workspace_id changes, the
--    model_id is preserved.
UPDATE model_states
   SET workspace_id = '00000000-0000-0000-0000-0000506c6174'
 WHERE workspace_id = '00000000-0000-0000-0000-000000000000'
   AND model_id IN ('llama3.2:1b', 'qwen3.5:9b', 'llama3.2-vision:11b');

-- 2) Move the matching autoscaler reconciliation rows. Post-20240152
--    `model_replicas` is re-keyed UNIQUE (workspace_id, model_id) — one row per
--    (workspace, model) — so the same model-id predicate carries the
--    reconciliation state across with no risk of colliding the unique key (the
--    platform side held no internal-pool rows before this migration).
UPDATE model_replicas
   SET workspace_id = '00000000-0000-0000-0000-0000506c6174'
 WHERE workspace_id = '00000000-0000-0000-0000-000000000000'
   AND model_id IN ('llama3.2:1b', 'qwen3.5:9b', 'llama3.2-vision:11b');

-- NOTE: the per-workspace `default` (worker) and `model_serving` (instrument)
-- `capacity` resources created by the OLD all-workspaces seeders
-- (`ensure_default_worker_group_all_workspaces` /
-- `ensure_model_serving_group_all_workspaces`, now replaced by the single
-- platform-tier `ensure_platform_*` seeders) are deliberately LEFT IN PLACE
-- here. They are NOT hard-deleted: a worker or serving runner may be bound to
-- one mid-flight, and tearing the routing-partition resource out from under an
-- in-flight competing-consumer subscription would strand its queue. A follow-up
-- migration soft-deletes them once the platform pool is the sole live routing
-- target. (There is no separate `model_states_policy` TABLE to migrate — the
-- autoscale policy was folded into nullable columns ON `model_states` by
-- 20240152, so it moves with the row updated in step 1.)
