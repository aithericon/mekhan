-- Mekhan-native Personal Access Tokens (PATs).
--
-- Replaces the Zitadel token-broker + RFC 7662 introspection path: a human
-- mints a `uat_{id}.{secret}` credential via `/api/v1/auth/tokens`, presents it
-- as `Authorization: Bearer uat_...` on non-interactive API calls (CI
-- `mekhan apply`), and the middleware reconstructs the OWNING human principal
-- against the local `users` spine — fully offline, no IdP round-trip.
--
-- Only the SHA-256 of the secret half is stored (`token_hash`); the plaintext
-- is surfaced exactly once at mint. `id` is allocated in Rust via
-- `Uuid::new_v4()` (the mint path needs it to build the token string), so there
-- is no `gen_random_uuid()` default here. Soft-delete via `revoked_at`
-- (NULL = live). Column order matches the `UserPatRow` `sqlx::FromRow`.

CREATE TABLE user_pats (
    id           UUID PRIMARY KEY,
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    description  TEXT,
    token_hash   TEXT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at   TIMESTAMPTZ,
    last_used_at TIMESTAMPTZ,
    revoked_at   TIMESTAMPTZ
);

-- O(user) lookup of a caller's live tokens for the list endpoint.
CREATE INDEX idx_user_pats_user_active ON user_pats (user_id) WHERE revoked_at IS NULL;
