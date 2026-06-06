import type { components } from '$lib/api/schema';

// Pure helpers for the streaming-channel authoring UI (docs/25). Kept out of the
// Svelte components so the non-trivial bits — rebuilding the `ElementType` union
// on a kind switch, identifier-safe names — are unit-testable without driving a
// shadcn Select through the DOM.

type Channel = components['schemas']['Channel'];
type ElementType = components['schemas']['ElementType'];
export type ElementKind = ElementType['type'];

/**
 * Channel names address edge handles AND synthesized place names in the compiled
 * net, so they must be Rhai-identifier-safe. Lower-case, collapse any run of
 * non-`[a-z0-9_]` to a single `_`, and strip leading underscores (a leading `_`
 * collides with the reserved `input._*` control leaves).
 */
export function sanitizeChannelName(raw: string): string {
	return raw
		.toLowerCase()
		.replace(/[^a-z0-9_]+/g, '_')
		.replace(/^_+/, '');
}

/**
 * A fresh, valid variant of the `ElementType` union for a kind. Switching kinds
 * MUST rebuild rather than spread — a stale `schema`/`content_type` from the
 * prior kind would make a malformed element (e.g. a `binary` carrying a leftover
 * `schema`).
 */
export function defaultElement(kind: ElementKind): ElementType {
	if (kind === 'json') return { type: 'json', schema: {} };
	if (kind === 'binary') return { type: 'binary', content_type: 'application/octet-stream' };
	return { type: 'any' };
}

/**
 * The seed channel an "Add channel" click appends: the most common shape — a
 * durable binary data-OUT channel, the producer side every streaming demo starts
 * from. Name is blank so the author fills it in (and the compiler flags an empty
 * name loudly rather than us guessing one).
 */
export function newChannel(): Channel {
	return {
		name: '',
		direction: 'out',
		plane: 'data',
		element: { type: 'binary', content_type: 'application/octet-stream' },
		transport: 'jetstream'
	};
}
