/**
 * Pure helpers for the per-template analytics page — kept DOM-free so they
 * are unit-testable and can't drift from the charts that consume them.
 * Mirrors the conventions of `data/analytics-transforms.ts`.
 */
import type { TemplateRunTimeseriesPoint } from '$lib/api/templates-analytics';
import type { BreakdownBucket } from '$lib/api/analytics';

// ── Timeseries pivot ─────────────────────────────────────────────────────────

export interface TimeseriesPivot {
	/** Sorted unique bucket timestamps (ISO strings) — the x-axis categories. */
	categories: string[];
	/** One aligned series per outcome (`success`, `failure`, `cancelled`). */
	series: { name: string; data: (number | null)[] }[];
}

/**
 * Reshape flat `(bucket, outcome, run_count)` rows into per-outcome series
 * aligned on a shared sorted bucket axis.  Missing `(bucket, outcome)` cells
 * are genuine gaps (zero runs in that slot), represented as `null` — the
 * GrowthChart honours `connectNulls: false` for visual clarity.
 */
export function pivotTemplateTimeseries(
	points: TemplateRunTimeseriesPoint[]
): TimeseriesPivot {
	const bucketSet = new Set<string>();
	const outcomeSet = new Set<string>();
	const byKey = new Map<string, number>();
	for (const p of points) {
		bucketSet.add(p.bucket);
		outcomeSet.add(p.outcome);
		byKey.set(`${p.bucket}\x00${p.outcome}`, p.run_count);
	}
	const categories = [...bucketSet].sort();
	// Deterministic outcome order: success → failure → cancelled.
	const OUTCOME_ORDER = ['success', 'failure', 'cancelled'];
	const outcomes = [...outcomeSet].sort(
		(a, b) =>
			(OUTCOME_ORDER.indexOf(a) === -1 ? 99 : OUTCOME_ORDER.indexOf(a)) -
			(OUTCOME_ORDER.indexOf(b) === -1 ? 99 : OUTCOME_ORDER.indexOf(b))
	);
	const series = outcomes.map((o) => ({
		name: o,
		data: categories.map((b) => byKey.get(`${b}\x00${o}`) ?? null)
	}));
	return { categories, series };
}

// ── Node-kind breakdown ──────────────────────────────────────────────────────

/**
 * Convert the `TemplateMetrics.node_kind_counts` map into a `BreakdownBucket[]`
 * suitable for `BreakdownChart` in `mode="count"`.  `bytes` is stubbed to 0 —
 * only `count` is meaningful here.
 *
 * Keys are prettified from snake_case to "Title Case" so labels read naturally
 * in the chart legend (`automated_step` → `Automated step`).
 */
export function nodeKindBuckets(counts: Record<string, number>): BreakdownBucket[] {
	return Object.entries(counts)
		.map(([key, count]) => ({
			key: key.replace(/_/g, ' ').replace(/^\w/, (c) => c.toUpperCase()),
			bytes: 0,
			count
		}))
		.sort((a, b) => b.count - a.count);
}

// ── Duration formatting ──────────────────────────────────────────────────────

/**
 * Compact human-readable run wall-clock (`300ms`, `4.2s`, `4m 12s`, `1h 3m`).
 * Returns `'—'` for null / undefined / NaN / negative.
 */
export function formatRunDuration(ms: number | null | undefined): string {
	if (ms == null || !Number.isFinite(ms) || ms < 0) return '—';
	if (ms < 1000) return `${Math.round(ms)}ms`;
	if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
	if (ms < 3_600_000) {
		const m = Math.floor(ms / 60_000);
		const s = Math.round((ms % 60_000) / 1000);
		return s ? `${m}m ${s}s` : `${m}m`;
	}
	const h = Math.floor(ms / 3_600_000);
	const m = Math.round((ms % 3_600_000) / 60_000);
	return m ? `${h}h ${m}m` : `${h}h`;
}

/**
 * Format a success rate (0..1) as a percentage string, e.g. `"87.5%"`.
 * Returns `'—'` for null / undefined.
 */
export function formatSuccessRate(rate: number | null | undefined): string {
	if (rate == null || !Number.isFinite(rate)) return '—';
	return `${(rate * 100).toFixed(1)}%`;
}
