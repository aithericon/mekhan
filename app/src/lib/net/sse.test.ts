import { describe, it, expect, vi, afterEach } from 'vitest';
import { connectSse } from './sse';

/**
 * Pure / DB-free tests for the terminal-status classification added to
 * `connectSse`. A terminal 4xx (404/409/401/403) must fire `onTerminal`
 * exactly once and never schedule a retry (`onReconnect`); a 5xx must keep
 * the existing retry/backoff behaviour.
 */

function jsonResponse(status: number, body: string): Response {
	return new Response(body, { status });
}

/** A never-ending SSE body so a 200 connection stays "open". */
function streamingOkResponse(): Response {
	const stream = new ReadableStream<Uint8Array>({
		start() {
			/* never enqueues, never closes — stays open until aborted */
		}
	});
	return new Response(stream, { status: 200 });
}

describe('connectSse terminal status handling', () => {
	afterEach(() => {
		vi.restoreAllMocks();
		vi.useRealTimers();
	});

	it('fires onTerminal(409, body) exactly once and never retries', async () => {
		const body = 'Net is completed or cancelled';
		const fetchImpl = vi.fn(async () => jsonResponse(409, body));
		const onTerminal = vi.fn();
		const onReconnect = vi.fn();
		const onRetriesExhausted = vi.fn();

		const conn = connectSse('http://x/events/stream', {
			fetchImpl,
			onEvent: () => {},
			onTerminal,
			onReconnect,
			onRetriesExhausted,
			maxRetries: 5,
			initialRetryMs: 1
		});

		// Let the connect microtask + body read settle.
		await vi.waitFor(() => expect(onTerminal).toHaveBeenCalledTimes(1));

		expect(onTerminal).toHaveBeenCalledWith(409, body);
		// Exactly one fetch (the initial connect) — no retry storm.
		expect(fetchImpl).toHaveBeenCalledTimes(1);
		expect(onReconnect).toHaveBeenCalledTimes(1); // the initial connect only
		expect(onRetriesExhausted).not.toHaveBeenCalled();

		// Give any (erroneously) scheduled retry time to fire — it must not.
		await new Promise((r) => setTimeout(r, 30));
		expect(fetchImpl).toHaveBeenCalledTimes(1);
		expect(onTerminal).toHaveBeenCalledTimes(1);

		conn.close();
	});

	it.each([404, 401, 403])(
		'treats %i as terminal (no retry)',
		async (status) => {
			const fetchImpl = vi.fn(async () => jsonResponse(status, 'nope'));
			const onTerminal = vi.fn();
			const onReconnect = vi.fn();

			const conn = connectSse('http://x/events/stream', {
				fetchImpl,
				onEvent: () => {},
				onTerminal,
				onReconnect,
				maxRetries: 5,
				initialRetryMs: 1
			});

			await vi.waitFor(() => expect(onTerminal).toHaveBeenCalledTimes(1));
			expect(onTerminal).toHaveBeenCalledWith(status, 'nope');

			await new Promise((r) => setTimeout(r, 20));
			expect(fetchImpl).toHaveBeenCalledTimes(1);

			conn.close();
		}
	);

	it('still retries on 503 (non-terminal) and never fires onTerminal', async () => {
		const fetchImpl = vi.fn(async () => jsonResponse(503, 'unavailable'));
		const onTerminal = vi.fn();
		const onReconnect = vi.fn();
		const onRetriesExhausted = vi.fn();

		const conn = connectSse('http://x/events/stream', {
			fetchImpl,
			onEvent: () => {},
			onTerminal,
			onReconnect,
			onRetriesExhausted,
			maxRetries: 3,
			initialRetryMs: 1
		});

		// 1 initial + 3 retries, then the budget is exhausted.
		await vi.waitFor(() => expect(onRetriesExhausted).toHaveBeenCalledTimes(1), {
			timeout: 1000
		});

		expect(onTerminal).not.toHaveBeenCalled();
		expect(fetchImpl).toHaveBeenCalledTimes(4); // initial + 3 retries
		// onReconnect fires before every (re)connect attempt.
		expect(onReconnect).toHaveBeenCalledTimes(4);

		conn.close();
	});

	it('408 and 429 remain retryable (not terminal)', async () => {
		for (const status of [408, 429]) {
			const fetchImpl = vi.fn(async () => jsonResponse(status, 'transient'));
			const onTerminal = vi.fn();
			const onRetriesExhausted = vi.fn();

			const conn = connectSse('http://x/events/stream', {
				fetchImpl,
				onEvent: () => {},
				onTerminal,
				onRetriesExhausted,
				maxRetries: 2,
				initialRetryMs: 1
			});

			await vi.waitFor(() => expect(onRetriesExhausted).toHaveBeenCalledTimes(1));
			expect(onTerminal).not.toHaveBeenCalled();
			expect(fetchImpl).toHaveBeenCalledTimes(3); // initial + 2 retries

			conn.close();
		}
	});

	it('a 200 stream resets retry count and does not fire onTerminal', async () => {
		const fetchImpl = vi.fn(async () => streamingOkResponse());
		const onTerminal = vi.fn();
		const onOpen = vi.fn();

		const conn = connectSse('http://x/events/stream', {
			fetchImpl,
			onEvent: () => {},
			onTerminal,
			onOpen,
			maxRetries: 2,
			initialRetryMs: 1
		});

		await vi.waitFor(() => expect(onOpen).toHaveBeenCalledTimes(1));
		await new Promise((r) => setTimeout(r, 20));
		expect(onTerminal).not.toHaveBeenCalled();
		expect(fetchImpl).toHaveBeenCalledTimes(1);

		conn.close();
	});
});
