/**
 * Route-guard helper. Call once at the top of `+layout.svelte` to make sure
 * the auth store is initialized before any protected UI renders, and to
 * redirect anonymous visitors to the IdP.
 *
 * Routes that should remain anonymous (`/auth/callback`) opt out by checking
 * `auth.ready` and not awaiting `requireSession()`.
 */
import { auth } from './store.svelte';

let initialized: Promise<void> | null = null;

export function ensureAuthInitialized(): Promise<void> {
	if (!initialized) initialized = auth.init();
	return initialized;
}

export async function requireSession(): Promise<void> {
	await ensureAuthInitialized();
	if (!auth.isAuthenticated) {
		await auth.signIn();
	}
}
