/**
 * Shared live-tap registry — ONE network read per (execution, channel), fanned
 * out to many independent renderers.
 *
 * The datastream tap (`GET .../channels/{c}/data?follow=1`) is a single, append-
 * only, HTTP-chunked byte stream. As soon as a channel is consumed in more than
 * one place at once — the instance panel's "Play live" AND a future on-graph edge
 * preview, say, or two stacked previews of the same edge — naively opening a tap
 * per consumer would (a) open N concurrent reads of the SAME bytes (wasteful,
 * and for a lossy `nats-latest` transport each reader sees a DIFFERENT subset),
 * and (b) for an order-sensitive MSE stream, two readers can't even both get a
 * coherent fragmented-MP4. So we open the tap ONCE per key, ref-count it, and
 * broadcast each source chunk to every live sink.
 *
 * Why a hand-rolled broadcaster instead of `ReadableStream.tee()`:
 *  - `tee()` gives exactly two branches; we need an arbitrary, dynamic set
 *    (subscribers come and go).
 *  - `tee()` has a SHARED unbounded backpressure buffer: it holds chunks until
 *    BOTH branches read them, so one slow branch makes the buffer (and memory)
 *    grow without bound and stalls the fast branch. For a live media feed that
 *    is exactly the wrong policy — we never want a slow renderer to stall the
 *    source read (which would back-pressure the producer / fill the kernel
 *    socket buffer) or to retain unbounded history.
 *
 * The source loop therefore NEVER awaits a sink. It reads a chunk and hands it
 * to each sink synchronously; each sink applies its OWN per-render-kind drop
 * policy and the source moves straight to the next `read()`:
 *  - 'mse'  — order-sensitive: a missing fragment corrupts the stream forever, a
 *             gap can't be silently tolerated. So each MSE sink gets a bounded
 *             FIFO queue (~MSE_QUEUE_LIMIT chunks). If a consumer falls that far
 *             behind, we ERROR that one sink's stream (surfacing the gap to its
 *             renderer) rather than stall the source or feed it a corrupt stream.
 *  - 'mjpeg' | 'pcm' — loss-tolerant: a dropped JPEG frame is a skipped image, a
 *             dropped PCM run is a tiny audio glitch. So each such sink keeps only
 *             the LATEST pending chunk (drop-to-latest); a slow renderer simply
 *             renders fewer, newer frames and never falls behind.
 *
 * One source read, per-sink isolation, no cross-sink coupling — mirroring the
 * presentation-side render dispatch in `renderers.ts` and the player shape in
 * `mseStreamPlayer.ts` / `mjpegStreamPlayer.ts`.
 */

import { authFetch } from '$lib/auth/fetch';
import type { LiveRenderKind } from './renderers';

/** Bounded MSE per-sink queue depth before the sink is marked degraded. */
const MSE_QUEUE_LIMIT = 64;

/** Test-only re-export of the MSE queue bound. */
export const MSE_QUEUE_LIMIT_FOR_TEST = MSE_QUEUE_LIMIT;

/** Injectable fetch so unit tests can feed a fake follow-stream Response. */
export type TapFetch = (url: string) => Promise<Response>;

/**
 * A single subscriber's view of a shared tap. Drive the `stream` into a player
 * exactly like the per-consumer body you'd get from `authFetch(...).body`; call
 * `release()` once (idempotent) when the consumer stops, which decrements the
 * ref-count and — on the LAST release — cancels the source read.
 */
export interface LiveTapSubscription {
	/** This subscriber's private ReadableStream of the shared source bytes. */
	stream: ReadableStream<Uint8Array>;
	/**
	 * Resolves once the source `Response` has arrived, with its resolved
	 * `content-type` header (or `null`). Lets a consumer prefer the per-envelope
	 * content-type (which may add runtime params, e.g. `audio/L16;rate=16000`)
	 * over a static declaration — the same refinement the panel did off the tap
	 * Response before the registry owned it. Rejects if the tap open fails.
	 */
	contentType: Promise<string | null>;
	/** Drop this subscriber (idempotent). Last release cancels the source. */
	release(): void;
}

/** Per-sink drop policy + the controller the source loop drives. */
interface Sink {
	readonly kind: LiveRenderKind;
	controller: ReadableStreamDefaultController<Uint8Array>;
	/** Bounded FIFO for 'mse'; for 'mjpeg'/'pcm' holds at most the latest chunk. */
	queue: Uint8Array[];
	/** Set once the sink errored (mse overflow) or was released — stop feeding it. */
	dead: boolean;
	/** A pending pull the source can satisfy directly (avoids growing the queue). */
	pendingPull: boolean;
}

/** One shared tap: the source read + the live set of sinks fanning off it. */
interface Entry {
	readonly key: string;
	refs: number;
	sinks: Set<Sink>;
	/** Cancels the source-side reader; set once the source loop has its reader. */
	cancelSource: () => void;
	/** Resolves with the source Response's content-type (or null). */
	contentType: Promise<string | null>;
	cancelled: boolean;
}

const registry = new Map<string, Entry>();

function keyFor(executionId: string, channelName: string): string {
	return `${executionId}::${channelName}`;
}

/** The follow-tap URL for a channel's bytes. */
function tapUrl(executionId: string, channelName: string): string {
	return `/api/v1/executions/${executionId}/channels/${encodeURIComponent(channelName)}/data?follow=1`;
}

/**
 * Hand one source chunk to one sink under its drop policy. Pure w.r.t. the
 * source loop: never awaits, never throws into the source.
 */
function feedSink(sink: Sink, chunk: Uint8Array): void {
	if (sink.dead) return;

	// If the sink's reader is waiting (pull pending), deliver straight through —
	// no queue growth, lowest latency.
	if (sink.pendingPull) {
		sink.pendingPull = false;
		try {
			sink.controller.enqueue(chunk);
		} catch {
			sink.dead = true;
		}
		return;
	}

	if (sink.kind === 'mse') {
		// Order-sensitive: bounded FIFO. Overflow => unrecoverable gap => error
		// this sink (and only this sink) so its renderer surfaces the break.
		if (sink.queue.length >= MSE_QUEUE_LIMIT) {
			sink.dead = true;
			try {
				sink.controller.error(
					new Error('live tap: MSE consumer fell behind (buffer overflow) — stream gap')
				);
			} catch {
				/* already closed/errored */
			}
			sink.queue.length = 0;
			return;
		}
		sink.queue.push(chunk);
	} else {
		// Loss-tolerant ('mjpeg' | 'pcm'): keep ONLY the latest pending chunk.
		// A slow renderer renders fewer, newer frames; it never falls behind.
		if (sink.queue.length === 0) sink.queue.push(chunk);
		else sink.queue[0] = chunk;
	}
}

/** Build a per-subscriber ReadableStream backed by `sink`'s queue/controller. */
function makeSinkStream(entry: Entry, sink: Sink): ReadableStream<Uint8Array> {
	return new ReadableStream<Uint8Array>({
		start(controller) {
			sink.controller = controller;
		},
		pull(controller) {
			if (sink.dead) return;
			const next = sink.queue.shift();
			if (next !== undefined) {
				controller.enqueue(next);
				return;
			}
			// Nothing buffered: remember the pull so the next source chunk goes
			// straight through. If the source already ended, close.
			if (entry.cancelled) {
				try {
					controller.close();
				} catch {
					/* ignore */
				}
				return;
			}
			sink.pendingPull = true;
		},
		cancel() {
			sink.dead = true;
			sink.queue.length = 0;
		}
	});
}

/** Start the single source-read loop for an entry. */
function startSource(entry: Entry, response: Response): void {
	const body = response.body;
	if (!body) {
		// No stream body: error every sink, mark cancelled.
		entry.cancelled = true;
		for (const s of entry.sinks) {
			s.dead = true;
			try {
				s.controller.error(new Error('live tap: response had no body'));
			} catch {
				/* ignore */
			}
		}
		return;
	}

	const reader = body.getReader();
	entry.cancelSource = () => void reader.cancel().catch(() => {});

	void (async () => {
		try {
			for (;;) {
				const { done, value } = await reader.read();
				if (entry.cancelled) break;
				if (done) break;
				if (!value || value.length === 0) continue;
				// Fan to every live sink WITHOUT awaiting any of them.
				for (const sink of entry.sinks) feedSink(sink, value);
			}
		} catch {
			// Source error: error all sinks (those still open).
			for (const sink of entry.sinks) {
				if (sink.dead) continue;
				sink.dead = true;
				try {
					sink.controller.error(new Error('live tap: source read failed'));
				} catch {
					/* ignore */
				}
			}
			entry.cancelled = true;
			return;
		}
		// Source ended (normal close or cancel): close every open sink.
		entry.cancelled = true;
		for (const sink of entry.sinks) {
			if (sink.dead) continue;
			try {
				sink.controller.close();
			} catch {
				/* ignore */
			}
		}
	})();
}

/**
 * Subscribe to a channel's live tap. The FIRST subscriber for a key opens ONE
 * `?follow=1` read and starts the single source loop; later subscribers join the
 * fan-out. Each subscriber gets its own `stream` (drop policy per `renderKind`)
 * and a `release()`; the LAST release cancels the source and drops the entry.
 *
 * `fetchImpl` is injectable for tests (defaults to {@link authFetch}).
 */
export function subscribe(
	executionId: string,
	channelName: string,
	renderKind: LiveRenderKind,
	fetchImpl: TapFetch = authFetch
): LiveTapSubscription {
	const key = keyFor(executionId, channelName);
	let entry = registry.get(key);

	if (!entry) {
		// First subscriber: create the entry and kick off the single tap open.
		let resolveCt!: (ct: string | null) => void;
		let rejectCt!: (err: unknown) => void;
		const contentType = new Promise<string | null>((res, rej) => {
			resolveCt = res;
			rejectCt = rej;
		});
		// Avoid an unhandledrejection if no consumer awaits contentType.
		void contentType.catch(() => {});

		const created: Entry = {
			key,
			refs: 0,
			sinks: new Set<Sink>(),
			cancelSource: () => {},
			contentType,
			cancelled: false
		};
		registry.set(key, created);
		entry = created;

		void (async () => {
			try {
				const r = await fetchImpl(tapUrl(executionId, channelName));
				if (!r.ok) throw new Error(`live tap failed: ${r.status}`);
				resolveCt(r.headers.get('content-type'));
				// The entry may already have been fully released before the fetch
				// resolved — if so, don't start reading.
				if (created.cancelled || created.refs === 0) {
					try {
						await r.body?.cancel();
					} catch {
						/* ignore */
					}
					return;
				}
				startSource(created, r);
			} catch (err) {
				rejectCt(err);
				created.cancelled = true;
				for (const s of created.sinks) {
					s.dead = true;
					try {
						s.controller.error(err instanceof Error ? err : new Error(String(err)));
					} catch {
						/* ignore */
					}
				}
				registry.delete(key);
			}
		})();
	}

	const e = entry;
	e.refs += 1;

	const sink: Sink = {
		kind: renderKind,
		// Replaced synchronously in the stream's start() below.
		controller: undefined as unknown as ReadableStreamDefaultController<Uint8Array>,
		queue: [],
		dead: false,
		pendingPull: false
	};
	const stream = makeSinkStream(e, sink);
	e.sinks.add(sink);

	let released = false;
	return {
		stream,
		contentType: e.contentType,
		release() {
			if (released) return;
			released = true;
			sink.dead = true;
			sink.queue.length = 0;
			e.sinks.delete(sink);
			e.refs -= 1;
			if (e.refs <= 0) {
				e.cancelled = true;
				e.cancelSource();
				registry.delete(key);
			}
		}
	};
}

/** Test-only: number of live entries (shared taps) currently open. */
export function _entryCount(): number {
	return registry.size;
}

/** Test-only: reset the singleton between tests. */
export function _reset(): void {
	for (const e of registry.values()) {
		e.cancelled = true;
		e.cancelSource();
	}
	registry.clear();
}
