/**
 * Zitadel adapter — wraps `oidc-client-ts` so the rest of the SPA depends
 * only on the `AuthProvider` port. Swap for a different IdP by writing
 * another implementation of `AuthProvider` and changing the import in
 * `store.svelte.ts`.
 */
import { UserManager, WebStorageStateStore, type User } from 'oidc-client-ts';

import type { AuthProvider, AuthSession, AuthUser } from './port';

export interface ZitadelAdapterConfig {
	authority: string; // Zitadel issuer URL
	clientId: string;
	redirectUri: string;
	postLogoutRedirectUri: string;
	scope?: string;
}

const ZITADEL_ROLES_CLAIM = 'urn:zitadel:iam:org:project:roles';

export class ZitadelAuthProvider implements AuthProvider {
	private readonly manager: UserManager;
	private session: AuthSession | null = null;
	private listeners = new Set<(s: AuthSession | null) => void>();

	constructor(cfg: ZitadelAdapterConfig) {
		this.manager = new UserManager({
			authority: cfg.authority,
			client_id: cfg.clientId,
			redirect_uri: cfg.redirectUri,
			post_logout_redirect_uri: cfg.postLogoutRedirectUri,
			scope: cfg.scope ?? 'openid profile email offline_access',
			response_type: 'code',
			loadUserInfo: true,
			automaticSilentRenew: true,
			userStore: new WebStorageStateStore({ store: window.localStorage })
		});

		this.manager.events.addUserLoaded((u: User) => this.publish(this.fromUser(u)));
		this.manager.events.addUserUnloaded(() => this.publish(null));
		this.manager.events.addAccessTokenExpired(() => this.publish(null));
	}

	async restore(): Promise<void> {
		const u = await this.manager.getUser();
		this.publish(u && !u.expired ? this.fromUser(u) : null);
	}

	getSession(): AuthSession | null {
		return this.session;
	}

	async signIn(): Promise<void> {
		await this.manager.signinRedirect();
	}

	async completeSignIn(): Promise<AuthSession> {
		const u = await this.manager.signinRedirectCallback();
		const s = this.fromUser(u);
		this.publish(s);
		return s;
	}

	async signOut(): Promise<void> {
		await this.manager.signoutRedirect();
	}

	subscribe(listener: (s: AuthSession | null) => void): () => void {
		this.listeners.add(listener);
		listener(this.session);
		return () => this.listeners.delete(listener);
	}

	private publish(s: AuthSession | null) {
		this.session = s;
		for (const l of this.listeners) l(s);
	}

	private fromUser(u: User): AuthSession {
		const profile = (u.profile ?? {}) as Record<string, unknown>;
		const rolesObj = profile[ZITADEL_ROLES_CLAIM];
		const roles =
			rolesObj && typeof rolesObj === 'object' ? Object.keys(rolesObj) : [];

		const user: AuthUser = {
			subject: u.profile?.sub ?? 'unknown',
			email: typeof profile.email === 'string' ? profile.email : undefined,
			displayName:
				typeof profile.name === 'string'
					? profile.name
					: typeof profile.preferred_username === 'string'
						? profile.preferred_username
						: undefined,
			roles
		};
		return {
			accessToken: u.access_token,
			expiresAt: u.expires_at ?? Math.floor(Date.now() / 1000) + 3600,
			user
		};
	}
}
