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

/** Format a byte count into a human-readable size string. */
export function displaySize(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
	if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
	return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}
