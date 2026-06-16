-- Demote the nil/`default` workspace to an internals/legacy-only system workspace.
--
-- Up to now `default` (id = Uuid::nil(), all zeros) was a normal, user-facing
-- tenant: the dev-noop user owned it, and the resolver auto-enrolled fresh
-- principals into it (`ensure_default_workspace_membership`). That made `nil`
-- a shared catch-all tenant — the opposite of the per-tenant isolation the
-- platform now wants.
--
-- After this migration the platform model is:
--   * Every NEW user gets their OWN personal workspace, minted lazily by the
--     resolver on first login (see `auth/resolver.rs::ensure_personal_workspace`,
--     landed in a later step of this phase). The resolver runs that provisioning
--     BEFORE any handler, so a principal with zero non-system memberships always
--     ends up with a home — demoting `default` here strands no one.
--   * `default` / nil becomes `is_system = TRUE`: reserved for internals and
--     legacy data (resources/instances that still carry `workspace_id = nil`,
--     and the engine's "default" string routing sentinel). System workspaces are
--     non-deletable and are not auto-joined by ordinary users.
--
-- NOTE ON EXISTING nil-ONLY MEMBERS — NO EAGER BACKFILL HERE.
-- We deliberately do NOT loop existing `workspace_members` of nil and mint a
-- personal workspace per row in SQL: personal-workspace slugs are derived from
-- email/display-name with `-{n}` collision retry (resolver logic), and the
-- `workspaces.slug` UNIQUE constraint makes that derivation unsafe to replicate
-- in a one-shot migration. Instead, any existing principal whose only membership
-- was nil gets a personal workspace lazily on their NEXT login, via the resolver
-- (the "zero non-system memberships" guard fires exactly once). Their old nil
-- data stays reachable through the active-workspace cookie, which does not filter
-- on `is_system`. The one principal we seed eagerly below is the dev-noop user,
-- because dev_noop has no real IdP/login to trigger the lazy path.

-- 1. Seed the dev-noop user's personal workspace, mirroring the dev seeds in
--    20240123 (default + dev-user owner) and 20240184 (acme-labs + dev-user-2).
--    Stable sentinel id `…0001`; dev-user's user_id is
--    uuid_v5(SUBJECT_UUID_NAMESPACE, "dev-user") = 3bb26085-…f55b — the same
--    derivation `AuthUser::subject_as_uuid()` used in 20240123/20240184.
--    Not a system workspace: it's the dev user's normal personal tenant. The
--    dev_noop roster is repointed at this id in a later step of this phase.
--    Idempotent.
INSERT INTO workspaces (id, slug, display_name, is_system)
VALUES ('00000000-0000-0000-0000-000000000001', 'dev-user', 'Dev User', FALSE)
ON CONFLICT (id) DO NOTHING;

INSERT INTO workspace_members (workspace_id, user_id, role)
VALUES
    ('00000000-0000-0000-0000-000000000001',
     '3bb26085-29f3-5fbf-8a8c-a2e485a1f55b',
     'owner')
ON CONFLICT (workspace_id, user_id) DO NOTHING;

-- 2. Demote nil/`default` to a system workspace (internals/legacy only).
UPDATE workspaces SET is_system = TRUE WHERE id = '00000000-0000-0000-0000-000000000000';
