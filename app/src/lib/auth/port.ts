/**
 * Frontend auth port. Mirrors the Rust backend's `TokenVerifier` /
 * `PrincipalResolver` split: the rest of the SPA depends only on this
 * interface and the `AuthSession` shape, never on Zitadel's specifics.
 *
 * Adapters live alongside this file (`zitadel-adapter.ts`). Swap by editing
 * the import in `store.svelte.ts`.
 */

export interface AuthUser {
	subject: string;
	email?: string;
	displayName?: string;
	roles: string[];
}

export interface AuthSession {
	accessToken: string;
	expiresAt: number; // unix seconds
	user: AuthUser;
}

export interface AuthProvider {
	/** Returns the active session, or null if signed out / never signed in. */
	getSession(): AuthSession | null;
	/** Redirect the browser to the identity provider's login flow. */
	signIn(): Promise<void>;
	/** Complete the OIDC redirect callback. Returns the established session. */
	completeSignIn(): Promise<AuthSession>;
	/** Clear local session state and (optionally) redirect to provider logout. */
	signOut(): Promise<void>;
	/**
	 * Subscribe to session changes. Returns an unsubscribe function. Adapters
	 * push a new value whenever silent renew refreshes the token or signOut
	 * clears it.
	 */
	subscribe(listener: (session: AuthSession | null) => void): () => void;
}
