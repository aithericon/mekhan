-- Attach the resources.workspace_id column (already NOT NULL since the
-- 20240120 migration) to the workspaces table via FK. No data migration is
-- needed: existing rows carry `Uuid::nil()` which is exactly the seeded
-- default workspace's id (chosen for this reason in 20240123). Vault paths
-- stored in `resource_versions.vault_path` therefore remain valid as-is.
--
-- The FK is added separately from the create-resources migration to keep
-- 20240120 history immutable and the workspace-introduction migration self-
-- contained.

ALTER TABLE resources
    ADD CONSTRAINT resources_workspace_fk
    FOREIGN KEY (workspace_id) REFERENCES workspaces(id);
