-- Re-scope the internal LLM model pool resources into the platform tier
-- (mekhan workflow ownership migration, Phase 1).
--
-- `internal_pool_router` (`internal_llm`) and `internal_pool_registry`
-- (`model_registry`) are the two resources every model-pool / LLM workflow
-- binds to. They were historically seeded as WORKSPACE-scoped resources in the
-- `demos` system workspace (`00000000-0000-0000-0000-0000000000de`). But the
-- internal pool is a GLOBAL inference data plane (the inference HTTP router +
-- competing-consumer queue are already cluster-wide), so these control-plane
-- resources belong to the synthetic platform scope
-- `PLATFORM_SCOPE_ID` (= '00000000-0000-0000-0000-0000506c6174', the trailing
-- bytes spell `Platr`). The resource resolver widens to
-- `WHERE (workspace_id = $caller OR scope_kind = 'platform')`
-- (`process/discover.rs`), so a platform-tier router/registry resolves for a
-- workflow compiled in ANY workspace — the dev-user workspace, the prod
-- service-user workspace, `demos`, everywhere — while a tenant row of the same
-- `path` deterministically shadows the platform default.
--
-- We UPDATE-rescope (not delete+recreate) so `resources.id` is PRESERVED:
-- `model_states.registry_resource_id` points at the registry row's id and frozen
-- `workflow_instances.resource_pins` reference resource ids — both survive a
-- re-scope but would break under delete+recreate.
--
-- Idempotent: re-running matches nothing because the moved rows no longer
-- satisfy the `scope_kind = 'workspace' AND workspace_id = demos` predicate (the
-- platform workspace_id ≠ any tenant). The boot re-seed
-- (`internal_pool::ensure_platform_internal_pool`) is likewise idempotent — it
-- resolves the platform row first and only creates one when absent.
--
-- Constraints satisfied: the target coordinate
-- ('platform', PLATFORM_SCOPE_ID, <path>) is unoccupied (no live or soft-deleted
-- platform row of these paths exists), so neither the partial
-- UNIQUE (scope_kind, scope_id, path) WHERE deleted_at IS NULL nor the legacy
-- non-partial UNIQUE (workspace_id, path) is violated; the platform anchor
-- workspace row (id = PLATFORM_SCOPE_ID) exists so resources_workspace_fk holds.

UPDATE resources
   SET workspace_id = '00000000-0000-0000-0000-0000506c6174',
       scope_kind   = 'platform',
       scope_id     = '00000000-0000-0000-0000-0000506c6174'
 WHERE path IN ('internal_pool_router', 'internal_pool_registry')
   AND scope_kind = 'workspace'
   AND workspace_id = '00000000-0000-0000-0000-0000000000de'
   AND deleted_at IS NULL;
