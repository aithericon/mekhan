-- BFF auth: server-side OIDC token custody.
--
-- The browser holds only an opaque HttpOnly cookie whose value is the
-- `auth_sessions.id`. The token set (access/refresh/id) lives here, never in
-- the client. `user_json` caches the resolved domain `AuthUser` so the hot
-- path (every authenticated request) is a single indexed lookup with no JWT
-- re-verification — re-resolution only happens on refresh.

-- Established sessions. One row per signed-in browser.
CREATE TABLE auth_sessions (
    -- Opaque 256-bit id, base64url (no padding). Also the cookie value.
    id TEXT PRIMARY KEY,
    -- OIDC `sub`. Indexed so an admin/logout-all can target a subject.
    subject TEXT NOT NULL,
    access_token TEXT NOT NULL,
    -- NULL when the IdP didn't issue a refresh token (no offline_access).
    refresh_token TEXT,
    -- Kept for RP-initiated logout (`id_token_hint` on end_session).
    id_token TEXT,
    -- Absolute access-token expiry. The authenticator refreshes in-place when
    -- now() is within a small skew of this.
    access_expires_at TIMESTAMPTZ NOT NULL,
    -- Cached resolved `AuthUser` (StaticPrincipalResolver output).
    user_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_auth_sessions_subject ON auth_sessions (subject);

-- In-flight Authorization-Code+PKCE logins. Created on GET /api/auth/login,
-- consumed (and deleted) on the matching GET /api/auth/callback. Short-lived;
-- swept by the same TTL sweep as expired sessions.
CREATE TABLE auth_login_flows (
    -- Random CSRF `state` echoed by the IdP on callback.
    state TEXT PRIMARY KEY,
    -- PKCE code_verifier (43-128 chars). Held server-side; never sent to the
    -- browser. The challenge = base64url(sha256(verifier)) went to the IdP.
    pkce_verifier TEXT NOT NULL,
    -- OIDC `nonce`, validated against the id_token on callback.
    nonce TEXT NOT NULL,
    -- Where to send the browser after a successful login (sanitized path).
    return_to TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
