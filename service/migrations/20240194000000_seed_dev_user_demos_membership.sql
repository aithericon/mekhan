-- Seed the dev-noop user as an owner of the system-owned `demos` workspace.
--
-- Until now the demos workspace (id `…00de`, seeded in 20240123) had only one
-- member: the synthetic demo *seeder* principal (`…0aaa`), added at runtime by
-- `demos::ensure_seeder_workspace_membership` so the publish-time resource
-- resolver can read demo-referenced resources. The dev-noop user
-- (`3bb26085-…f55b`) was never a member — it could only *visit* demos read-only
-- via the `is_system` browsing path (`auth/active_workspace.rs`), with no
-- membership row, so demos never appeared as a first-class owned tenant in its
-- switcher and it could not pass the `editor`-of-demos write gate
-- (`handlers/demos.rs`).
--
-- This mirrors the other dev-user seeds — 20240123 (default), 20240184
-- (acme-labs / dev-user-2), 20240189 (dev-user personal) — giving the dev
-- identity an explicit `owner` row in demos, consistent with it owning every
-- other workspace it belongs to. dev-user's user_id is
-- `uuid_v5(SUBJECT_UUID_NAMESPACE, "dev-user")` = 3bb26085-…f55b, the same
-- derivation `AuthUser::subject_as_uuid()` uses. Idempotent.
INSERT INTO workspace_members (workspace_id, user_id, role)
VALUES
    ('00000000-0000-0000-0000-0000000000de',
     '3bb26085-29f3-5fbf-8a8c-a2e485a1f55b',
     'owner')
ON CONFLICT (workspace_id, user_id) DO NOTHING;
