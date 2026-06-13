import { clsx, type ClassValue } from 'clsx';
import { twMerge } from 'tailwind-merge';
import type { ColumnDef } from '@tanstack/table-core';

export function cn(...inputs: ClassValue[]) {
	return twMerge(clsx(inputs));
}

/**
 * Column definitions for the report data table: one column per header,
 * addressing string[][] rows by index. Cells are strings by contract
 * (TaskBlock 'table'); the `alphanumeric` sorting fn gives natural ordering
 * ("9" < "10") without per-column type hints. Ragged rows sort as ''.
 */
export function tableColumns(headers: string[]): ColumnDef<string[]>[] {
	return headers.map((_, i) => ({
		id: String(i),
		accessorFn: (row: string[]) => row[i] ?? '',
		sortingFn: 'alphanumeric'
	}));
}

/** Stringify one table cell: strings pass through, scalars via String(),
 * null/undefined to '', anything structured to JSON. */
function stringifyCell(value: unknown): string {
	if (value == null) return '';
	if (typeof value === 'string') return value;
	if (typeof value === 'number' || typeof value === 'boolean') return String(value);
	return JSON.stringify(value);
}

/**
 * Resolve a table block's rows. `rows_ref` (a `<slug>.<field>[.<more>…]`
 * dotted path, ≥2 segments — same grammar the compiler stages into the
 * task payload) wins over the static `rows`; when it is absent, malformed,
 * or doesn't resolve to an array in `taskData`, fall back to `rows`
 * (empty table when neither is usable). Cells are stringified so upstream
 * numeric matrices render without a producer-side formatting pass.
 */
export function resolveTableRows(
	block: { rows?: string[][]; rows_ref?: string },
	taskData?: Record<string, unknown>
): string[][] {
	const ref = block.rows_ref?.trim();
	if (ref && !ref.includes('[*]')) {
		const path = ref.split('.');
		if (path.length >= 2 && path.every((s) => s.length > 0)) {
			let cur: unknown = taskData;
			for (const seg of path) {
				if (cur == null || typeof cur !== 'object') {
					cur = undefined;
					break;
				}
				cur = (cur as Record<string, unknown>)[seg];
			}
			if (Array.isArray(cur)) {
				return cur.map((row) =>
					Array.isArray(row) ? row.map(stringifyCell) : [stringifyCell(row)]
				);
			}
		}
	}
	return block.rows ?? [];
}

/** Format a byte count into a human-readable size string. */
export function displaySize(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
	if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
	return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}
