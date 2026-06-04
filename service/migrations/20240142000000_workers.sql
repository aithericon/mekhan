-- Phase A — Grouped + Enrolled Workers (the identity plane for the executor
-- worker pool; docs/23 + docs/24).
--
-- The exact parallel of `20240134000000_runners.sql` for the *worker* pool. A
-- worker is the long-running executor daemon that PULLS jobs off the per-backend
-- `executor-<wire>` work queues. Today those workers are anonymous; these two
-- tables give them the same enrolled / scoped-credential / revocable identity
-- runners already have, WITHOUT changing the pull dispatch discipline. The
-- `worker_group` is a second coarse *pull* routing coordinate
-- (`executor-<wire>/<group>`) — a competing pool, never a per-worker partition.
--
--   workers                       One row per enrolled worker. Identified by
--                                 (workspace_id, name). Authenticated by a
--                                 mekhan-native control-plane credential
--                                 `wkr_{id}.{secret}` — only the SHA-256 of the
--                                 secret half is stored (`token_hash`), never the
--                                 plaintext. Works fully offline in `dev_noop`.
--                                 Soft-delete via `revoked_at` (NULL = live).
--   worker_registration_tokens    Reusable `wt_{id}.{secret}` enrollment secrets
--                                 (the elastic-autoscale launch-template model:
--                                 bake a group token in, workers self-enroll on
--                                 boot). An enrolled worker inherits the token's
--                                 `workspace_id` + `worker_group`. Reusable or
--                                 single-use (`reusable`), optionally capped by
--                                 `max_uses` and `expires_at`. Only the SHA-256 of
--                                 the secret half is stored. Soft-delete via
--                                 `revoked_at`.
--
-- `workspace_id` is a UUID without an FK constraint here, mirroring the runners
-- migration — forward-compatible with the workspaces table; adding a FK later is
-- a single ALTER away.

CREATE TABLE workers (
    id                  UUID         PRIMARY KEY DEFAULT gen_random_uuid(),

    workspace_id        UUID         NOT NULL,

    -- Operator-facing name, unique within a workspace.
    name                TEXT         NOT NULL,

    -- Optional group the worker joins (inherited from the registration token).
    -- A second coarse PULL routing coordinate (`executor-<wire>/<group>`), NOT a
    -- per-worker partition. NULL = the plain anonymous-equivalent pull path.
    worker_group        TEXT,

    -- SHA-256 (hex) of the secret half of the `wkr_{id}.{secret}` credential.
    -- The plaintext is returned exactly once at enrollment and never stored.
    token_hash          TEXT         NOT NULL,

    -- Optional NATS account public key the worker presented at enrollment.
    nats_public_key     TEXT,

    -- Self-reported executor backends this worker serves (wire-names, e.g.
    -- `["python","docker"]`). The set the scoped JWT's SUBSCRIBE grant is built
    -- from. Defaults to `[]`.
    backends            JSONB        NOT NULL DEFAULT '[]'::jsonb,

    -- Lifecycle marker: `enrolled` | `revoked`. Strings (not an enum) so v2 can
    -- grow states without an ALTER TYPE.
    status              TEXT         NOT NULL DEFAULT 'enrolled',

    -- Bumped by the heartbeat endpoint.
    last_seen_at        TIMESTAMPTZ,

    -- The registration token's `created_by` — workers enroll without a human
    -- principal on the request, so attribution flows from the token.
    enrolled_by         UUID         NOT NULL,
    enrolled_at         TIMESTAMPTZ  NOT NULL DEFAULT NOW(),

    -- Soft-delete tombstone. NULL = live.
    revoked_at          TIMESTAMPTZ,

    UNIQUE (workspace_id, name)
);

CREATE INDEX idx_workers_workspace
    ON workers (workspace_id)
    WHERE revoked_at IS NULL;


CREATE TABLE worker_registration_tokens (
    id                  UUID         PRIMARY KEY DEFAULT gen_random_uuid(),

    workspace_id        UUID         NOT NULL,

    -- Group every worker enrolled with this token joins.
    worker_group        TEXT,

    -- SHA-256 (hex) of the secret half of the `wt_{id}.{secret}` credential.
    token_hash          TEXT         NOT NULL,

    -- `true` → the token may enroll many workers (subject to `max_uses`).
    -- `false` → single-use: exhausted once `uses >= 1`.
    reusable            BOOLEAN      NOT NULL DEFAULT TRUE,

    -- Count of successful enrollments against this token.
    uses                INT          NOT NULL DEFAULT 0,

    -- Optional hard cap on `uses`. NULL = unlimited (reusable) / governed by the
    -- single-use rule (non-reusable).
    max_uses            INT,

    -- Optional expiry. NULL = never expires.
    expires_at          TIMESTAMPTZ,

    created_by          UUID         NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),

    -- Soft-delete tombstone. NULL = live.
    revoked_at          TIMESTAMPTZ
);

CREATE INDEX idx_worker_reg_tokens_workspace
    ON worker_registration_tokens (workspace_id)
    WHERE revoked_at IS NULL;
