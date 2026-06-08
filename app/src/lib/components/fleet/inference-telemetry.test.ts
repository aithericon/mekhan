import { describe, it, expect } from 'vitest';
import {
	pivotTimeseries,
	TIMESERIES_METRICS,
	bucketLabel,
	compact
} from './inference-telemetry';
import type { InferenceTimeseriesPoint } from '$lib/api/inference';

const pt = (
	bucket: string,
	model_id: string,
	over: Partial<InferenceTimeseriesPoint> = {}
): InferenceTimeseriesPoint => ({
	bucket,
	model_id,
	requests: 0,
	completed: 0,
	errors: 0,
	prompt_tokens: 0,
	completion_tokens: 0,
	p50_ms: null,
	p95_ms: null,
	...over
});

describe('pivotTimeseries', () => {
	const points = [
		pt('2026-06-08T10:00:00Z', 'a', { requests: 3, p50_ms: 100 }),
		pt('2026-06-08T10:01:00Z', 'a', { requests: 5, p50_ms: 200 }),
		// model b only appears in the second bucket
		pt('2026-06-08T10:01:00Z', 'b', { requests: 7, p50_ms: 50 })
	];

	it('aligns every model on the shared sorted bucket axis', () => {
		const { categories, series } = pivotTimeseries(points, (p) => p.requests, 'zero');
		expect(categories).toEqual(['2026-06-08T10:00:00Z', '2026-06-08T10:01:00Z']);
		expect(series.map((s) => s.name)).toEqual(['a', 'b']);
		expect(series.find((s) => s.name === 'a')!.data).toEqual([3, 5]);
	});

	it("fills a missing (bucket, model) cell with 0 for count metrics", () => {
		const { series } = pivotTimeseries(points, (p) => p.requests, 'zero');
		// b is absent in bucket 0 → 0
		expect(series.find((s) => s.name === 'b')!.data).toEqual([0, 7]);
	});

	it('fills a missing cell with null (a gap) for latency/rate metrics', () => {
		const { series } = pivotTimeseries(points, (p) => p.p50_ms ?? null, 'gap');
		expect(series.find((s) => s.name === 'b')!.data).toEqual([null, 50]);
		expect(series.find((s) => s.name === 'a')!.data).toEqual([100, 200]);
	});
});

describe('TIMESERIES_METRICS', () => {
	const p = pt('t', 'm', {
		requests: 10,
		errors: 2,
		prompt_tokens: 30,
		completion_tokens: 70,
		p50_ms: 120,
		p95_ms: 480
	});
	const by = (k: string) => TIMESERIES_METRICS.find((m) => m.key === k)!;

	it('derives error rate as a percentage of requests', () => {
		expect(by('error_pct').valueOf(p)).toBe(20);
		// guard divide-by-zero → gap
		expect(by('error_pct').valueOf(pt('t', 'm', { requests: 0, errors: 0 }))).toBeNull();
	});

	it('sums prompt + completion tokens', () => {
		expect(by('tokens').valueOf(p)).toBe(100);
	});

	it('passes latency percentiles through, gapping nulls', () => {
		expect(by('p95_ms').valueOf(p)).toBe(480);
		expect(by('p50_ms').valueOf(pt('t', 'm'))).toBeNull();
	});
});

describe('formatting', () => {
	it('compacts large numbers', () => {
		expect(compact(950)).toBe('950');
		expect(compact(1500)).toBe('1.5k');
		expect(compact(2_300_000)).toBe('2.3M');
	});
	it('renders a bucket ISO as HH:MM', () => {
		// local-time formatting; assert the shape, not a fixed zone
		expect(bucketLabel('2026-06-08T10:05:00Z')).toMatch(/^\d{2}:\d{2}$/);
		expect(bucketLabel('not-a-date')).toBe('not-a-date');
	});
});
