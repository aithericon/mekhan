/**
 * Route-guard helper. Call once at the top of `+layout.svelte` to make sure
 * the auth store is initialized before any protected UI renders, and to
 * redirect anonymous visitors to the server-side login.
 *
 * In `dev_noop` the session probe always succeeds, so `requireSession()` is a
 * no-op and the app runs offline. In `bff` mode an unauthenticated visitor is
 * sent (full-page) to `/api/auth/login`, which 302s to Zitadel.
 */
import { auth } from './store.svelte';
import { seedSelfProfile } from '$lib/stores/profiles.svelte';

let initialized: Promise<void> | null = null;

export function ensureAuthInitialized(): Promise<void> {
	if (!initialized) {
		// Seed the profile cache with the caller's own identity once the session
		// resolves, so the most common authorship UUID renders without a round trip.
		initialized = auth.init().then(() => seedSelfProfile());
	}
	return initialized;
}

export async function requireSession(): Promise<void> {
	await ensureAuthInitialized();
	if (!auth.isAuthenticated) {
		// Full-page navigation — the Zitadel redirect must be a top-level
		// request, not a fetch (CORS + cookie + the IdP login UI).
		auth.signIn();
	}
}
