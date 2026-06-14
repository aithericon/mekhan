-- Dev convenience seed: a SECOND workspace (org) + a SECOND dev user, so
-- `dev_noop` can demonstrate cross-tenant isolation by switching both the
-- active workspace AND the acting user without a real IdP.
--
-- Mirrors the existing dev seeds (migration 20240123 seeds the `default`
-- workspace + dev-user; 20240162 seeds the dev-user profile). Inert in a real
-- BFF/Zitadel deployment: the workspace carries no `zitadel_org_id`, and no
-- Zitadel principal ever resolves to `dev-user-2`, so this is orphan dev data
-- there, exactly like the pre-existing `dev-user`/`default` seed.
--
-- Topology (matches the "U1 in both orgs, U2 in org-2 only" choice):
--   dev-user    (3bb26085-…f55b) → owner of `default` (seeded in 20240123)
--                                 + owner of `acme-labs` (here)  → can switch orgs
--   dev-user-2  (2141c005-…373c) → owner of `acme-labs` ONLY     → isolated tenant
--
-- IDs are stable sentinels:
--   acme-labs workspace = 00000000-0000-0000-0000-000000000002
--   dev-user-2 user_id  = uuid_v5(SUBJECT_UUID_NAMESPACE, "dev-user-2")
--                       = 2141c005-6494-5bfa-b67d-7ca77f5f373c
-- (the same v5 derivation `AuthUser::subject_as_uuid()` applies to "dev-user",
--  which is why the dev-user literal below matches migration 20240123/20240162).

-- Second workspace (org). Not a system workspace — a normal tenant the dev
-- user(s) act in. Idempotent.
INSERT INTO workspaces (id, slug, display_name, is_system)
VALUES ('00000000-0000-0000-0000-000000000002', 'acme-labs', 'Acme Labs', FALSE)
ON CONFLICT (id) DO NOTHING;

-- Second dev user profile.
INSERT INTO user_profiles (user_id, email, display_name)
VALUES ('2141c005-6494-5bfa-b67d-7ca77f5f373c', 'dev2@local', 'Dev User Two')
ON CONFLICT (user_id) DO NOTHING;

-- Memberships:
--   dev-user   → owner of acme-labs (so the primary dev user has a second org
--                to switch between).
--   dev-user-2 → owner of acme-labs only (a cleanly isolated tenant; NOT a
--                member of `default`, so switching to it shows an isolated view).
INSERT INTO workspace_members (workspace_id, user_id, role)
VALUES
    ('00000000-0000-0000-0000-000000000002', '3bb26085-29f3-5fbf-8a8c-a2e485a1f55b', 'owner'),
    ('00000000-0000-0000-0000-000000000002', '2141c005-6494-5bfa-b67d-7ca77f5f373c', 'owner')
ON CONFLICT (workspace_id, user_id) DO NOTHING;
