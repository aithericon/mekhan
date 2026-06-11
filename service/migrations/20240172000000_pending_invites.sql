-- Phase 4 — pending invites / onboarding. An Admin/Owner invites by email
-- (optionally pre-seeding a workspace role + object grants); the invitee
-- accepts via a public token link, which provisions/resolves their identity
-- and applies the membership + grants atomically.
--
-- Numbered AFTER the object-ACL migration (171) because `invite_object_grants`
-- mirrors the `object_grants` shape and accept writes into `object_grants`
-- via `apply_grant`.
--
-- The raw token is NEVER stored — only its SHA-256 hash. Lookup is a single
-- indexed equality on `token_hash`; the public preview/accept endpoints return
-- one generic 404 for unknown/expired/revoked/accepted (no enumeration).

CREATE TABLE pending_invites (
    id               UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id     UUID         NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    email            TEXT         NOT NULL,   -- normalized lower-case
    role             TEXT         NOT NULL CHECK (role IN ('owner', 'admin', 'editor', 'viewer')),
    token_hash       BYTEA        NOT NULL,   -- SHA-256 of the raw token; raw token never stored
    invited_by       UUID         NOT NULL,
    created_at       TIMESTAMPTZ  NOT NULL DEFAULT now(),
    expires_at       TIMESTAMPTZ  NOT NULL,
    accepted_at      TIMESTAMPTZ  NULL,
    accepted_user_id UUID         NULL,
    revoked_at       TIMESTAMPTZ  NULL,
    status           TEXT         NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'accepted', 'revoked', 'expired'))
);

CREATE UNIQUE INDEX pending_invites_token_hash_uniq ON pending_invites (token_hash);

-- One live invite per email per workspace (resend rotates the row's token).
-- Mirrors roster_members' live-unique partial index (migration 20240163).
CREATE UNIQUE INDEX pending_invites_active_email_uniq
    ON pending_invites (workspace_id, lower(email)) WHERE status = 'pending';

CREATE INDEX idx_pending_invites_ws ON pending_invites (workspace_id, status);

-- Object grants to apply on accept. Mirrors `object_grants` (polymorphic, no FK
-- on object_id) but scoped to the invite and cascaded with it.
CREATE TABLE invite_object_grants (
    invite_id   UUID  NOT NULL REFERENCES pending_invites(id) ON DELETE CASCADE,
    object_type TEXT  NOT NULL CHECK (object_type IN ('folder', 'template', 'instance')),
    object_id   UUID  NOT NULL,
    role        TEXT  NOT NULL CHECK (role IN ('owner', 'admin', 'editor', 'viewer')),
    PRIMARY KEY (invite_id, object_type, object_id)
);
