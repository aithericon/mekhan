-- Identity seam: a thin local mirror of the caller's human-readable identity
-- (email + display name), keyed by the same `subject_as_uuid()` value every
-- `workspace_members.user_id` and `roster_members.member_user_id` already use.
--
-- Why a separate table rather than columns on `workspace_members`? Membership
-- is per-(workspace,user); identity is per-user and shared across every
-- workspace + roster the principal belongs to. The auth extractor upserts this
-- row best-effort on each authenticated request, so member-admin and roster
-- listings can LEFT JOIN it to show "Dev User <dev@local>" instead of a raw
-- UUID — without a second IdP round-trip.
CREATE TABLE user_profiles (
    user_id      UUID         PRIMARY KEY,
    email        TEXT,
    display_name TEXT,
    updated_at   TIMESTAMPTZ  NOT NULL DEFAULT now()
);

-- Seed the dev-noop principal so `dev_noop` shows a name out of the box. The
-- UUID is `uuid_v5(SUBJECT_UUID_NAMESPACE, "dev-user")` — the exact literal
-- migration 20240123 seeded into `workspace_members` for the dev owner.
INSERT INTO user_profiles (user_id, email, display_name) VALUES
    ('3bb26085-29f3-5fbf-8a8c-a2e485a1f55b', 'dev@local', 'Dev User');
