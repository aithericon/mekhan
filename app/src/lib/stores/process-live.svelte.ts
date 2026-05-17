/**
 * Process-live store: initial DB backfill + SSE streaming of metrics & logs.
 *
 * - On init: REST fetch of `/metrics/series` + `/logs/tail` for the given window.
 * - Opens two SSE streams (`/metrics/stream`, `/logs/stream`) with `since_seq=0`.
 * - Receives `metric`/`log` events and appends to the appropriate state.
 * - Handles `resync` (broadcast lag) and `gap` (ring buffer rolled past us) by
 *   re-running the DB backfill and reopening the stream.
 * - Mirrors `tasks.svelte.ts` for the fetch+ReadableStream+backoff pattern.
 */

import {
	getProcessArtifactsList,
	getProcessLogsTail,
	getProcessMetricsSeries,
	processArtifactsStreamUrl,
	processLogsStreamUrl,
	processMetricsStreamUrl,
	type LiveArtifactEntry,
	type LiveLogEvent,
	type LiveMetricEvent,
	type LogTailRow,
	type MetricPoint
} from '$lib/api/client';
import { createSseChannel, type SseChannel } from '$lib/net/sse';

const SSE_MAX_RETRIES = 8;
const SSE_INITIAL_RETRY_MS = 1000;
const DEFAULT_MAX_POINTS_PER_SERIES = 5000;
const DEFAULT_MAX_LOG_BUFFER = 2000;
const DEFAULT_MAX_ARTIFACT_BUFFER = 500;

export interface ProcessLiveOptions {
	/** Metric keys to subscribe to. Empty means all. */
	keys?: string[];
	/** Optional signal_key drilldown. */
	signalKey?: string;
	/** Optional log level filter. */
	logLevel?: string;
	/** Optional log text filter (server-side ILIKE on backfill, substring on stream). */
	logQuery?: string;
	/** Backfill window in seconds. */
	windowSeconds?: number;
	/** Max points retained per metric series. */
	maxPointsPerSeries?: number;
	/** Max log entries retained. */
	maxLogBuffer?: number;
	/** Artifact category whitelist. Empty = all renderable. */
	artifactCategories?: string[];
	/** Render-hint whitelist (applied to user_metadata.render_hint). */
	artifactRenderHints?: string[];
	/** Max artifact entries retained. */
	maxArtifactBuffer?: number;
}

export interface SeriesState {
	bucketSeconds: number;
	series: Record<string, MetricPoint[]>;
}

export type ConnectionStatus = 'idle' | 'loading' | 'streaming' | 'reconnecting' | 'error';

export function createProcessLiveStore(processId: string, opts: ProcessLiveOptions = {}) {
	const maxPointsPerSeries = opts.maxPointsPerSeries ?? DEFAULT_MAX_POINTS_PER_SERIES;
	const maxLogBuffer = opts.maxLogBuffer ?? DEFAULT_MAX_LOG_BUFFER;
	const maxArtifactBuffer = opts.maxArtifactBuffer ?? DEFAULT_MAX_ARTIFACT_BUFFER;

	// Reactive state (Svelte 5 runes).
	let metrics = $state<SeriesState>({ bucketSeconds: 0, series: {} });
	let logs = $state<LogTailRow[]>([]);
	let artifacts = $state<LiveArtifactEntry[]>([]);
	let metricStatus = $state<ConnectionStatus>('idle');
	let logStatus = $state<ConnectionStatus>('idle');
	let artifactStatus = $state<ConnectionStatus>('idle');
	let errorMessage = $state<string | null>(null);

	// Filter state — mutable so consumers can change without rebuilding the store.
	let keys = [...(opts.keys ?? [])];
	let signalKey = opts.signalKey;
	let logLevel = opts.logLevel;
	let logQuery = opts.logQuery;
	let windowSeconds = opts.windowSeconds ?? 3600;
	let artifactCategories = [...(opts.artifactCategories ?? [])];
	let artifactRenderHints = [...(opts.artifactRenderHints ?? [])];

	// Per-key epoch-ms high-water mark from the most recent metrics backfill.
	// The SSE stream opens at since_seq=0 and the server replays its
	// ring-buffer snapshot, which overlaps the DB backfill — a streamed point
	// at/before its key's backfilled max is already shown (downsampled) and
	// must be dropped to avoid duplication. A key absent here was NOT
	// backfilled (e.g. older than the window), so its stream points are the
	// only source and must be kept.
	let metricsBackfillMax: Record<string, number> = {};

	// Highest metric/log/artifact seq observed (drives reconnect resume).
	let lastMetricSeq = 0;
	let lastLogSeq = 0;
	let lastArtifactSeq = 0;

	// SSE control.
	let metricsChannel: SseChannel | null = null;
	let logsChannel: SseChannel | null = null;
	let artifactsChannel: SseChannel | null = null;
	let destroyed = false;

	// Staleness tracking for tab-resume / network-resume heuristics.
	// Server keepalive is every 5s, so >20s silence implies a dead connection.
	const STALENESS_MS = 20_000;
	let lastEventTime = 0;

	function appendMetric(e: LiveMetricEvent) {
		if (keys.length > 0 && !keys.includes(e.key)) return;
		if (signalKey && e.signal_key !== signalKey) return;
		lastEventTime = Date.now();
		lastMetricSeq = Math.max(lastMetricSeq, e.seq);
		// Already represented by the DB backfill (downsampled) for this key —
		// the stream's initial snapshot replays it; skip to avoid duplication.
		// Keys not in the backfill (older than the window) fall through.
		const cap = metricsBackfillMax[e.key];
		if (cap !== undefined && new Date(e.timestamp).getTime() <= cap) {
			return;
		}
		const arr = metrics.series[e.key] ?? [];
		arr.push({ t: e.timestamp, v: e.value });
		if (arr.length > maxPointsPerSeries) {
			arr.splice(0, arr.length - maxPointsPerSeries);
		}
		metrics.series[e.key] = arr;
		// Trigger reactivity on the container.
		metrics = { bucketSeconds: metrics.bucketSeconds, series: metrics.series };
	}

	function appendArtifact(e: LiveArtifactEntry) {
		lastEventTime = Date.now();
		if (typeof e.seq === 'number') {
			lastArtifactSeq = Math.max(lastArtifactSeq, e.seq);
		}
		// Upsert by artifact_id so live events replace DB-backfill rows.
		const id = e.artifact_id ?? e.id;
		if (!id) return;
		const idx = artifacts.findIndex((a) => (a.artifact_id ?? a.id) === id);
		if (idx >= 0) {
			const next = [...artifacts];
			next[idx] = { ...next[idx], ...e };
			artifacts = next;
		} else {
			const next = [...artifacts, e];
			if (next.length > maxArtifactBuffer) {
				next.splice(0, next.length - maxArtifactBuffer);
			}
			artifacts = next;
		}
	}

	function appendLog(e: LiveLogEvent) {
		if (signalKey && e.signal_key !== signalKey) return;
		if (logLevel && e.level !== logLevel) return;
		if (logQuery && !e.message.toLowerCase().includes(logQuery.toLowerCase())) return;
		lastEventTime = Date.now();
		lastLogSeq = Math.max(lastLogSeq, e.seq);
		const row: LogTailRow = {
			id: -e.seq, // synthetic (DB rows have positive ids); negatives mark live rows
			process_id: e.process_id,
			level: e.level,
			source: e.source ?? null,
			message: e.message,
			detail: (e.detail as Record<string, unknown> | null) ?? null,
			timestamp: e.timestamp,
			signal_key: e.signal_key ?? null
		};
		logs = [...logs, row];
		if (logs.length > maxLogBuffer) {
			logs = logs.slice(logs.length - maxLogBuffer);
		}
	}

	async function backfillMetrics() {
		metricStatus = 'loading';
		try {
			const now = new Date();
			const since = new Date(now.getTime() - windowSeconds * 1000);
			const resp = await getProcessMetricsSeries(processId, {
				keys: keys.length > 0 ? keys : undefined,
				since,
				until: now,
				signal_key: signalKey,
				max_points: 2000
			});
			metrics = { bucketSeconds: resp.bucket_seconds, series: resp.series };
			// Record the newest backfilled instant per key; the stream snapshot
			// replays everything ≤ this, which we then drop as duplicates.
			const nextMax: Record<string, number> = {};
			for (const [k, pts] of Object.entries(resp.series)) {
				let m = 0;
				for (const p of pts) {
					const t = new Date(p.t).getTime();
					if (t > m) m = t;
				}
				if (m > 0) nextMax[k] = m;
			}
			metricsBackfillMax = nextMax;
			errorMessage = null;
		} catch (e) {
			errorMessage = e instanceof Error ? e.message : String(e);
			metricStatus = 'error';
			throw e;
		}
	}

	async function backfillLogs() {
		logStatus = 'loading';
		try {
			const now = new Date();
			const since = new Date(now.getTime() - windowSeconds * 1000);
			const resp = await getProcessLogsTail(processId, {
				since,
				until: now,
				level: logLevel,
				signal_key: signalKey,
				q: logQuery,
				limit: 500
			});
			// LogsTailResponse.logs has slightly broader nullability than LogTailRow
			// (LogRow uses `source: Option<String>` → `string | null | undefined`).
			logs = resp.logs.map((r) => ({
				...r,
				source: r.source ?? null,
				signal_key: r.signal_key ?? null,
				detail: (r.detail as Record<string, unknown> | null) ?? null
			}));
			errorMessage = null;
		} catch (e) {
			errorMessage = e instanceof Error ? e.message : String(e);
			logStatus = 'error';
			throw e;
		}
	}

	async function backfillArtifacts() {
		artifactStatus = 'loading';
		try {
			// Artifacts are low-frequency and the lineage view is cheap — load
			// full history for scrubber support (bounded by the limit).
			const resp = await getProcessArtifactsList(processId, {
				categories: artifactCategories.length > 0 ? artifactCategories : undefined,
				render_hints: artifactRenderHints.length > 0 ? artifactRenderHints : undefined,
				limit: maxArtifactBuffer
			});
			// CatalogueEntry → LiveArtifactEntry: CatalogueEntry has `process_id: string | null`,
			// LiveArtifactEntry uses `string | undefined`. Same for several other Option fields.
			artifacts = resp.entries.map((e) => ({
				...e,
				id: e.id,
				process_id: e.process_id ?? undefined,
				mime_type: e.mime_type ?? null,
				storage_path: e.storage_path ?? null,
				size_bytes: e.size_bytes ?? null,
				process_step: e.process_step ?? null,
				signal_key: e.signal_key ?? null,
				user_metadata: (e.user_metadata as Record<string, unknown> | null) ?? null
			}));
			errorMessage = null;
		} catch (e) {
			errorMessage = e instanceof Error ? e.message : String(e);
			artifactStatus = 'error';
			throw e;
		}
	}

	function connectMetrics() {
		if (destroyed) return;
		metricsChannel?.close();
		metricsChannel = createSseChannel<LiveMetricEvent>({
			streamUrl: () =>
				processMetricsStreamUrl(processId, {
					since_seq: lastMetricSeq,
					signal_key: signalKey,
					keys: keys.length > 0 ? keys : undefined
				}),
			eventName: 'metric',
			onPayload: appendMetric,
			backfill: backfillMetrics,
			setStatus: (s) => {
				metricStatus = s;
			},
			maxRetries: SSE_MAX_RETRIES,
			initialRetryMs: SSE_INITIAL_RETRY_MS
		});
	}

	function connectArtifacts() {
		if (destroyed) return;
		artifactsChannel?.close();
		artifactsChannel = createSseChannel<LiveArtifactEntry>({
			streamUrl: () =>
				processArtifactsStreamUrl(processId, {
					since_seq: lastArtifactSeq,
					categories: artifactCategories.length > 0 ? artifactCategories : undefined,
					render_hints: artifactRenderHints.length > 0 ? artifactRenderHints : undefined
				}),
			eventName: 'artifact',
			onPayload: appendArtifact,
			backfill: backfillArtifacts,
			setStatus: (s) => {
				artifactStatus = s;
			},
			maxRetries: SSE_MAX_RETRIES,
			initialRetryMs: SSE_INITIAL_RETRY_MS
		});
	}

	function connectLogs() {
		if (destroyed) return;
		logsChannel?.close();
		logsChannel = createSseChannel<LiveLogEvent>({
			streamUrl: () =>
				processLogsStreamUrl(processId, {
					since_seq: lastLogSeq,
					signal_key: signalKey,
					level: logLevel,
					q: logQuery
				}),
			eventName: 'log',
			onPayload: appendLog,
			backfill: backfillLogs,
			setStatus: (s) => {
				logStatus = s;
			},
			maxRetries: SSE_MAX_RETRIES,
			initialRetryMs: SSE_INITIAL_RETRY_MS
		});
	}

	// Force-reconnect both SSE streams with fresh retry budgets.
	// Called from `online` / `visibilitychange` / `pageshow` handlers below.
	// Each connectX() builds a brand-new channel whose retry counter starts at
	// 0, so the retry budget is implicitly refreshed.
	function forceReconnect(reason: string) {
		if (destroyed) return;
		console.log(`[process-live] force-reconnect: ${reason}`);
		metricsChannel?.close();
		logsChannel?.close();
		artifactsChannel?.close();
		// Refresh DB backfill so we don't miss events that rolled past the ring
		// buffer while the tab was idle.
		Promise.all([backfillMetrics(), backfillLogs(), backfillArtifacts()])
			.catch(() => undefined)
			.finally(() => {
				connectMetrics();
				connectLogs();
				connectArtifacts();
			});
	}

	function handleOnline() {
		forceReconnect('network online');
	}

	function handleVisibilityChange() {
		if (destroyed || typeof document === 'undefined' || document.hidden) return;
		const disconnected =
			metricStatus !== 'streaming' || logStatus !== 'streaming';
		const stale = lastEventTime > 0 && Date.now() - lastEventTime > STALENESS_MS;
		if (disconnected || stale) {
			forceReconnect(`tab visible (disconnected=${disconnected}, stale=${stale})`);
		}
	}

	function handlePageShow(e: PageTransitionEvent) {
		if (destroyed || !e.persisted) return;
		forceReconnect('restored from bfcache');
	}

	function attachResumeListeners() {
		if (typeof window === 'undefined') return;
		window.addEventListener('online', handleOnline);
		window.addEventListener('pageshow', handlePageShow);
		document.addEventListener('visibilitychange', handleVisibilityChange);
	}

	function detachResumeListeners() {
		if (typeof window === 'undefined') return;
		window.removeEventListener('online', handleOnline);
		window.removeEventListener('pageshow', handlePageShow);
		document.removeEventListener('visibilitychange', handleVisibilityChange);
	}

	async function init() {
		if (destroyed) return;
		attachResumeListeners();
		await Promise.all([
			backfillMetrics(),
			backfillLogs(),
			backfillArtifacts()
		]).catch(() => undefined);
		connectMetrics();
		connectLogs();
		connectArtifacts();
	}

	function setKeys(next: string[]) {
		keys = [...next];
		lastMetricSeq = 0;
		// Reset metric series so we only show the newly-selected keys.
		metrics = { bucketSeconds: metrics.bucketSeconds, series: {} };
		backfillMetrics()
			.then(() => connectMetrics())
			.catch(() => undefined);
	}

	function setSignalKey(next: string | undefined) {
		signalKey = next;
		lastMetricSeq = 0;
		lastLogSeq = 0;
		metrics = { bucketSeconds: 0, series: {} };
		logs = [];
		init();
	}

	function setLogFilter(next: { level?: string; query?: string }) {
		logLevel = next.level;
		logQuery = next.query;
		lastLogSeq = 0;
		logs = [];
		backfillLogs()
			.then(() => connectLogs())
			.catch(() => undefined);
	}

	function setWindowSeconds(sec: number) {
		windowSeconds = sec;
		lastMetricSeq = 0;
		lastLogSeq = 0;
		metrics = { bucketSeconds: 0, series: {} };
		logs = [];
		init();
	}

	function setArtifactFilter(next: { categories?: string[]; renderHints?: string[] }) {
		artifactCategories = [...(next.categories ?? [])];
		artifactRenderHints = [...(next.renderHints ?? [])];
		lastArtifactSeq = 0;
		artifacts = [];
		backfillArtifacts()
			.then(() => connectArtifacts())
			.catch(() => undefined);
	}

	function destroy() {
		destroyed = true;
		detachResumeListeners();
		metricsChannel?.close();
		logsChannel?.close();
		artifactsChannel?.close();
	}

	return {
		get metrics() {
			return metrics;
		},
		get logs() {
			return logs;
		},
		get metricStatus() {
			return metricStatus;
		},
		get logStatus() {
			return logStatus;
		},
		get artifacts() {
			return artifacts;
		},
		get artifactStatus() {
			return artifactStatus;
		},
		get artifactCategories() {
			return artifactCategories;
		},
		get artifactRenderHints() {
			return artifactRenderHints;
		},
		get error() {
			return errorMessage;
		},
		get keys() {
			return keys;
		},
		get signalKey() {
			return signalKey;
		},
		get logLevel() {
			return logLevel;
		},
		get logQuery() {
			return logQuery;
		},
		get windowSeconds() {
			return windowSeconds;
		},
		init,
		setKeys,
		setSignalKey,
		setLogFilter,
		setArtifactFilter,
		setWindowSeconds,
		destroy
	};
}
