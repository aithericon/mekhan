import { describe, it, expect } from 'vitest';
import type { BreakdownBucket, SnapshotPoint } from '$lib/api/analytics';
import {
	toTreemapNodes,
	pivotSnapshots,
	sizeClassLabel,
	sortSizeClasses,
	ageCohortLabel,
	topNWithOther,
	dedupRatio,
	GROWTH_WINDOWS
} from './analytics-transforms';

const bucket = (key: string, bytes: number, count = 1): BreakdownBucket => ({ key, bytes, count });

describe('toTreemapNodes', () => {
	it('keeps the top-N by bytes and folds the rest into a non-drillable "other"', () => {
		const buckets = [
			bucket('a', 100, 10),
			bucket('b', 300, 30),
			bucket('c', 200, 20),
			bucket('d', 50, 5),
			bucket('e', 25, 2)
		];
		const nodes = toTreemapNodes(buckets, 2);
		expect(nodes.map((n) => n.name)).toEqual(['b', 'c', 'other (3 dirs)']);
		const other = nodes[2];
		expect(other.path).toBeNull();
		expect(other.isLeaf).toBe(true);
		expect(other.value).toBe(175);
		expect(other.count).toBe(17);
	});

	it('labels nested directory keys with the last segment but keeps the full drill path', () => {
		const nodes = toTreemapNodes([{ key: 'legacy/datasets/genomes', bytes: 10, count: 1 }]);
		expect(nodes[0].name).toBe('genomes');
		expect(nodes[0].path).toBe('legacy/datasets/genomes');
	});

	it('emits no "other" node when everything fits', () => {
		const nodes = toTreemapNodes([bucket('a', 1), bucket('b', 2)], 5);
		expect(nodes).toHaveLength(2);
		expect(nodes.every((n) => n.path !== null)).toBe(true);
	});

	it('carries the backend is_leaf flag through (drill stop)', () => {
		const nodes = toTreemapNodes([{ key: 'a', bytes: 1, count: 1, is_leaf: true }]);
		expect(nodes[0].isLeaf).toBe(true);
	});
});

describe('pivotSnapshots', () => {
	const pt = (bucket: string, server: string, total_bytes: number): SnapshotPoint => ({
		bucket,
		file_server_id: server,
		dim: 'total',
		key: '',
		file_count: 1,
		total_bytes
	});

	it('aligns per-server series on a shared sorted bucket axis with null gaps', () => {
		const points = [
			pt('2026-06-10T01:00:00Z', 's1', 100),
			pt('2026-06-10T00:00:00Z', 's1', 50),
			pt('2026-06-10T00:00:00Z', 's2', 10)
			// s2 missing at 01:00 → a real gap, NOT 0.
		];
		const { categories, series } = pivotSnapshots(points, (p) => p.total_bytes);
		expect(categories).toEqual(['2026-06-10T00:00:00Z', '2026-06-10T01:00:00Z']);
		expect(series).toEqual([
			{ name: 's1', data: [50, 100] },
			{ name: 's2', data: [10, null] }
		]);
	});

	it('returns empty axes for no points', () => {
		expect(pivotSnapshots([], (p) => p.total_bytes)).toEqual({ categories: [], series: [] });
	});
});

describe('size-class helpers', () => {
	it('sortSizeClasses orders by parsed byte magnitude, unknown last', () => {
		const shuffled = [
			bucket('≥4 GiB', 1),
			bucket('unknown', 1),
			bucket('<1 KiB', 1),
			bucket('1 MiB–64 MiB', 1),
			bucket('1 KiB–64 KiB', 1)
		];
		expect(sortSizeClasses(shuffled).map((b) => b.key)).toEqual([
			'<1 KiB',
			'1 KiB–64 KiB',
			'1 MiB–64 MiB',
			'≥4 GiB',
			'unknown'
		]);
	});

	it('orders "<X" strictly before classes starting at X', () => {
		const out = sortSizeClasses([bucket('1 KiB–64 KiB', 1), bucket('<1 KiB', 1)]);
		expect(out.map((b) => b.key)).toEqual(['<1 KiB', '1 KiB–64 KiB']);
	});

	it('sizeClassLabel prettifies only the unknown sentinel', () => {
		expect(sizeClassLabel('<1 KiB')).toBe('<1 KiB');
		expect(sizeClassLabel('unknown')).toBe('Unknown');
		expect(sizeClassLabel('')).toBe('Unknown');
	});
});

describe('ageCohortLabel', () => {
	it('expands known cohort keys and falls back to the raw key', () => {
		expect(ageCohortLabel('<7d')).toBe('Last 7 days');
		expect(ageCohortLabel('>2y')).toBe('Over 2 years');
		expect(ageCohortLabel('unknown')).toBe('Unknown');
		expect(ageCohortLabel('13mo')).toBe('13mo');
	});
});

describe('topNWithOther', () => {
	it('sorts by bytes desc and aggregates the overflow into one "other" row', () => {
		const out = topNWithOther(
			[bucket('a', 10, 1), bucket('b', 30, 3), bucket('c', 20, 2), bucket('d', 5, 7)],
			2
		);
		expect(out.map((b) => b.key)).toEqual(['b', 'c', 'other']);
		expect(out[2]).toEqual({ key: 'other', count: 8, bytes: 15 });
	});

	it('passes through untouched (sorted) when nothing overflows', () => {
		const out = topNWithOther([bucket('a', 1), bucket('b', 2)], 5);
		expect(out.map((b) => b.key)).toEqual(['b', 'a']);
	});

	it('honors a custom valueOf (count mode)', () => {
		const out = topNWithOther(
			[bucket('big-bytes', 100, 1), bucket('many-files', 1, 100)],
			1,
			(b) => b.count
		);
		expect(out[0].key).toBe('many-files');
	});
});

describe('dedupRatio', () => {
	it('is physical ÷ logical bytes (2.0 = each byte stored twice)', () => {
		expect(dedupRatio(200, 100)).toBe(2);
		expect(dedupRatio(100, 100)).toBe(1);
	});

	it('is null when nothing is catalogued (or inputs are not finite)', () => {
		expect(dedupRatio(0, 0)).toBeNull();
		expect(dedupRatio(10, 0)).toBeNull();
		expect(dedupRatio(Number.NaN, 10)).toBeNull();
	});
});

describe('GROWTH_WINDOWS', () => {
	it('offers 24h/7d/30d with chart-friendly bucket counts', () => {
		expect(GROWTH_WINDOWS.map((w) => w.label)).toEqual(['24h', '7d', '30d']);
		for (const w of GROWTH_WINDOWS) {
			const buckets = w.windowSecs / w.bucketSecs;
			expect(buckets).toBeGreaterThanOrEqual(20);
			expect(buckets).toBeLessThanOrEqual(120);
		}
	});
});
