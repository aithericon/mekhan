/**
 * Auth-aware `fetch` for the few call sites that can't route through the
 * `openapi-fetch` client (SSE streams, multipart uploads, ad-hoc URLs).
 *
 * Sources the access token from the same auth store that the
 * `openapi-fetch` middleware uses, so dev-noop + Zitadel modes both work
 * without the call site caring which one is active.
 */
import { auth } from './store.svelte';

export function authFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
	const token = auth.getAccessToken();
	if (!token) return fetch(input, init);

	const headers = new Headers(init?.headers);
	if (!headers.has('Authorization')) {
		headers.set('Authorization', `Bearer ${token}`);
	}
	return fetch(input, { ...init, headers });
}
