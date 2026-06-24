-- Email-keyed identity seam.
--
-- Until now a principal's mekhan `user_id` was a one-way `uuid_v5(NAMESPACE,
-- oidc_sub)` hash (`AuthUser::subject_as_uuid()`), so the same human logging in
-- through a different IdP subject (a re-provisioned Zitadel user, a future
-- second provider) became a brand-new, disconnected identity — losing every
-- grant, membership, and authorship row. This migration introduces a real
-- identity spine:
--
--   * `users`           — one row per human, keyed by a stable mekhan `id`,
--                         with a case-insensitive UNIQUE `email` so the same
--                         person reconciles across providers/subjects by their
--                         verified email.
--   * `user_identities` — the (provider, subject) → user_id links. A user can
--                         accumulate several identities (multiple Zitadel subs,
--                         a second IdP) all pointing at one `users.id`.
--
-- The resolver (`auth/resolver.rs::resolve_user_id`) is the writer: it looks up
-- the identity first, falls back to verified-email reconciliation, and finally
-- mints a new user keyed by the LEGACY v5 hash of the subject so pre-existing
-- `workflow_instances.created_by` / `workspace_members.user_id` / grant rows
-- keep resolving to the same id for already-seen subjects.
--
-- `auth_tokens.subject` stays the raw OIDC subject (NOT rekeyed). `user_profiles`
-- is intentionally left intact here — a later phase folds it into `users`.

CREATE EXTENSION IF NOT EXISTS citext;

CREATE TABLE users (
    id           UUID         PRIMARY KEY,
    -- Case-insensitive so `Alice@Corp` and `alice@corp` are one identity. The
    -- UNIQUE is what makes verified-email reconciliation safe. NULL allowed:
    -- machine principals (runners/workers) and ambiguous backfill rows carry no
    -- email and are keyed only by `id`.
    email        CITEXT       UNIQUE,
    display_name TEXT,
    avatar_url   TEXT,
    -- 'active' for a fully-provisioned human; 'invited' reserved for a user
    -- pre-created by an invite that has not yet been claimed by a login.
    status       TEXT         NOT NULL DEFAULT 'active',
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE TABLE user_identities (
    provider       TEXT        NOT NULL,
    subject        TEXT        NOT NULL,
    user_id        UUID        NOT NULL REFERENCES users(id),
    email_verified BOOLEAN     NOT NULL DEFAULT false,
    linked_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, subject)
);

CREATE INDEX idx_user_identities_user ON user_identities(user_id);

-- Backfill `users` from the existing identity surfaces so every id already in
-- use as a grant/membership/authorship key has a `users` row to resolve to.
--
-- Source 1: `user_profiles` — the richest source (email + display_name +
-- avatar_url). `user_profiles.email` is plain TEXT with no uniqueness, so it may
-- carry duplicate / case-colliding values that would violate the CITEXT UNIQUE
-- on `users.email`. Dedupe to ONE winning row per `lower(email)` (preferring a
-- non-null display_name, then the most recently updated), and NULL out the email
-- on every non-winning row so they still get a `users` row keyed by their id —
-- just without an email handle (they remain reconcilable later, manually).
WITH ranked AS (
    SELECT
        up.user_id,
        up.email,
        up.display_name,
        up.avatar_url,
        CASE
            WHEN up.email IS NULL OR btrim(up.email) = '' THEN NULL
            ELSE lower(btrim(up.email))
        END AS email_key,
        ROW_NUMBER() OVER (
            PARTITION BY CASE
                WHEN up.email IS NULL OR btrim(up.email) = '' THEN NULL
                ELSE lower(btrim(up.email))
            END
            ORDER BY
                (up.display_name IS NOT NULL) DESC,
                up.updated_at DESC,
                up.user_id ASC
        ) AS rn
    FROM user_profiles up
)
INSERT INTO users (id, email, display_name, avatar_url, status)
SELECT
    r.user_id,
    -- Only the winning row per email_key keeps the email; NULL email rows and
    -- non-winning duplicates insert with email NULL to honour the UNIQUE.
    CASE WHEN r.email_key IS NOT NULL AND r.rn = 1 THEN r.email ELSE NULL END,
    r.display_name,
    r.avatar_url,
    'active'
FROM ranked r
ON CONFLICT (id) DO NOTHING;

-- Source 2: any `workspace_members.user_id` lacking a profile row (machine
-- principals, or ids that pre-date the profile mirror). Email NULL.
INSERT INTO users (id, email, display_name, avatar_url, status)
SELECT DISTINCT m.user_id, NULL, NULL, NULL, 'active'
FROM workspace_members m
ON CONFLICT (id) DO NOTHING;
