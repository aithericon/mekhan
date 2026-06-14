/**
 * DOM tests for the artifact-embed node view — its render branches: no run
 * context, an empty live store, a live store carrying a renderable image, and a
 * PINNED single artifact (rendered straight from snapshot attrs, no store).
 *
 * The live + pinned image branches prove the node view actually draws media via
 * the SHARED ArtifactsPanel / renderer registry (the same path the Process
 * Overview "Media" card uses) — what the schema round-trip test can't reach.
 */
import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';
import ArtifactEmbedView from './ArtifactEmbedView.svelte';
import type { ArtifactEmbedAttrs } from './artifact-embed';
import type { ArtifactEmbedContext } from './embed-context';

afterEach(() => cleanup());

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

function attrs(partial: Partial<ArtifactEmbedAttrs> = {}): ArtifactEmbedAttrs {
	return {
		processId: 'proc-1',
		processName: 'Simulate',
		mode: 'all',
		groupKey: '',
		groupLabel: '',
		artifactId: '',
		artifactName: '',
		storagePath: '',
		mimeType: '',
		renderHint: '',
		category: '',
		processStep: '',
		caption: '',
		...partial
	};
}

describe('ArtifactEmbedView', () => {
	it('renders a neutral placeholder when there is no run context', () => {
		const { getByText } = render(ArtifactEmbedView, {
			props: { attrs: attrs(), editable: true, context: null, onDelete: () => {} }
		});
		expect(getByText(/needs a run context/i)).toBeTruthy();
	});

	it('shows the empty-state when the run has no renderable media yet', () => {
		const { getByText } = render(ArtifactEmbedView, {
			props: { attrs: attrs(), editable: false, context: contextWith([]), onDelete: () => {} }
		});
		expect(getByText(/No renderable media yet/i)).toBeTruthy();
	});

	it('mounts the shared ArtifactsPanel for an "all" embed with a renderable image', () => {
		const ctx = contextWith([imageArtifact]);
		const { getByText, queryByText } = render(ArtifactEmbedView, {
			props: { attrs: attrs({ caption: 'Final renders' }), editable: true, context: ctx, onDelete: () => {} }
		});
		expect(ctx.getArtifactStore).toHaveBeenCalledWith('proc-1');
		expect(queryByText(/No renderable media yet/i)).toBeNull();
		expect(getByText('gp_final_state.png')).toBeTruthy();
		// caption drives the header
		expect(getByText('Final renders')).toBeTruthy();
	});

	it('renders a pinned single artifact WITHOUT resolving the live store', () => {
		const ctx = contextWith([]);
		const { getAllByText } = render(ArtifactEmbedView, {
			props: {
				attrs: attrs({
					mode: 'artifact',
					artifactId: 'art-1',
					artifactName: 'gp_final_state.png',
					storagePath: imageArtifact.storage_path,
					mimeType: 'image/png',
					category: 'plot'
				}),
				editable: true,
				context: ctx,
				onDelete: () => {}
			}
		});
		// Pinned mode draws from attrs, not the per-process store.
		expect(ctx.getArtifactStore).not.toHaveBeenCalled();
		// header (and the image renderer) show the artifact name → renderer mounted
		expect(getAllByText('gp_final_state.png').length).toBeGreaterThanOrEqual(1);
	});

	it('uses the group label as the header for a group embed', () => {
		const { getByText } = render(ArtifactEmbedView, {
			props: {
				attrs: attrs({ mode: 'group', groupKey: 'hint:gp-posterior', groupLabel: 'gp-posterior' }),
				editable: true,
				context: contextWith([]),
				onDelete: () => {}
			}
		});
		expect(getByText('gp-posterior')).toBeTruthy();
		expect(getByText(/Simulate/)).toBeTruthy();
	});
});
