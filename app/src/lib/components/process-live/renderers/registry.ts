/**
 * Renderer dispatch for live artifacts.
 *
 * Priority order:
 *   1. `user_metadata.render_hint` (producer-declared) → HINT_RENDERERS
 *   2. `mime_type` prefix match → MIME_RENDERERS
 *   3. null → caller falls back to the existing ArtifactCard download tile.
 *
 * Adding a new visualization = one Svelte component + one entry here. No
 * backend changes. No store changes. The producer just sets
 * `metadata={"render_hint": "my-viz"}` on `log_artifact`.
 */

import type { Component } from 'svelte';
import type { LiveArtifactEntry } from '$lib/api/client';

import GpPosteriorRenderer from './GpPosteriorRenderer.svelte';
import ImageRenderer from './ImageRenderer.svelte';
import TextRenderer from './TextRenderer.svelte';
import JsonRenderer from './JsonRenderer.svelte';

export interface RendererProps {
	entry: LiveArtifactEntry;
}

/** render_hint → component. Primary dispatch. */
export const HINT_RENDERERS: Record<string, Component<RendererProps>> = {
	'gp-posterior': GpPosteriorRenderer as unknown as Component<RendererProps>
	// future: 'docking-pose', 'ei-surface', 'candidate-scatter', ...
};

/** MIME-prefix regex → component. Fallback for declared MIME types. */
export const MIME_RENDERERS: [RegExp, Component<RendererProps>][] = [
	[/^image\//, ImageRenderer as unknown as Component<RendererProps>],
	[/^text\//, TextRenderer as unknown as Component<RendererProps>],
	[/^application\/json/, JsonRenderer as unknown as Component<RendererProps>]
];

/** Render hints the frontend can handle. Used for the stream whitelist. */
export const KNOWN_RENDER_HINTS = Object.keys(HINT_RENDERERS);

/** Categories always considered potentially renderable (MIME-based). */
export const RENDERABLE_CATEGORIES = ['model', 'plot', 'image', 'text', 'dataset'];

export function pickRenderer(entry: LiveArtifactEntry): Component<RendererProps> | null {
	const meta = entry.user_metadata ?? {};
	const hint = typeof meta.render_hint === 'string' ? meta.render_hint : null;
	if (hint && HINT_RENDERERS[hint]) return HINT_RENDERERS[hint];
	const mime = entry.mime_type ?? '';
	for (const [re, c] of MIME_RENDERERS) if (re.test(mime)) return c;
	return null;
}

/**
 * Group key for an artifact — drives the "one panel per viz kind" layout.
 * Prefers render_hint (exact viz bucket), falls back to MIME family, then
 * category. Artifacts sharing a group key share a slider.
 */
export function groupKey(entry: LiveArtifactEntry): string {
	const meta = entry.user_metadata ?? {};
	const hint = typeof meta.render_hint === 'string' ? meta.render_hint : null;
	if (hint) return `hint:${hint}`;
	const mime = entry.mime_type ?? '';
	if (mime.startsWith('image/')) return 'mime:image';
	if (mime.startsWith('video/')) return 'mime:video';
	if (mime.startsWith('text/')) return 'mime:text';
	if (mime.startsWith('application/json')) return 'mime:json';
	return `category:${entry.category}`;
}

export function groupLabel(key: string): string {
	if (key.startsWith('hint:')) return key.slice(5);
	if (key.startsWith('mime:')) return key.slice(5);
	if (key.startsWith('category:')) return key.slice(9);
	return key;
}

/**
 * Parse a process_step string (either a pure integer or an `iteration`
 * metadata field) into a sortable number. Falls back to -1 if unparseable
 * so live-only rows still sort deterministically by created_at.
 */
export function stepNumber(entry: LiveArtifactEntry): number {
	const step = entry.process_step;
	if (step) {
		const n = parseInt(step, 10);
		if (!Number.isNaN(n)) return n;
	}
	const meta = entry.user_metadata ?? {};
	const iter = meta.iteration;
	if (typeof iter === 'string') {
		const n = parseInt(iter, 10);
		if (!Number.isNaN(n)) return n;
	}
	if (typeof iter === 'number') return iter;
	return -1;
}
