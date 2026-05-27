-- Scope templates to a workspace + add a visibility flag. `visibility=public`
-- is the cross-workspace read switch (used for demos today, for shared
-- templates later) — orthogonal to the workspace-membership ACL.
--
-- Backfill strategy: rows authored by the demo seeder (well-known author_id
-- from `service/src/demos.rs:494`) move into the demos workspace and become
-- public. Everything else lands in the default workspace.

ALTER TABLE workflow_templates
    ADD COLUMN workspace_id UUID
        NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000'
        REFERENCES workspaces(id),
    ADD COLUMN visibility   TEXT NOT NULL DEFAULT 'workspace'
        CHECK (visibility IN ('workspace','public'));

-- The DEFAULT above keeps unmigrated INSERT paths (the existing handlers
-- in step 1 of the workspace rollout, before step 4 wires workspace_id
-- through) writing into the default workspace. Once all INSERT call sites
-- pass workspace_id explicitly, the DEFAULT is harmless — keep it as a
-- safety net.

UPDATE workflow_templates
   SET workspace_id = '00000000-0000-0000-0000-0000000000de',
       visibility   = 'public'
 WHERE author_id = '00000000-0000-0000-0000-000000000aaa';

-- Hot path: list-templates filters by workspace AND is_latest. The partial
-- index keeps it tight without bloating the index for archived versions.
CREATE INDEX idx_templates_workspace_latest
    ON workflow_templates(workspace_id) WHERE is_latest;
