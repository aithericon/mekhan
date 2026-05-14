// SPDX-License-Identifier: Apache-2.0
import { getContext, setContext } from 'svelte';

const LINK_ID_KEY = 'magic-link-id';

/** Set the magic link ID in Svelte context (call from magic link pages). */
export function setLinkId(linkId: string): void {
	setContext(LINK_ID_KEY, linkId);
}

/** Get the magic link ID from Svelte context, or null if not in a magic link page. */
export function getLinkId(): string | null {
	return getContext<string | null>(LINK_ID_KEY) ?? null;
}

/** Append `?link=` to a URL if it's an internal file URL and a linkId is available. */
export function withLinkParam(url: string, linkId: string | null): string {
	if (!linkId) return url;
	// Match both relative (/api/files/...) and absolute (https://app.hpi.dev/api/files/...) URLs
	if (!url.includes('/api/files/')) return url;
	const sep = url.includes('?') ? '&' : '?';
	return `${url}${sep}link=${encodeURIComponent(linkId)}`;
}
