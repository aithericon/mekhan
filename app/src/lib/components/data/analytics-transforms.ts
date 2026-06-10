/**
 * Pure helpers for the Data Analytics tab — kept DOM-free so they're
 * unit-testable and can't drift from the charts that consume them.
 * Mirrors the conventions of fleet/inference-telemetry.ts.
 */
import type { BreakdownBucket, SnapshotPoint } from '$lib/api/analytics';

// ── Treemap ──────────────────────────────────────────────────────────────────

export interface TreemapNode {
	/** Display label — the last path segment of the directory key. */
	name: string;
	/** Treemap area = bytes. */
	value: number;
	count: number;
	/** Full directory path to drill into; null for the aggregated remainder. */
	path: string | null;
	/** True when the backend says nothing nests below (stop drilling). */
	isLeaf: boolean;
}

/**
 * Shape one breakdown level (directory dimension) into echarts treemap nodes:
 * top-N directories by bytes plus one non-drillable "other" remainder node so
 * a wide level can't explode the treemap.
 */
export function toTreemapNodes(buckets: BreakdownBucket[], topN = 30): TreemapNode[] {
	const sorted = [...buckets].sort((a, b) => b.bytes - a.bytes);
	const top = sorted.slice(0, topN);
	const rest = sorted.slice(topN);
	const nodes: TreemapNode[] = top.map((b) => ({
		name: b.key.split('/').filter(Boolean).pop() ?? b.key,
		value: b.bytes,
		count: b.count,
		path: b.key,
		isLeaf: b.is_leaf ?? false
	}));
	if (rest.length > 0) {
		nodes.push({
			name: `other (${rest.length} dirs)`,
			value: rest.reduce((s, b) => s + b.bytes, 0),
			count: rest.reduce((s, b) => s + b.count, 0),
			path: null,
			isLeaf: true
		});
	}
	return nodes;
}

// ── Growth timeseries pivot ──────────────────────────────────────────────────

export interface PivotResult {
	/** Sorted unique bucket timestamps (ISO strings) — the x-axis categories. */
	categories: string[];
	/** One aligned series per file server, in `categories` order. */
	series: { name: string; data: (number | null)[] }[];
}

/**
 * Reshape flat `(bucket, server)` snapshot rows into per-server series aligned
 * on a shared, sorted bucket axis. A missing `(bucket, server)` cell is a real
 * gap (no snapshot was captured) — `null`, never a fabricated 0.
 */
export function pivotSnapshots(
	points: SnapshotPoint[],
	valueOf: (p: SnapshotPoint) => number
): PivotResult {
	const bucketSet = new Set<string>();
	const serverSet = new Set<string>();
	const byKey = new Map<string, number>();
	for (const p of points) {
		bucketSet.add(p.bucket);
		serverSet.add(p.file_server_id);
		byKey.set(`${p.bucket} ${p.file_server_id}`, valueOf(p));
	}
	const categories = [...bucketSet].sort();
	const series = [...serverSet].sort().map((s) => ({
		name: s,
		data: categories.map((b) => byKey.get(`${b} ${s}`) ?? null)
	}));
	return { categories, series };
}

/** Window presets for the growth chart — look-back + bucket width pairs that
 *  keep the point count chart-friendly. */
export interface GrowthWindow {
	label: string;
	windowSecs: number;
	bucketSecs: number;
}
export const GROWTH_WINDOWS: GrowthWindow[] = [
	{ label: '24h', windowSecs: 86_400, bucketSecs: 3_600 },
	{ label: '7d', windowSecs: 604_800, bucketSecs: 21_600 },
	{ label: '30d', windowSecs: 2_592_000, bucketSecs: 86_400 }
];

// ── Dimension key labelling / ordering ───────────────────────────────────────

const UNIT_BYTES: Record<string, number> = {
	B: 1,
	KiB: 1024,
	MiB: 1024 ** 2,
	GiB: 1024 ** 3,
	TiB: 1024 ** 4
};

/** Parse a size-class key (`<1 KiB`, `1–64 KiB`, `≥4 GiB`) to a sortable byte
 *  magnitude; unparseable keys (e.g. `unknown`) sort last. */
function sizeClassMagnitude(key: string): number {
	const m = key.match(/(\d+(?:\.\d+)?)\s*(B|KiB|MiB|GiB|TiB)/);
	if (!m) return Number.POSITIVE_INFINITY;
	let v = Number(m[1]) * (UNIT_BYTES[m[2]] ?? 1);
	// `<X` is everything strictly below X — order it before classes starting AT X.
	if (key.trimStart().startsWith('<')) v -= 0.5;
	return v;
}

/** Human label for a size-class bucket key (backend keys are already
 *  presentation-ready; only the null sentinel needs prettifying). */
export function sizeClassLabel(key: string): string {
	return key === '' || key === 'unknown' ? 'Unknown' : key;
}

/** Order size-class buckets smallest→largest (the backend GROUP BY gives no
 *  meaningful order); unknown keys go last. */
export function sortSizeClasses(buckets: BreakdownBucket[]): BreakdownBucket[] {
	return [...buckets].sort((a, b) => sizeClassMagnitude(a.key) - sizeClassMagnitude(b.key));
}

/** Human label for an age-cohort key (`<7d` … `>2y`). */
export function ageCohortLabel(key: string): string {
	const labels: Record<string, string> = {
		'<7d': 'Last 7 days',
		'7d-30d': '7–30 days',
		'30d-90d': '30–90 days',
		'90d-1y': '90 days – 1 year',
		'1y-2y': '1–2 years',
		'>2y': 'Over 2 years'
	};
	if (key === '' || key === 'unknown') return 'Unknown';
	return labels[key] ?? key;
}

// ── Generic bucket shaping ───────────────────────────────────────────────────

/** Top-N buckets by `valueOf` plus one aggregated `other` row (bar/donut
 *  panels). Pass-through when nothing overflows. */
export function topNWithOther(
	buckets: BreakdownBucket[],
	n: number,
	valueOf: (b: BreakdownBucket) => number = (b) => b.bytes
): BreakdownBucket[] {
	const sorted = [...buckets].sort((a, b) => valueOf(b) - valueOf(a));
	if (sorted.length <= n) return sorted;
	const top = sorted.slice(0, n);
	const rest = sorted.slice(n);
	return [
		...top,
		{
			key: 'other',
			count: rest.reduce((s, b) => s + b.count, 0),
			bytes: rest.reduce((s, b) => s + b.bytes, 0)
		}
	];
}

/** Dedup ratio = physical (hashed inventory) bytes ÷ logical (catalogue)
 *  bytes. 1.0 means every byte is stored once; 2.0 means each logical byte
 *  exists in two copies. Null when nothing is catalogued yet. */
export function dedupRatio(physicalBytes: number, catalogueBytes: number): number | null {
	if (!Number.isFinite(physicalBytes) || !Number.isFinite(catalogueBytes)) return null;
	if (catalogueBytes <= 0) return null;
	return physicalBytes / catalogueBytes;
}
