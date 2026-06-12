/**
 * Pure-logic tests for the editor toolbar's Runs menu helpers. What this locks
 * in: the family id follows the backend's `chain_root_id` rule
 * (`base_template_id ?? id`), the "View all" deep-link keeps `mode=any` (so
 * draft/test runs the menu showed don't vanish behind the list's live-only
 * default), and the row label prefers the real start time over creation.
 */
import { describe, it, expect } from 'vitest';
import { templateFamilyId, allRunsHref, runWhenLabel } from './runs-menu';

describe('templateFamilyId', () => {
	it('uses base_template_id for a forked version row', () => {
		expect(templateFamilyId({ id: 'v2-id', base_template_id: 'root-id' })).toBe('root-id');
	});

	it('falls back to the row id for a chain root', () => {
		expect(templateFamilyId({ id: 'root-id', base_template_id: null })).toBe('root-id');
		expect(templateFamilyId({ id: 'root-id' })).toBe('root-id');
	});
});

describe('allRunsHref', () => {
	it('scopes the instances list to the family with mode=any', () => {
		expect(allRunsHref('abc-123')).toBe('/instances?template_family=abc-123&mode=any');
	});

	it('URL-encodes the id', () => {
		expect(allRunsHref('a&b')).toBe('/instances?template_family=a%26b&mode=any');
	});
});

describe('runWhenLabel', () => {
	const now = new Date('2026-06-12T12:00:00Z');

	it('prefers started_at when the run actually started', () => {
		expect(
			runWhenLabel(
				{ started_at: '2026-06-12T11:55:00Z', created_at: '2026-06-12T10:00:00Z' },
				now
			)
		).toBe('started 5m ago');
	});

	it('falls back to created_at for a run that never started', () => {
		expect(runWhenLabel({ started_at: null, created_at: '2026-06-12T09:00:00Z' }, now)).toBe(
			'created 3h ago'
		);
	});
});
