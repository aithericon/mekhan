import { describe, it, expect } from 'vitest';
import type { TemplateRunTimeseriesPoint } from '$lib/api/templates-analytics';
import {
	pivotTemplateTimeseries,
	nodeKindBuckets,
	formatRunDuration,
	formatSuccessRate
} from './template-analytics-transforms';

// ── pivotTemplateTimeseries ──────────────────────────────────────────────────

const point = (
	bucket: string,
	outcome: string,
	run_count: number
): TemplateRunTimeseriesPoint => ({ bucket, outcome, run_count });

describe('pivotTemplateTimeseries', () => {
	it('aligns per-outcome series on a shared sorted bucket axis with null gaps', () => {
		const points = [
			point('2026-06-10T00:00:00Z', 'success', 5),
			point('2026-06-10T01:00:00Z', 'success', 3),
			point('2026-06-10T00:00:00Z', 'failure', 2)
			// failure missing at 01:00 → null, not 0
		];
		const { categories, series } = pivotTemplateTimeseries(points);
		expect(categories).toEqual(['2026-06-10T00:00:00Z', '2026-06-10T01:00:00Z']);
		const successSeries = series.find((s) => s.name === 'success');
		const failureSeries = series.find((s) => s.name === 'failure');
		expect(successSeries?.data).toEqual([5, 3]);
		expect(failureSeries?.data).toEqual([2, null]);
	});

	it('returns empty categories and series for no points', () => {
		const { categories, series } = pivotTemplateTimeseries([]);
		expect(categories).toEqual([]);
		expect(series).toEqual([]);
	});

	it('orders outcomes success → failure → cancelled', () => {
		const points = [
			point('2026-06-10T00:00:00Z', 'cancelled', 1),
			point('2026-06-10T00:00:00Z', 'failure', 2),
			point('2026-06-10T00:00:00Z', 'success', 3)
		];
		const { series } = pivotTemplateTimeseries(points);
		expect(series.map((s) => s.name)).toEqual(['success', 'failure', 'cancelled']);
	});

	it('handles a single outcome across multiple buckets', () => {
		const points = [
			point('2026-06-10T00:00:00Z', 'success', 10),
			point('2026-06-10T01:00:00Z', 'success', 20)
		];
		const { categories, series } = pivotTemplateTimeseries(points);
		expect(categories).toHaveLength(2);
		expect(series).toHaveLength(1);
		expect(series[0].data).toEqual([10, 20]);
	});
});

// ── nodeKindBuckets ──────────────────────────────────────────────────────────

describe('nodeKindBuckets', () => {
	it('maps snake_case keys to Title case labels, bytes=0, sorted by count desc', () => {
		const buckets = nodeKindBuckets({
			automated_step: 4,
			human_task: 2,
			start: 1,
			end: 1
		});
		expect(buckets.map((b) => b.key)).toEqual([
			'Automated step',
			'Human task',
			'Start',
			'End'
		]);
		expect(buckets.every((b) => b.bytes === 0)).toBe(true);
		expect(buckets[0].count).toBe(4);
	});

	it('returns empty array for an empty map', () => {
		expect(nodeKindBuckets({})).toEqual([]);
	});

	it('capitalises only the first character of the label', () => {
		const [{ key }] = nodeKindBuckets({ sub_workflow: 1 });
		expect(key).toBe('Sub workflow');
	});
});

// ── formatRunDuration ────────────────────────────────────────────────────────

describe('formatRunDuration', () => {
	it('formats sub-second as ms', () => {
		expect(formatRunDuration(300)).toBe('300ms');
	});
	it('formats seconds with one decimal', () => {
		expect(formatRunDuration(4200)).toBe('4.2s');
	});
	it('formats minutes + seconds', () => {
		expect(formatRunDuration(4 * 60_000 + 12_000)).toBe('4m 12s');
	});
	it('omits seconds when zero', () => {
		expect(formatRunDuration(4 * 60_000)).toBe('4m');
	});
	it('formats hours + minutes', () => {
		expect(formatRunDuration(1.05 * 3_600_000)).toBe('1h 3m');
	});
	it('returns — for null, undefined, NaN, negative', () => {
		expect(formatRunDuration(null)).toBe('—');
		expect(formatRunDuration(undefined)).toBe('—');
		expect(formatRunDuration(Number.NaN)).toBe('—');
		expect(formatRunDuration(-1)).toBe('—');
	});
});

// ── formatSuccessRate ────────────────────────────────────────────────────────

describe('formatSuccessRate', () => {
	it('converts 0..1 fraction to percentage string', () => {
		expect(formatSuccessRate(0.875)).toBe('87.5%');
		expect(formatSuccessRate(1)).toBe('100.0%');
		expect(formatSuccessRate(0)).toBe('0.0%');
	});
	it('returns — for null/undefined', () => {
		expect(formatSuccessRate(null)).toBe('—');
		expect(formatSuccessRate(undefined)).toBe('—');
	});
});
