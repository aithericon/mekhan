-- FK anchor for the platform scope (ScopeKind::Platform).
--
-- Platform-scoped resources store `workspace_id = PLATFORM_SCOPE_ID`
-- ('00000000-0000-0000-0000-0000506c6174') so that tenant
-- `WHERE workspace_id = $tenant` queries naturally exclude them (the sentinel
-- equals no real tenant id). But `resources.workspace_id` — and the parallel
-- columns on workflow_templates / folders / pages / library_packs — carry a
-- foreign key to `workspaces(id)`. So PLATFORM_SCOPE_ID must EXIST as a row, or
-- creating any platform-owned resource fails `resources_workspace_fk` (observed
-- as the platform worker-group / model-serving-group boot seeders 500ing).
--
-- This row is a RESERVED, INTERNAL anchor — NOT a tenant workspace:
--   * `is_system = TRUE` — never auto-deleted, and the resolver no longer
--     auto-enrolls anyone into system workspaces (Phase 3), so no user ever
--     becomes a member or has it as their active workspace.
--   * All platform behavior is driven by `scope_kind = 'platform'` + the global
--     `is_platform_admin` capability + global visibility — NOT by membership in
--     this row. The row exists ONLY to satisfy referential integrity for
--     platform-owned rows.
--
-- Idempotent: ON CONFLICT DO NOTHING. Must precede the boot seeders
-- (ensure_platform_default_worker_group / ensure_platform_model_serving_group),
-- which it does — all migrations run before any seeder.
INSERT INTO workspaces (id, slug, display_name, is_system)
VALUES ('00000000-0000-0000-0000-0000506c6174', 'platform', 'Platform (shared)', TRUE)
ON CONFLICT (id) DO NOTHING;
