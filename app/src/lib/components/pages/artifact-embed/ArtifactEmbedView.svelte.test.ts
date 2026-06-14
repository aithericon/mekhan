/**
 * DOM tests for the artifact-embed node view — the three render branches the
 * Tiptap node mounts: no run context, a context with zero renderable media, and
 * a context whose live store carries a renderable image.
 *
 * The image branch is what proves the node view actually mounts the SHARED
 * ArtifactsPanel (the same component the Process Overview "Media" card uses)
 * against a resolved per-process store — the one path the schema round-trip
 * test can't reach.
 */
import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';
import ArtifactEmbedView from './ArtifactEmbedView.svelte';
import type { ArtifactEmbedContext } from './embed-context';

afterEach(() => cleanup());

// Minimal live-store shape ArtifactsPanel reads: artifacts + status + error.
function storeWith(artifacts: unknown[]) {
	return { artifacts, artifactStatus: 'streaming', error: null };
}

function contextWith(artifacts: unknown[]): ArtifactEmbedContext {
	return {
		processes: [],
		getArtifactStore: vi.fn(() => storeWith(artifacts))
	} as unknown as ArtifactEmbedContext;
}

const imageArtifact = {
	id: 'art-1',
	artifact_id: 'art-1',
	execution_id: 'exec-1',
	name: 'gp_final_state.png',
	category: 'plot',
	filename: 'gp_final_state.png',
	mime_type: 'image/png',
	storage_path: 'artifacts/exec-1/plot/gp_final_state.png',
	size_bytes: 1234,
	process_step: '3',
	signal_key: null,
	user_metadata: null,
	created_at: '2026-06-14T10:00:00Z'
};

const baseAttrs = { processId: 'proc-1', processName: 'Simulate', caption: 'Final renders' };

describe('ArtifactEmbedView', () => {
	it('renders a neutral placeholder when there is no run context', () => {
		const { getByText } = render(ArtifactEmbedView, {
			props: { attrs: baseAttrs, editable: true, context: null, onDelete: () => {} }
		});
		expect(getByText(/needs a run context/i)).toBeTruthy();
	});

	it('shows the empty-state when the run has no renderable media yet', () => {
		const { getByText } = render(ArtifactEmbedView, {
			props: { attrs: baseAttrs, editable: false, context: contextWith([]), onDelete: () => {} }
		});
		expect(getByText(/No renderable media yet/i)).toBeTruthy();
	});

	it('mounts the shared ArtifactsPanel when the store carries a renderable image', () => {
		const ctx = contextWith([imageArtifact]);
		const { getByText, queryByText } = render(ArtifactEmbedView, {
			props: { attrs: baseAttrs, editable: true, context: ctx, onDelete: () => {} }
		});
		// Resolved the per-process store...
		expect(ctx.getArtifactStore).toHaveBeenCalledWith('proc-1');
		// ...and rendered the artifact (panel mounted), not the empty-state.
		expect(queryByText(/No renderable media yet/i)).toBeNull();
		expect(getByText('gp_final_state.png')).toBeTruthy();
	});

	it('shows the caption + process name in the header', () => {
		const { getByText } = render(ArtifactEmbedView, {
			props: { attrs: baseAttrs, editable: true, context: contextWith([]), onDelete: () => {} }
		});
		expect(getByText('Final renders')).toBeTruthy();
		expect(getByText(/Simulate/)).toBeTruthy();
	});
});
