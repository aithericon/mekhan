/**
 * Per-template analytics API wrappers.
 *
 * Two thin functions over the `/templates/{id}/analytics` family:
 *   - `getTemplateAnalytics` — structural shape + usage/duration/node rollups
 *     for the whole version chain.
 *   - `getTemplateAnalyticsTimeseries` — run-outcome points rebucketed from
 *     the hour-grained rollup table, oldest first.
 *
 * Types are derived from the generated OpenAPI schema (`schema.d.ts`).
 * Call sites import from here, not from `schema.d.ts` directly.
 */
import { rawJson } from './client';
import type { components } from './schema';

// ── Type aliases ───────────────────────────────────────────────────────────

export type TemplateAnalytics = components['schemas']['TemplateAnalytics'];
export type TemplateMetrics = components['schemas']['TemplateMetrics'];
export type TemplateUsageSummary = components['schemas']['TemplateUsageSummary'];
export type RunsByOutcome = components['schemas']['RunsByOutcome'];
export type DurationPercentiles = components['schemas']['DurationPercentiles'];
export type NodeHotspot = components['schemas']['NodeHotspot'];
export type TemplateNodeHotspots = components['schemas']['TemplateNodeHotspots'];
export type TemplateRunTimeseriesPoint = components['schemas']['TemplateRunTimeseriesPoint'];

// ── Endpoints ──────────────────────────────────────────────────────────────

/**
 * GET /api/v1/templates/{id}/analytics
 *
 * Full per-template analytics: structural shape (compile-time) + usage /
 * duration / node rollups, scoped to the whole version chain and the
 * requested instance `mode` (default `live`).
 */
export async function getTemplateAnalytics(
	templateId: string,
	opts?: { mode?: string }
): Promise<TemplateAnalytics> {
	const qs = opts?.mode ? `?mode=${encodeURIComponent(opts.mode)}` : '';
	return rawJson(`/templates/${templateId}/analytics${qs}`);
}

/**
 * GET /api/v1/templates/{id}/analytics/timeseries
 *
 * Run-outcome points rebucketed from the hour-grained `template_run_rollup`,
 * one `(bucket, outcome)` cell per row, oldest first. `mode` defaults to
 * `live`; `bucket_secs` defaults to 3600 (hourly); `window_secs` defaults
 * to 30 days.
 */
export async function getTemplateAnalyticsTimeseries(
	templateId: string,
	params?: {
		mode?: string;
		/** Bucket width in seconds (clamped 60..=7d, default 3600). */
		bucket_secs?: number;
		/** Look-back window in seconds (capped at 365d, default 30d). */
		window_secs?: number;
	}
): Promise<TemplateRunTimeseriesPoint[]> {
	const qs = new URLSearchParams();
	if (params?.mode) qs.set('mode', params.mode);
	if (params?.bucket_secs != null) qs.set('bucket_secs', String(params.bucket_secs));
	if (params?.window_secs != null) qs.set('window_secs', String(params.window_secs));
	const query = qs.toString();
	return rawJson(`/templates/${templateId}/analytics/timeseries${query ? `?${query}` : ''}`);
}
