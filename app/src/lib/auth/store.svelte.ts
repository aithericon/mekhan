/**
 * Svelte 5 rune-based auth store — BFF model.
 *
 * The browser no longer runs the OIDC flow or holds any token. Auth state is
 * a single server probe: `GET /api/auth/session` returns the resolved
 * `AuthUser` (200) when the HttpOnly `mekhan_session` cookie is valid, or 401
 * when it isn't. In `dev_noop` the backend always returns the fixed dev user,
 * so the SPA runs fully offline with no redirect.
 *
 * The `auth` rune store shape is kept so existing callers
 * (`auth.session`, `auth.isAuthenticated`, `auth.ready`) are untouched.
 */
import type { AuthSession, AuthUser } from './port';

/** Wire shape of `GET /api/auth/session` — the backend's `AuthUser`. */
interface SessionUserDto {
	subject: string;
	email?: string | null;
	display_name?: string | null;
	roles?: string[];
	org_id?: string | null;
}

function toUser(dto: SessionUserDto): AuthUser {
	return {
		subject: dto.subject,
		email: dto.email ?? undefined,
		displayName: dto.display_name ?? undefined,
		roles: dto.roles ?? [],
		orgId: dto.org_id ?? undefined
	};
}

class AuthStore {
	#session = $state<AuthSession | null>(null);
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

	/**
	 * Probe the server for the current session. Idempotent — safe to call
	 * from the layout guard on every navigation; the cookie does the work.
	 */
	async init(): Promise<void> {
		try {
			const res = await fetch('/api/auth/session', {
				headers: { Accept: 'application/json' },
				credentials: 'same-origin'
			});
			if (res.ok) {
				const dto = (await res.json()) as SessionUserDto;
				this.#session = {
					// No token ever reaches the browser in the BFF model. The
					// `accessToken` field is retained for the `AuthSession`
					// shape but is intentionally empty.
					accessToken: '',
					expiresAt: Number.MAX_SAFE_INTEGER,
					user: toUser(dto)
				};
			} else {
				this.#session = null;
			}
		} catch {
			// Network failure → treat as signed out; the guard will redirect.
			this.#session = null;
		} finally {
			this.#ready = true;
		}
	}

	/**
	 * Full-page navigation to the server-side login. Must be a navigation
	 * (not fetch) so the subsequent Zitadel redirect is a top-level request.
	 */
	signIn(returnTo?: string): void {
		const target = returnTo ?? window.location.pathname + window.location.search;
		window.location.assign(`/api/auth/login?return_to=${encodeURIComponent(target)}`);
	}

	/**
	 * Kill the server session, then navigate to the IdP end-session URL (if
	 * the backend provides one) or home.
	 */
	async signOut(): Promise<void> {
		try {
			const res = await fetch('/api/auth/logout', {
				method: 'POST',
				credentials: 'same-origin'
			});
			this.#session = null;
			const body = (await res.json().catch(() => ({}))) as {
				end_session_url?: string | null;
			};
			window.location.assign(body.end_session_url ?? '/');
		} catch {
			this.#session = null;
			window.location.assign('/');
		}
	}
}

export const auth = new AuthStore();
