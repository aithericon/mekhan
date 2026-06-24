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
	/**
	 * Deterministic `uuid_v5(SUBJECT_UUID_NAMESPACE, subject)` — the same value
	 * carried on every `created_by`/`author_id`/grant row. Always present on the
	 * session DTO (the backend serializes it unconditionally) so the profile
	 * cache can seed itself with the caller's own identity without recomputing
	 * the v5 namespace in JS.
	 */
	userId?: string;
	email?: string;
	displayName?: string;
	/** Profile photo URL from the OIDC `picture` claim; absent → initials. */
	avatarUrl?: string;
	roles: string[];
	/**
	 * Mekhan workspace the session is currently scoped to. Populated by the
	 * resolver from `workspace_members`, optionally overridden per-session
	 * via the `mekhan_active_workspace` cookie set by `POST /api/v1/me/active-workspace`.
	 */
	workspaceId?: string;
	/**
	 * The caller's role (`owner` | `admin` | `editor` | `viewer`) in their
	 * RESOLVED `workspaceId`. Populated by the resolver from the same
	 * `workspace_members` row. Drives admin-only UI gating (roster enroll /
	 * edit / revoke). Absent when no membership backs the workspace.
	 */
	workspaceRole?: string;
	/**
	 * Whether the principal is a platform-level administrator — a global tier
	 * above any single workspace. Gates platform-scoped affordances (e.g.
	 * creating/curating `scope_kind: 'platform'` resources). The server is
	 * authoritative; this only hides the affordance client-side.
	 */
	isPlatformAdmin?: boolean;
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
