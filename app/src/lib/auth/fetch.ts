/**
 * `fetch` for the few call sites that can't route through the `openapi-fetch`
 * client (SSE streams, multipart uploads, ad-hoc URLs).
 *
 * BFF model: there is no token to attach — the `mekhan_session` HttpOnly
 * cookie is sent automatically on same-origin requests (including
 * `EventSource`/SSE, which can't set headers but does carry cookies). This
 * thin wrapper just guarantees `credentials: 'same-origin'` so it works
 * identically whether the caller is dev_noop or behind Zitadel.
 */
export function authFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
	return fetch(input, { credentials: 'same-origin', ...init });
}
