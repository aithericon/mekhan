import { describe, it, expect, vi, beforeEach } from 'vitest';

// Capture the path passed to rawJson so we can assert the query-string assembly
// (the only logic in these thin wrappers worth testing in isolation).
const rawJson = vi.fn(async (_path: string) => ({}) as unknown);
vi.mock('./client', () => ({
	rawJson: (p: string) => rawJson(p),
	ApiError: class ApiError extends Error {
		constructor(public status: number, msg: string) {
			super(msg);
		}
	}
}));

import { listDataEntries } from './data';

describe('listDataEntries query building', () => {
	beforeEach(() => rawJson.mockClear());

	it('maps category to the catalogue filter DSL and passes search/sort/paging', async () => {
		await listDataEntries({ category: 'dataset', search: 'genome', sort: '-created_at', page: 2, page_size: 25 });
		const path = rawJson.mock.calls[0][0];
		expect(path.startsWith('/data/entries?')).toBe(true);
		const qs = new URLSearchParams(path.split('?')[1]);
		expect(qs.get('filter[category][eq]')).toBe('dataset');
		expect(qs.get('search')).toBe('genome');
		expect(qs.get('sort')).toBe('-created_at');
		expect(qs.get('page')).toBe('2');
		expect(qs.get('page_size')).toBe('25');
	});

	it('omits the query string entirely when no params are given', async () => {
		await listDataEntries();
		expect(rawJson.mock.calls[0][0]).toBe('/data/entries');
	});

	it('does not emit a category filter for the "all" sentinel (caller strips it)', async () => {
		await listDataEntries({ search: 'x' });
		const path = rawJson.mock.calls[0][0];
		expect(path).not.toContain('filter[category]');
	});
});
