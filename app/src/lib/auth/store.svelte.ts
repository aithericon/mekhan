/**
 * Svelte 5 rune-based auth store. Composes the configured `AuthProvider`
 * adapter; the rest of the app reads `auth.session` reactively and never
 * imports the Zitadel adapter directly.
 *
 * Dev mode: when `VITE_AUTH_MODE=dev_noop`, no provider is constructed. The
 * store hands out a synthetic `dev` session and `getAccessToken()` returns
 * an empty string, which the backend's `NoopTokenVerifier` accepts.
 */
import { ZitadelAuthProvider, type ZitadelAdapterConfig } from './zitadel-adapter';
import type { AuthProvider, AuthSession } from './port';

const DEV_NOOP_SESSION: AuthSession = {
	accessToken: '',
	expiresAt: Number.MAX_SAFE_INTEGER,
	user: {
		subject: 'dev-user',
		email: 'dev@local',
		displayName: 'Dev User',
		roles: []
	}
};

function readConfig(): { mode: 'zitadel' | 'dev_noop'; zitadel?: ZitadelAdapterConfig } {
	const mode = (import.meta.env.VITE_AUTH_MODE as string) ?? 'dev_noop';
	if (mode === 'zitadel') {
		return {
			mode,
			zitadel: {
				authority: import.meta.env.VITE_AUTH_ISSUER_URL as string,
				clientId: import.meta.env.VITE_AUTH_CLIENT_ID as string,
				redirectUri:
					(import.meta.env.VITE_AUTH_REDIRECT_URI as string) ??
					`${window.location.origin}/auth/callback`,
				postLogoutRedirectUri:
					(import.meta.env.VITE_AUTH_POST_LOGOUT_URI as string) ??
					window.location.origin,
				scope: import.meta.env.VITE_AUTH_SCOPE as string | undefined
			}
		};
	}
	return { mode: 'dev_noop' };
}

class AuthStore {
	#session = $state<AuthSession | null>(null);
	#provider: AuthProvider | null = null;
	#ready = $state(false);

	get session(): AuthSession | null {
		return this.#session;
	}

	get ready(): boolean {
		return this.#ready;
	}

	get isAuthenticated(): boolean {
		return this.#session != null;
	}

	getAccessToken(): string {
		return this.#session?.accessToken ?? '';
	}

	async init(): Promise<void> {
		const cfg = readConfig();
		if (cfg.mode === 'dev_noop') {
			this.#session = DEV_NOOP_SESSION;
			this.#ready = true;
			return;
		}
		const provider = new ZitadelAuthProvider(cfg.zitadel!);
		this.#provider = provider;
		provider.subscribe((s) => {
			this.#session = s;
		});
		await provider.restore();
		this.#ready = true;
	}

	async signIn(): Promise<void> {
		if (this.#provider) await this.#provider.signIn();
	}

	async completeSignIn(): Promise<void> {
		if (this.#provider) await this.#provider.completeSignIn();
	}

	async signOut(): Promise<void> {
		if (this.#provider) await this.#provider.signOut();
		this.#session = null;
	}
}

export const auth = new AuthStore();
