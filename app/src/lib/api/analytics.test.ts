import { describe, it, expect, vi, beforeEach } from 'vitest';

// Capture the path + init passed to rawJson so we can assert the query-string
// assembly (the only logic in these thin wrappers worth testing in isolation).
const rawJson = vi.fn(async (_path: string, _init?: RequestInit) => ({}) as unknown);
vi.mock('./client', () => ({
	rawJson: (p: string, i?: RequestInit) => rawJson(p, i),
	ApiError: class ApiError extends Error {
		constructor(
			public status: number,
			msg: string
		) {
			super(msg);
		}
	}
}));

import {
	getAnalyticsBreakdown,
	getAnalyticsTimeseries,
	triggerAnalyticsSnapshot
} from './analytics';

describe('getAnalyticsBreakdown query building', () => {
	beforeEach(() => rawJson.mockClear());

	it('always sets group_by and maps scope filters to the inventory filter DSL', async () => {
		await getAnalyticsBreakdown({
			group_by: 'extension',
			file_server_id: 'lab-nas-1',
			status: 'verified',
			search: 'genome',
			limit: 50
		});
		const path = rawJson.mock.calls[0][0];
		expect(path.startsWith('/data/analytics/breakdown?')).toBe(true);
		const qs = new URLSearchParams(path.split('?')[1]);
		expect(qs.get('group_by')).toBe('extension');
		expect(qs.get('filter[file_server_id][eq]')).toBe('lab-nas-1');
		expect(qs.get('filter[status][eq]')).toBe('verified');
		expect(qs.get('search')).toBe('genome');
		expect(qs.get('limit')).toBe('50');
	});

	it('passes under/depth for directory drill-down', async () => {
		await getAnalyticsBreakdown({ group_by: 'directory', under: 'legacy/datasets/', depth: 1 });
		const qs = new URLSearchParams(rawJson.mock.calls[0][0].split('?')[1]);
		expect(qs.get('group_by')).toBe('directory');
		expect(qs.get('under')).toBe('legacy/datasets/');
		expect(qs.get('depth')).toBe('1');
	});

	it('omits empty optional params entirely', async () => {
		await getAnalyticsBreakdown({ group_by: 'server', file_server_id: '', under: '' });
		const path = rawJson.mock.calls[0][0];
		expect(path).toBe('/data/analytics/breakdown?group_by=server');
	});
});

describe('getAnalyticsTimeseries query building', () => {
	beforeEach(() => rawJson.mockClear());

	it('passes dim/key/server scope and window/bucket widths', async () => {
		await getAnalyticsTimeseries({
			dim: 'extension',
			key: 'csv',
			file_server_id: 'lab-nas-1',
			bucket_secs: 3600,
			window_secs: 86400
		});
		const path = rawJson.mock.calls[0][0];
		expect(path.startsWith('/data/analytics/timeseries?')).toBe(true);
		const qs = new URLSearchParams(path.split('?')[1]);
		expect(qs.get('dim')).toBe('extension');
		expect(qs.get('key')).toBe('csv');
		expect(qs.get('file_server_id')).toBe('lab-nas-1');
		expect(qs.get('bucket_secs')).toBe('3600');
		expect(qs.get('window_secs')).toBe('86400');
	});

	it('sends only dim when nothing else is scoped', async () => {
		await getAnalyticsTimeseries({ dim: 'total' });
		expect(rawJson.mock.calls[0][0]).toBe('/data/analytics/timeseries?dim=total');
	});
});

describe('triggerAnalyticsSnapshot', () => {
	beforeEach(() => rawJson.mockClear());

	it('POSTs to the snapshot endpoint', async () => {
		await triggerAnalyticsSnapshot();
		expect(rawJson.mock.calls[0][0]).toBe('/data/analytics/snapshot');
		expect(rawJson.mock.calls[0][1]).toMatchObject({ method: 'POST' });
	});
});
