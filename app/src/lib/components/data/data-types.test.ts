import { describe, it, expect, vi, beforeEach } from 'vitest';
import type { CatalogueDataType } from '$lib/api/data';

const listDataTypes = vi.fn(async (): Promise<CatalogueDataType[]> => []);
const createDataType = vi.fn();
const updateDataType = vi.fn();
const deleteDataType = vi.fn();
vi.mock('$lib/api/data', () => ({
	listDataTypes: () => listDataTypes(),
	createDataType: (b: unknown) => createDataType(b),
	updateDataType: (id: string, b: unknown) => updateDataType(id, b),
	deleteDataType: (id: string) => deleteDataType(id)
}));

import { DataTypesState } from './data-types.svelte';

function dt(over: Partial<CatalogueDataType> = {}): CatalogueDataType {
	return {
		id: 'dt-1',
		name: 'sensor readings',
		columns: [{ name: 'ts', data_type: 'timestamp<UTC>', nullable: false }],
		digests: ['a1b2c3d4e5f60718'],
		entry_count: 3,
		created_at: '2026-06-11T00:00:00Z',
		updated_at: '2026-06-11T00:00:00Z',
		...over
	};
}

describe('DataTypesState', () => {
	beforeEach(() => {
		listDataTypes.mockReset().mockResolvedValue([]);
		createDataType.mockReset();
		updateDataType.mockReset();
		deleteDataType.mockReset();
	});

	it('load() populates the list and derives byDigest + names', async () => {
		const a = dt();
		const b = dt({ id: 'dt-2', name: 'logs', digests: ['ffff000011112222', '0123456789abcdef'] });
		listDataTypes.mockResolvedValue([a, b]);
		const s = new DataTypesState();
		expect(s.loading).toBe(true);
		await s.load();
		expect(s.loading).toBe(false);
		expect(s.error).toBeNull();
		expect(s.list).toEqual([a, b]);
		expect(s.byDigest.get('a1b2c3d4e5f60718')?.name).toBe('sensor readings');
		expect(s.byDigest.get('0123456789abcdef')?.name).toBe('logs');
		expect(s.names).toEqual(new Set(['sensor readings', 'logs']));
	});

	it('load() failure sets error but keeps the last good list', async () => {
		listDataTypes.mockResolvedValue([dt()]);
		const s = new DataTypesState();
		await s.load();
		listDataTypes.mockRejectedValue(new Error('boom'));
		await s.load();
		expect(s.error).toBe('boom');
		expect(s.loading).toBe(false);
		expect(s.list).toHaveLength(1);
	});

	it('resolveDigests is bound: works detached, unknown name → undefined', async () => {
		listDataTypes.mockResolvedValue([dt()]);
		const s = new DataTypesState();
		await s.load();
		const { resolveDigests } = s; // detached, as compileQuery receives it
		expect(resolveDigests('sensor readings')).toEqual(['a1b2c3d4e5f60718']);
		expect(resolveDigests('nope')).toBeUndefined();
	});

	it('promote() creates, reloads, and returns the created type', async () => {
		const created = dt({ name: 'fresh' });
		createDataType.mockResolvedValue(created);
		listDataTypes.mockResolvedValue([created]);
		const s = new DataTypesState();
		const out = await s.promote({ digest: 'a1b2c3d4e5f60718', name: 'fresh' });
		expect(out).toEqual(created);
		expect(createDataType).toHaveBeenCalledWith({ digest: 'a1b2c3d4e5f60718', name: 'fresh' });
		expect(listDataTypes).toHaveBeenCalledTimes(1);
		expect(s.names.has('fresh')).toBe(true);
	});

	it('update() patches then reloads', async () => {
		updateDataType.mockResolvedValue(dt({ name: 'renamed' }));
		const s = new DataTypesState();
		await s.update('dt-1', { name: 'renamed' });
		expect(updateDataType).toHaveBeenCalledWith('dt-1', { name: 'renamed' });
		expect(listDataTypes).toHaveBeenCalledTimes(1);
	});

	it('remove() deletes then reloads', async () => {
		deleteDataType.mockResolvedValue(undefined);
		const s = new DataTypesState();
		await s.remove('dt-1');
		expect(deleteDataType).toHaveBeenCalledWith('dt-1');
		expect(listDataTypes).toHaveBeenCalledTimes(1);
	});

	it('mutation failure rethrows (dialogs handle 409 etc.) and skips the reload', async () => {
		createDataType.mockRejectedValue(new Error('conflict'));
		const s = new DataTypesState();
		await expect(s.promote({ digest: 'x', name: 'dup' })).rejects.toThrow('conflict');
		expect(listDataTypes).not.toHaveBeenCalled();
	});
});
