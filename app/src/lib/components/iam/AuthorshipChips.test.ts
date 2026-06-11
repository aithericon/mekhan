/**
 * DOM tests for AuthorshipChips — the created/updated "who·when" footer.
 *
 * The profile cache is mocked so the embedded UserChip resolves synchronously
 * offline (no batch fetch under jsdom). What this locks in: the "Updated by"
 * line appears ONLY when the last mutation differs from creation (different
 * mutator, or a meaningfully later timestamp), and a null mutator with a later
 * timestamp renders as the literal "System".
 */
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';

vi.mock('$lib/stores/profiles.svelte', () => ({
	profiles: {
		ensure: vi.fn(),
		seed: vi.fn(),
		get: vi.fn(() => null) // resolved-but-missing → UserChip falls back to short UUID
	}
}));

import AuthorshipChips from './AuthorshipChips.svelte';

afterEach(() => cleanup());

describe('AuthorshipChips', () => {
	it('shows only a Created line when nothing meaningful changed', () => {
		const { getByText, queryByTestId } = render(AuthorshipChips, {
			props: {
				createdBy: 'aaaaaaaa-0000-0000-0000-000000000000',
				createdAt: '2026-06-11T10:00:00Z',
				updatedBy: 'aaaaaaaa-0000-0000-0000-000000000000',
				updatedAt: '2026-06-11T10:00:00Z'
			}
		});
		expect(getByText('Created by')).toBeTruthy();
		expect(queryByTestId('authorship-updated')).toBeNull();
	});

	it('shows an Updated line when the mutator differs', () => {
		const { getByTestId } = render(AuthorshipChips, {
			props: {
				createdBy: 'aaaaaaaa-0000-0000-0000-000000000000',
				createdAt: '2026-06-11T10:00:00Z',
				updatedBy: 'bbbbbbbb-0000-0000-0000-000000000000',
				updatedAt: '2026-06-11T11:00:00Z'
			}
		});
		expect(getByTestId('authorship-updated')).toBeTruthy();
	});

	it('renders System for a null mutator with a later timestamp', () => {
		const { getByTestId, getByText } = render(AuthorshipChips, {
			props: {
				createdBy: 'aaaaaaaa-0000-0000-0000-000000000000',
				createdAt: '2026-06-11T10:00:00Z',
				updatedBy: null,
				updatedAt: '2026-06-11T12:00:00Z'
			}
		});
		expect(getByTestId('authorship-updated')).toBeTruthy();
		expect(getByText('System')).toBeTruthy();
	});
});
