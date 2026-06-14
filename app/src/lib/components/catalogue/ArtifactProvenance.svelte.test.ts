import { describe, it, expect, afterEach } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';
import ArtifactProvenance from './ArtifactProvenance.svelte';
import type { LiveArtifactEntry } from '$lib/api/client';

afterEach(() => cleanup());

function entry(p: Partial<LiveArtifactEntry> = {}): LiveArtifactEntry {
	return {
		id: 'a',
		artifact_id: 'a',
		execution_id: 'e',
		name: 'x.png',
		category: 'plot',
		filename: 'x.png',
		mime_type: 'image/png',
		storage_path: 'p',
		size_bytes: 20480,
		process_step: '4',
		signal_key: null,
		user_metadata: null,
		created_at: '2026-06-14T10:00:00Z',
		...p
	} as LiveArtifactEntry;
}

describe('ArtifactProvenance', () => {
	it('renders step, category, type and a human size', () => {
		const { getByText } = render(ArtifactProvenance, { props: { entry: entry() } });
		expect(getByText(/Step 4/)).toBeTruthy();
		expect(getByText('plot')).toBeTruthy();
		expect(getByText('PNG')).toBeTruthy();
		expect(getByText('20.0 KB')).toBeTruthy();
	});

	it('surfaces producer params but hides the render_hint plumbing key', () => {
		const { getByText, queryByText } = render(ArtifactProvenance, {
			props: { entry: entry({ user_metadata: { render_hint: 'gp-posterior', ramp: 76, hold: 1260 } }) }
		});
		expect(getByText('ramp: 76')).toBeTruthy();
		expect(getByText('hold: 1260')).toBeTruthy();
		expect(queryByText(/render_hint/)).toBeNull();
	});

	it('renders nothing when there are no facts or params', () => {
		const { container } = render(ArtifactProvenance, {
			props: {
				entry: entry({
					process_step: null,
					category: '',
					mime_type: null,
					size_bytes: null,
					created_at: '',
					user_metadata: null
				})
			}
		});
		expect(container.textContent?.trim()).toBe('');
	});
});
