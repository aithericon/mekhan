-- Re-home seeded demos from the system `demos` workspace into `default`.
--
-- Demos were originally seeded into the system-owned `demos` workspace
-- (…00de). That made them visible to everyone (rows are `visibility=public`,
-- read via the cross-workspace public-read branch) but NOT editable: the write
-- gate (`gate_template_write` → `require_role(Editor)`) checks membership of
-- the template's workspace, and neither the dev-noop user nor BFF users are
-- members of the system `demos` workspace — so every publish/edit of a seeded
-- demo 403'd. The seeder now targets the default workspace (`demos.rs`,
-- `DEMO_WORKSPACE_ID = Uuid::nil()`), but the seeder is idempotent and won't
-- re-home rows that already exist — including on already-deployed instances.
--
-- This migration moves them. The dev-noop user owns `default` (migration
-- 20240123) and the BFF resolver auto-provisions every authenticated user as an
-- editor of it (`ensure_default_workspace_membership`), so once demos live here
-- they become first-class editable starting points. `workspace_id` is purely
-- the ACL pointer — S3 files, Y.Doc, and triggers are keyed by template_id, so
-- moving the row alone is sufficient. Rows keep `visibility=public`, preserving
-- discovery for users whose active workspace is some other tenant.
--
-- Idempotent: re-running is a no-op once the `demos` workspace is drained. The
-- system `demos` workspace row itself is left in place (harmless, unreferenced).
UPDATE workflow_templates
SET workspace_id = '00000000-0000-0000-0000-000000000000'
WHERE workspace_id = '00000000-0000-0000-0000-0000000000de';
