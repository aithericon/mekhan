/**
 * Auth data shapes shared across the SPA.
 *
 * In the BFF model the browser never runs the OIDC flow or holds a token —
 * the Rust service owns all of that. What remains client-side is just the
 * resolved-principal shape that `GET /api/auth/session` returns and the
 * `auth` rune store exposes. (The old `AuthProvider` port + Zitadel adapter
 * were removed with the client-side OIDC.)
 */

export interface AuthUser {
	subject: string;
	email?: string;
	displayName?: string;
	roles: string[];
	/** Zitadel org the principal belongs to, when the IdP asserts one. */
	orgId?: string;
	/**
	 * Mekhan workspace the session is currently scoped to. Populated by the
	 * resolver from `workspace_members`, optionally overridden per-session
	 * via the `mekhan_active_workspace` cookie set by `POST /api/v1/me/active-workspace`.
	 */
	workspaceId?: string;
}

export interface AuthSession {
	/**
	 * Always empty in the BFF model — no token reaches the browser. Retained
	 * so the `AuthSession` shape (and existing readers) stay stable.
	 */
	accessToken: string;
	expiresAt: number; // unix seconds
	user: AuthUser;
}
