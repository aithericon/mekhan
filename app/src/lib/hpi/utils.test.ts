/**
 * Tests for the hpi report-table column defs (`tableColumns`).
 *
 * Following this codebase's convention, we test the pure helper rather than
 * mounting the component. The component is a thin render over a TanStack
 * table built from these columns, so exercising them through a headless
 * `createTable` instance covers the behaviour that matters:
 *
 *   - natural (alphanumeric) ordering on string cells — "9" < "10", so
 *     numeric report columns sort sensibly without type hints,
 *   - ragged rows: a missing cell sorts as '' instead of throwing,
 *   - one column per header, addressed by index.
 */
import { describe, it, expect } from 'vitest';
import {
	createTable,
	getCoreRowModel,
	getSortedRowModel,
	type SortingState,
	type TableState
} from '@tanstack/table-core';
import { tableColumns } from './utils';

function sortedColumn(headers: string[], rows: string[][], sorting: SortingState): string[][] {
	let state = { sorting } as TableState;
	const table = createTable<string[]>({
		data: rows,
		columns: tableColumns(headers),
		state,
		onStateChange: (updater) => {
			state = typeof updater === 'function' ? updater(state) : updater;
		},
		renderFallbackValue: null,
		getCoreRowModel: getCoreRowModel(),
		getSortedRowModel: getSortedRowModel()
	});
	return table.getRowModel().rows.map((r) => r.original);
}

describe('tableColumns', () => {
	const headers = ['Sample', 'Temp (°C)'];

	it('builds one index-addressed column per header', () => {
		expect(tableColumns(headers).map((c) => c.id)).toEqual(['0', '1']);
		const [row] = sortedColumn(headers, [['a', 'b']], []);
		expect(row).toEqual(['a', 'b']);
	});

	it('sorts numerically-valued string cells naturally ("9" < "10")', () => {
		const rows = [
			['s1', '1210'],
			['s2', '980'],
			['s3', '1115']
		];
		const sorted = sortedColumn(headers, rows, [{ id: '1', desc: false }]);
		expect(sorted.map((r) => r[1])).toEqual(['980', '1115', '1210']);
		const desc = sortedColumn(headers, rows, [{ id: '1', desc: true }]);
		expect(desc.map((r) => r[1])).toEqual(['1210', '1115', '980']);
	});

	it('treats missing cells in ragged rows as empty strings', () => {
		const rows = [['s1', '42'], ['s2'], ['s3', '7']];
		const sorted = sortedColumn(headers, rows, [{ id: '1', desc: false }]);
		expect(sorted.map((r) => r[0])).toEqual(['s2', 's3', 's1']);
	});

	it('leaves rows in input order when no sorting is applied', () => {
		const rows = [
			['b', '2'],
			['a', '1']
		];
		const sorted = sortedColumn(headers, rows, []);
		expect(sorted.map((r) => r[0])).toEqual(['b', 'a']);
	});
});
