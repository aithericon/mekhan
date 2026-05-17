/**
 * Unified Server-Sent-Events client.
 *
 * Replaces the three hand-rolled SSE loops that previously lived in
 * `process-live.svelte.ts`, `petri.svelte.ts` and `tasks.svelte.ts`. The
 * parser is the (correct) one from `process-live` — a `ReadableStream` +
 * `TextDecoder` line buffer that handles multi-line `data:` fields, CRLF,
 * comment/keepalive lines and the optional single leading space after the
 * field colon, per the WHATWG SSE grammar.
 *
 * Reconnect policy is caller-configurable so each existing store keeps its
 * exact observable behaviour:
 *  - process-live: 8 retries, then surface an error per channel.
 *  - petri:        5 retries, then fall back to polling.
 *  - tasks:        5 retries, then stop silently.
 */

export interface SseEvent {
	/** The `event:` field value, defaulting to `'message'`. */
	event: string;
	/** The joined `data:` field value (multi-line joined with `\n`). */
	data: string;
}

export interface ConnectSseOptions {
	/** Invoked once per dispatched SSE event. May be async. */
	onEvent: (event: SseEvent) => void | Promise<void>;
	/**
	 * Invoked right before a (re)connection attempt's `fetch`. Receives the
	 * zero-based attempt number (0 = first connect, 1 = first reconnect, …).
	 */
	onReconnect?: (attempt: number) => void;
	/** Invoked once a connection's response is OK and the body stream is open. */
	onOpen?: () => void;
	/**
	 * Invoked when the stream errors/ends and the retry budget is spent. The
	 * caller decides what to do (poll fallback, error status, nothing).
	 */
	onRetriesExhausted?: () => void;
	/**
	 * Invoked when the connect response is a TERMINAL client error (e.g. 404,
	 * 409, 401, 403) that retrying can never fix — for instance the petri
	 * engine's 409 "Net is completed or cancelled" tombstone gate. The
	 * connection stops cleanly WITHOUT scheduling a retry or exhausting the
	 * budget; the caller decides the resulting state. `body` is the response
	 * body text (it carries the engine's reason string).
	 */
	onTerminal?: (status: number, body: string) => void;
	/** Max reconnect attempts after the initial connect. Default 8. */
	maxRetries?: number;
	/** Base backoff in ms; delay = initialRetryMs * 2^attempt. Default 1000. */
	initialRetryMs?: number;
	/**
	 * `fetch` implementation. Defaults to global `fetch`; pass `authFetch`
	 * for BFF cookie credentials.
	 */
	fetchImpl?: (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;
}

export interface SseConnection {
	/** Abort the current connection and stop reconnecting. */
	close(): void;
}

/**
 * Whether an HTTP status is a TERMINAL client error: retrying it can never
 * succeed, so the connection must stop instead of entering backoff.
 *
 * The whole 4xx range is terminal EXCEPT 408 (Request Timeout) and 429 (Too
 * Many Requests), which are transient and retryable. 5xx, network errors and
 * normal stream-end stay on the existing retry path.
 */
function isTerminalStatus(status: number): boolean {
	if (status < 400 || status >= 500) return false;
	if (status === 408 || status === 429) return false;
	return true;
}

/**
 * Parse an SSE `ReadableStream`, dispatching `(event, data)` once per
 * dispatched event. Handles multi-line `data:` fields, CRLF line endings,
 * comment lines (`:` prefix) and the optional leading space after the colon.
 * Stops when `signal` aborts or the stream ends.
 */
export async function parseSseStream(
	body: ReadableStream<Uint8Array>,
	signal: AbortSignal,
	onEvent: (event: SseEvent) => void | Promise<void>
): Promise<void> {
	const reader = body.getReader();
	const decoder = new TextDecoder();
	let buffer = '';
	let currentEvent = 'message';
	let currentData: string[] = [];

	const flush = async () => {
		if (currentData.length === 0 && currentEvent === 'message') return;
		const data = currentData.join('\n');
		currentData = [];
		const ev = currentEvent;
		currentEvent = 'message';
		await onEvent({ event: ev, data });
	};

	while (!signal.aborted) {
		const { done, value } = await reader.read();
		if (done) break;
		buffer += decoder.decode(value, { stream: true });
		const lines = buffer.split('\n');
		buffer = lines.pop() ?? '';
		for (const rawLine of lines) {
			const line = rawLine.replace(/\r$/, '');
			if (line === '') {
				await flush();
				continue;
			}
			if (line.startsWith(':')) continue; // comment / keepalive
			const colon = line.indexOf(':');
			const field = colon === -1 ? line : line.slice(0, colon);
			const valueRaw = colon === -1 ? '' : line.slice(colon + 1);
			const value = valueRaw.startsWith(' ') ? valueRaw.slice(1) : valueRaw;
			if (field === 'event') currentEvent = value;
			else if (field === 'data') currentData.push(value);
			// `id` field ignored — callers track resume position in the payload.
		}
	}
}

/**
 * Open an SSE stream with automatic exponential-backoff reconnect.
 *
 * `url` may be a string or a thunk; when a thunk it is re-evaluated on every
 * (re)connect so a moving resume cursor (e.g. `?from_sequence=N`) is always
 * current — matching the prior stores that rebuilt the URL each retry.
 *
 * Returns immediately with a handle whose `close()` aborts the connection and
 * cancels any pending reconnect. The retry counter resets to 0 on every
 * successful connection (matching the prior per-store behaviour).
 */
export function connectSse(
	url: string | (() => string),
	options: ConnectSseOptions
): SseConnection {
	const {
		onEvent,
		onReconnect,
		onOpen,
		onRetriesExhausted,
		onTerminal,
		maxRetries = 8,
		initialRetryMs = 1000,
		fetchImpl = fetch
	} = options;

	let controller: AbortController | null = null;
	let retryCount = 0;
	let closed = false;
	let pendingTimer: ReturnType<typeof setTimeout> | null = null;

	function scheduleRetry() {
		if (closed) return;
		if (retryCount >= maxRetries) {
			onRetriesExhausted?.();
			return;
		}
		const delay = initialRetryMs * Math.pow(2, retryCount);
		retryCount++;
		pendingTimer = setTimeout(() => {
			pendingTimer = null;
			connect();
		}, delay);
	}

	function connect() {
		if (closed) return;
		controller?.abort();
		const ctrl = new AbortController();
		controller = ctrl;

		onReconnect?.(retryCount);
		const target = typeof url === 'function' ? url() : url;

		(async () => {
			try {
				const resp = await fetchImpl(target, { signal: ctrl.signal });
				if (!resp.ok) {
					if (isTerminalStatus(resp.status)) {
						// Permanent client error (e.g. petri's 409 tombstone):
						// retrying can never succeed. Stop cleanly, do NOT
						// schedule a retry or spend the retry budget.
						const body = await resp.text().catch(() => '');
						if (ctrl.signal.aborted) return;
						closed = true;
						onTerminal?.(resp.status, body);
						return;
					}
					throw new Error(`SSE connect failed: ${resp.status}`);
				}
				if (!resp.body) {
					throw new Error(`SSE connect failed: ${resp.status}`);
				}
				retryCount = 0;
				onOpen?.();
				await parseSseStream(resp.body, ctrl.signal, onEvent);
			} catch (e) {
				if (ctrl.signal.aborted) return;
				if (e instanceof Error && e.name === 'AbortError') return;
				console.warn('SSE error:', e);
			}
			scheduleRetry();
		})();
	}

	connect();

	return {
		close() {
			closed = true;
			if (pendingTimer !== null) {
				clearTimeout(pendingTimer);
				pendingTimer = null;
			}
			controller?.abort();
			controller = null;
		}
	};
}

export interface SseChannelOptions<TPayload> {
	/** Builds the stream URL from the channel's current resume position. */
	streamUrl: () => string;
	/** The `event:` name carrying payloads (e.g. `'metric'`). */
	eventName: string;
	/** Parsed-payload handler (one decoded JSON object per `eventName` event). */
	onPayload: (payload: TPayload) => void;
	/**
	 * Event names that should trigger a backfill + reset (e.g. `'gap'`,
	 * `'resync'`). The backfill runs, then the channel reconnects.
	 */
	backfillOn?: string[];
	/** Re-fetch the durable snapshot. Errors are swallowed. */
	backfill: () => Promise<unknown>;
	/** Status sink, mirroring the prior per-channel ConnectionStatus. */
	setStatus: (status: 'reconnecting' | 'streaming' | 'error') => void;
	/**
	 * Invoked when the stream hits a TERMINAL client error (see
	 * {@link ConnectSseOptions.onTerminal}). The channel stops reconnecting;
	 * by default it transitions to `'error'` via `setStatus`.
	 */
	onTerminal?: (status: number, body: string) => void;
	/** Max reconnect attempts. Default 8. */
	maxRetries?: number;
	/** Base backoff in ms. Default 1000. */
	initialRetryMs?: number;
}

export interface SseChannel {
	connect(): void;
	close(): void;
}

/**
 * A self-reconnecting SSE channel that decodes JSON payloads for one event
 * name and re-runs a backfill on `gap`/`resync`-style events. Collapses the
 * three near-identical `connectMetrics`/`connectLogs`/`connectArtifacts`
 * loops in `process-live`.
 *
 * `streamUrl()` is read lazily on every (re)connect so the latest resume
 * cursor and filters are always used.
 */
export function createSseChannel<TPayload>(
	opts: SseChannelOptions<TPayload>
): SseChannel {
	const {
		streamUrl,
		eventName,
		onPayload,
		backfillOn = ['gap', 'resync'],
		backfill,
		setStatus,
		onTerminal,
		maxRetries = 8,
		initialRetryMs = 1000
	} = opts;

	let conn: SseConnection | null = null;
	let closed = false;

	function connect() {
		if (closed) return;
		conn?.close();
		conn = connectSse(streamUrl, {
			maxRetries,
			initialRetryMs,
			onReconnect: () => setStatus('reconnecting'),
			onOpen: () => setStatus('streaming'),
			onRetriesExhausted: () => setStatus('error'),
			onTerminal: (status, body) => {
				closed = true;
				if (onTerminal) onTerminal(status, body);
				else setStatus('error');
			},
			onEvent: async ({ event, data }) => {
				if (event === eventName) {
					try {
						onPayload(JSON.parse(data) as TPayload);
					} catch {
						/* ignore malformed */
					}
				} else if (backfillOn.includes(event)) {
					await backfill().catch(() => undefined);
				}
			}
		});
	}

	connect();

	return {
		connect,
		close() {
			closed = true;
			conn?.close();
			conn = null;
		}
	};
}
