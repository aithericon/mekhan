-- Phase B.11 — OAuth in-flight state.
--
-- The actual OAuth *token bundle* is stored as a regular Resource of type
-- `google_oauth` (via the resources/resource_versions tables in migration
-- 20240120000000). This table only holds the **short-lived authorization-
-- code flow state** between `GET /api/oauth/google/start` and
-- `GET /api/oauth/google/callback`.
--
-- Row lifecycle:
--   1. /start  -> INSERT with a freshly-generated CSRF `state` + PKCE verifier
--   2. /callback?state=...&code=... -> SELECT WHERE state = $1, validate
--                                       expiry, DELETE row, exchange code.
--   3. Sweep  -> background task deletes any rows older than `expires_at`.
--
-- The 10-minute expiry matches Google's authorization-code TTL; longer is
-- never useful and shorter risks legitimate-user races.

CREATE TABLE oauth_state (
    -- Random CSRF `state` echoed by the IdP on callback. Also the row's PK
    -- because the callback only knows this string.
    state               TEXT         PRIMARY KEY,

    -- Wire identifier of the provider (currently always `google` in v1;
    -- column kept generic so multi-provider in v2 doesn't need a migration).
    provider            TEXT         NOT NULL,

    -- PKCE code_verifier (43-128 chars). Held server-side; never sent to the
    -- browser. The challenge = base64url(sha256(verifier)) goes to the IdP.
    pkce_verifier       TEXT         NOT NULL,

    -- OIDC `nonce`, validated against the id_token on callback.
    nonce               TEXT         NOT NULL,

    -- Principal who started the flow. Used to attribute the resulting
    -- `google_oauth` Resource to the right owner on callback.
    principal_id        UUID         NOT NULL,

    -- Workspace the resulting Resource will be created in. Captured at
    -- /start time so the callback doesn't need to recompute it (the user
    -- may have switched workspaces in another tab between start and
    -- callback).
    workspace_id        UUID         NOT NULL,

    -- Resource path the flow will create on success (e.g. `f/me/gmail`).
    -- The /start handler accepts this from the client; /callback uses it
    -- as the target for the new `resources` row.
    resource_path       TEXT         NOT NULL,

    -- Where to send the browser after the callback finishes. Sanitized
    -- path; never a full URL.
    return_to           TEXT         NOT NULL,

    created_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    -- 10-minute window (Plan B.11). Background sweep deletes expired rows.
    expires_at          TIMESTAMPTZ  NOT NULL DEFAULT (NOW() + INTERVAL '10 minutes')
);

-- Sweep-friendly: the background task does
-- `DELETE FROM oauth_state WHERE expires_at < NOW()` and benefits from a
-- range scan on expires_at.
CREATE INDEX idx_oauth_state_expires_at ON oauth_state (expires_at);
