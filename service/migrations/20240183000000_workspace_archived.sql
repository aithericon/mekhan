-- Soft-delete (archive) for workspaces.
--
-- Deleting a tenant is the most destructive control-plane action: a workspace
-- is a complete isolation boundary, so a hard purge would have to cascade
-- across templates, instances, folders, resources/assets, catalogue + file
-- inventory, Vault secrets, webhook triggers and Yjs rooms. We don't do that
-- here. Archiving sets `archived_at` and nothing else — every row survives for
-- audit / recovery. An archived workspace simply disappears from the tenant
-- picker, the membership listing, and auth resolution (see `list_workspaces`,
-- `membership_workspace`, `apply_override`). A separate explicit hard-purge
-- operation can be layered on later.
--
-- System workspaces (`is_system`) and the seeded `default` can never be
-- archived — enforced in the `delete_workspace` handler, not by a constraint,
-- so recovery (clearing `archived_at`) stays a plain UPDATE.
ALTER TABLE workspaces ADD COLUMN archived_at TIMESTAMPTZ;

-- The hot path is "live workspaces only" (picker, resolver). A partial index
-- keeps those scans off the archived rows.
CREATE INDEX idx_workspaces_live ON workspaces (id) WHERE archived_at IS NULL;
