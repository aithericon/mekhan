-- Phase 1 — Lab Runner Fleet.
--
-- Two workspace-scoped tables backing GitLab-style runner enrollment:
--
--   runners                       One row per enrolled lab runner. Identified
--                                 by (workspace_id, name). Authenticated by a
--                                 mekhan-native control-plane credential
--                                 `rnr_{id}.{secret}` — only the SHA-256 of the
--                                 secret half is stored (`token_hash`), never
--                                 the plaintext. This is NOT a Zitadel PAT and
--                                 works fully offline in `dev_noop`. Soft-delete
--                                 via `revoked_at` (NULL = live).
--   runner_registration_tokens    GitLab-style enrollment secrets. A runner
--                                 presents `rt_{id}.{secret}` to the public
--                                 `POST /api/v1/runners/enroll` endpoint; the
--                                 enrolled runner inherits the token's
--                                 `workspace_id` + `pool`. Reusable or
--                                 single-use (`reusable`), optionally capped by
--                                 `max_uses` and `expires_at`. Only the SHA-256
--                                 of the secret half is stored. Soft-delete via
--                                 `revoked_at`.
--
-- `workspace_id` is a UUID without an FK constraint here, mirroring the
-- resources migration — forward-compatible with the workspaces table; adding
-- a FK later is a single ALTER away.

CREATE TABLE runners (
    id                  UUID         PRIMARY KEY DEFAULT gen_random_uuid(),

    workspace_id        UUID         NOT NULL,

    -- Operator-facing name, unique within a workspace.
    name                TEXT         NOT NULL,

    -- Optional pool the runner joins (inherited from the registration token).
    pool                TEXT,

    -- SHA-256 (hex) of the secret half of the `rnr_{id}.{secret}` credential.
    -- The plaintext is returned exactly once at enrollment and never stored.
    token_hash          TEXT         NOT NULL,

    -- Optional NATS account public key the runner presented at enrollment.
    nats_public_key     TEXT,

    -- Arbitrary self-reported capability blob.
    capabilities        JSONB        NOT NULL DEFAULT '{}'::jsonb,

    -- Lifecycle marker: `enrolled` | `revoked`. Strings (not an enum) so v2
    -- can grow states without an ALTER TYPE.
    status              TEXT         NOT NULL DEFAULT 'enrolled',

    -- Bumped by the heartbeat endpoint.
    last_seen_at        TIMESTAMPTZ,

    -- The registration token's `created_by` — runners enroll without a human
    -- principal on the request, so attribution flows from the token.
    enrolled_by         UUID         NOT NULL,
    enrolled_at         TIMESTAMPTZ  NOT NULL DEFAULT NOW(),

    -- Soft-delete tombstone. NULL = live.
    revoked_at          TIMESTAMPTZ,

    UNIQUE (workspace_id, name)
);

CREATE INDEX idx_runners_workspace
    ON runners (workspace_id)
    WHERE revoked_at IS NULL;


CREATE TABLE runner_registration_tokens (
    id                  UUID         PRIMARY KEY DEFAULT gen_random_uuid(),

    workspace_id        UUID         NOT NULL,

    -- Pool every runner enrolled with this token joins.
    pool                TEXT,

    -- SHA-256 (hex) of the secret half of the `rt_{id}.{secret}` credential.
    token_hash          TEXT         NOT NULL,

    -- `true` → the token may enroll many runners (subject to `max_uses`).
    -- `false` → single-use: exhausted once `uses >= 1`.
    reusable            BOOLEAN      NOT NULL DEFAULT TRUE,

    -- Count of successful enrollments against this token.
    uses                INT          NOT NULL DEFAULT 0,

    -- Optional hard cap on `uses`. NULL = unlimited (reusable) / governed by
    -- the single-use rule (non-reusable).
    max_uses            INT,

    -- Optional expiry. NULL = never expires.
    expires_at          TIMESTAMPTZ,

    created_by          UUID         NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),

    -- Soft-delete tombstone. NULL = live.
    revoked_at          TIMESTAMPTZ
);

CREATE INDEX idx_runner_reg_tokens_workspace
    ON runner_registration_tokens (workspace_id)
    WHERE revoked_at IS NULL;
