import { describe, it, expect, vi, beforeEach } from 'vitest';

// Capture the path passed to rawJson so we can assert the query-string assembly
// (the only logic in these thin wrappers worth testing in isolation).
const rawJson = vi.fn(async (_path: string, _init?: RequestInit) => ({}) as unknown);
vi.mock('./client', () => ({
	rawJson: (p: string, init?: RequestInit) => rawJson(p, init),
	ApiError: class ApiError extends Error {
		constructor(public status: number, msg: string) {
			super(msg);
		}
	}
}));

const authFetch = vi.fn(
	async (_path: string, _init?: RequestInit) => new Response(null, { status: 204 })
);
vi.mock('$lib/auth/fetch', () => ({
	authFetch: (p: string, init?: RequestInit) => authFetch(p, init)
}));

import {
	listDataEntries,
	getCatalogueFacets,
	getCatalogueQueryFields,
	listSavedQueries,
	createSavedQuery,
	updateSavedQuery,
	deleteSavedQuery,
	listDataTypes,
	createDataType,
	updateDataType,
	deleteDataType
} from './data';

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

	it('compiles generic filter triples to filter[FIELD][OP]=VALUE alongside legacy params', async () => {
		await listDataEntries({
			category: 'dataset',
			filters: [
				{ field: 'meta.num_rows', op: 'gte', value: '1000' },
				{ field: 'size_bytes', op: 'lt', value: '1048576' }
			],
			file_metadata: '{"format":"csv"}'
		});
		const path = rawJson.mock.calls[0][0];
		const qs = new URLSearchParams(path.split('?')[1]);
		expect(qs.get('filter[category][eq]')).toBe('dataset');
		expect(qs.get('filter[meta.num_rows][gte]')).toBe('1000');
		expect(qs.get('filter[size_bytes][lt]')).toBe('1048576');
		expect(qs.get('file_metadata')).toBe('{"format":"csv"}');
	});
});

describe('getCatalogueFacets query building', () => {
	beforeEach(() => rawJson.mockClear());

	it('sets group_by/limit and compiles filter triples + search + file_metadata', async () => {
		await getCatalogueFacets({
			group_by: 'format',
			limit: 50,
			search: 'genome',
			filters: [{ field: 'source_net', op: 'eq', value: 'net-1' }],
			file_metadata: '{"column_names":["email"]}'
		});
		const path = rawJson.mock.calls[0][0];
		expect(path.startsWith('/catalogue/facets?')).toBe(true);
		const qs = new URLSearchParams(path.split('?')[1]);
		expect(qs.get('group_by')).toBe('format');
		expect(qs.get('limit')).toBe('50');
		expect(qs.get('search')).toBe('genome');
		expect(qs.get('filter[source_net][eq]')).toBe('net-1');
		expect(qs.get('file_metadata')).toBe('{"column_names":["email"]}');
	});

	it('omits the optional params when absent', async () => {
		await getCatalogueFacets({ group_by: 'category' });
		const path = rawJson.mock.calls[0][0];
		const qs = new URLSearchParams(path.split('?')[1]);
		expect(qs.get('group_by')).toBe('category');
		expect(qs.has('limit')).toBe(false);
		expect(qs.has('search')).toBe(false);
		expect(qs.has('file_metadata')).toBe(false);
	});
});

describe('getCatalogueQueryFields', () => {
	it('hits /catalogue/query-fields and caches the promise module-level', async () => {
		rawJson.mockClear();
		await getCatalogueQueryFields();
		await getCatalogueQueryFields();
		const calls = rawJson.mock.calls.filter((c) => c[0] === '/catalogue/query-fields');
		expect(calls.length).toBe(1);
	});
});

describe('saved queries CRUD', () => {
	beforeEach(() => {
		rawJson.mockClear();
		authFetch.mockClear();
	});

	it('lists from /catalogue/saved-queries', async () => {
		await listSavedQueries();
		expect(rawJson.mock.calls[0][0]).toBe('/catalogue/saved-queries');
	});

	it('creates with POST + JSON body', async () => {
		await createSavedQuery({ name: 'csv heavies', q: 'filter[meta.format][eq]=csv' });
		const [path, init] = rawJson.mock.calls[0];
		expect(path).toBe('/catalogue/saved-queries');
		expect(init?.method).toBe('POST');
		expect(JSON.parse(init?.body as string)).toEqual({
			name: 'csv heavies',
			q: 'filter[meta.format][eq]=csv'
		});
	});

	it('updates with PATCH to /catalogue/saved-queries/{id}', async () => {
		await updateSavedQuery('sq-1', { name: 'renamed' });
		const [path, init] = rawJson.mock.calls[0];
		expect(path).toBe('/catalogue/saved-queries/sq-1');
		expect(init?.method).toBe('PATCH');
		expect(JSON.parse(init?.body as string)).toEqual({ name: 'renamed' });
	});

	it('deletes with DELETE (204, no JSON parse)', async () => {
		await deleteSavedQuery('sq-2');
		const [path, init] = authFetch.mock.calls[0];
		expect(path).toBe('/api/v1/catalogue/saved-queries/sq-2');
		expect(init?.method).toBe('DELETE');
		expect(rawJson).not.toHaveBeenCalled();
	});
});

describe('data types CRUD', () => {
	beforeEach(() => {
		rawJson.mockClear();
		authFetch.mockClear();
	});

	it('lists from /catalogue/data-types', async () => {
		await listDataTypes();
		expect(rawJson.mock.calls[0][0]).toBe('/catalogue/data-types');
	});

	it('promotes with POST + JSON body', async () => {
		await createDataType({ digest: 'a1b2c3d4e5f60718', name: 'sensor readings' });
		const [path, init] = rawJson.mock.calls[0];
		expect(path).toBe('/catalogue/data-types');
		expect(init?.method).toBe('POST');
		expect(JSON.parse(init?.body as string)).toEqual({
			digest: 'a1b2c3d4e5f60718',
			name: 'sensor readings'
		});
	});

	it('updates with PATCH to /catalogue/data-types/{id}', async () => {
		await updateDataType('dt-1', { name: 'renamed', attach_digests: ['ffff000011112222'] });
		const [path, init] = rawJson.mock.calls[0];
		expect(path).toBe('/catalogue/data-types/dt-1');
		expect(init?.method).toBe('PATCH');
		expect(JSON.parse(init?.body as string)).toEqual({
			name: 'renamed',
			attach_digests: ['ffff000011112222']
		});
	});

	it('deletes with DELETE (204, no JSON parse)', async () => {
		await deleteDataType('dt-2');
		const [path, init] = authFetch.mock.calls[0];
		expect(path).toBe('/api/v1/catalogue/data-types/dt-2');
		expect(init?.method).toBe('DELETE');
		expect(rawJson).not.toHaveBeenCalled();
	});

	it('delete surfaces a non-2xx as ApiError', async () => {
		authFetch.mockResolvedValueOnce(new Response('not found', { status: 404 }));
		await expect(deleteDataType('dt-missing')).rejects.toMatchObject({ status: 404 });
	});
});
