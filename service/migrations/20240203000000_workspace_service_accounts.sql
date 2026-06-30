-- Workspace service accounts — non-human API principals OWNED BY A WORKSPACE.
--
-- A workspace-scoped human PAT (`uat_`, migration 20240201) dies when its owner
-- loses membership — bad for CI/deploy. A service account is owned by the
-- workspace itself, carries a workspace role, and SURVIVES member offboarding;
-- it dies only when disabled or its token is revoked. This is ADDITIVE — human
-- `uat_` PATs are kept.
--
--   service_accounts          One row per workspace-owned machine principal.
--                             Identified by (workspace_id, name). Carries a
--                             FIXED workspace role (viewer|editor|admin — NEVER
--                             owner: a SA may never be owner). Soft-disable via
--                             `disabled_at` (NULL = live).
--   service_account_tokens    The `sat_{id}.{secret}` bearer credentials for a
--                             service account. Only the SHA-256 of the secret
--                             half is stored (`token_hash`), never the
--                             plaintext. Revoke via `revoked_at`; optional
--                             `expires_at`; best-effort `last_used_at`.
--
-- Unlike runners.sql (which leaves `workspace_id` FK-less), these tables use
-- real FKs — matching user_pats (20240201) and the workspaces/users spine.
-- VERIFIED FK targets: workspaces(id) and users(id) are both UUID PKs.

CREATE TABLE service_accounts (
    id              UUID         PRIMARY KEY DEFAULT gen_random_uuid(),

    workspace_id    UUID         NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,

    -- Operator-facing name, unique within a workspace.
    name            TEXT         NOT NULL,

    description     TEXT,

    -- Fixed workspace role the SA acts with. CHECK excludes 'owner' — a service
    -- account may never be a workspace owner.
    role            TEXT         NOT NULL CHECK (role IN ('viewer', 'editor', 'admin')),

    -- The human admin who created the SA. SET NULL on user delete so the SA
    -- (workspace-owned) outlives its creator's account.
    created_by      UUID         REFERENCES users(id) ON DELETE SET NULL,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),

    -- Soft-disable tombstone. NULL = live; non-NULL ⇒ every token is rejected.
    disabled_at     TIMESTAMPTZ,

    UNIQUE (workspace_id, name)
);

CREATE INDEX idx_service_accounts_workspace
    ON service_accounts (workspace_id)
    WHERE disabled_at IS NULL;


CREATE TABLE service_account_tokens (
    id                  UUID         PRIMARY KEY DEFAULT gen_random_uuid(),

    service_account_id  UUID         NOT NULL REFERENCES service_accounts(id) ON DELETE CASCADE,

    -- Operator-facing token name (e.g. "ci-deploy").
    name                TEXT         NOT NULL,

    -- SHA-256 (hex) of the secret half of the `sat_{id}.{secret}` credential.
    -- The plaintext is returned exactly once at mint and never stored.
    token_hash          TEXT         NOT NULL,

    created_at          TIMESTAMPTZ  NOT NULL DEFAULT now(),

    -- Optional expiry. NULL = never expires.
    expires_at          TIMESTAMPTZ,

    -- Best-effort last-use bump (fire-and-forget on the verify path).
    last_used_at        TIMESTAMPTZ,

    -- Revocation tombstone. NULL = live.
    revoked_at          TIMESTAMPTZ
);

CREATE INDEX idx_service_account_tokens_sa
    ON service_account_tokens (service_account_id)
    WHERE revoked_at IS NULL;
