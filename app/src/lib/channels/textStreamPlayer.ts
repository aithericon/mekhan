/**
 * Play a LIVE text byte stream — a `text/*` data channel — by decoding UTF-8
 * incrementally and appending each decoded run into a console-style tail.
 *
 * This is the `text` render adapter the {@link planLiveRender} registry
 * dispatches to (text/plain echo feeds, CSV/log streams, LLM token streams).
 * The datastream tap (`GET .../channels/{c}/data?follow=1`) serves the
 * channel's bytes as ONE concatenated HTTP-chunked stream with per-`write()`
 * boundaries NOT preserved, so a multi-byte UTF-8 code point can be split
 * across chunks — the streaming `TextDecoder` (`{ stream: true }`) carries the
 * partial sequence over instead of emitting replacement characters.
 *
 * Mirrors `playMjpegStream`'s shape (a `stop()`-able handle, `onStatus`/
 * `onProgress` callbacks) so panels drive every live renderer uniformly. The
 * adapter is ORDER-SENSITIVE (a dropped chunk is missing text, not a skipped
 * frame), so the shared tap registry queues `text` sinks FIFO like `mse`,
 * never drop-to-latest.
 */

import type { LiveMediaHandle, LiveMediaStatus } from './mseStreamPlayer';

export interface TextStreamOptions {
	/** The tap response's `ReadableStream` body (`?follow=1`). */
	stream: ReadableStream<Uint8Array>;
	/** Receives each decoded text run, in order. The consumer owns accumulation
	 *  (typically `tailCap(prev + chunk, …)` into a scrolling `<pre>`). */
	onText: (chunk: string) => void;
	onStatus?: (status: LiveMediaStatus, error?: string) => void;
	/** Reports `(charsDecoded, bytesReceived)` as text lands. */
	onProgress?: (chars: number, bytesReceived: number) => void;
}

/**
 * Keep only the trailing `cap` characters of an accumulated tail — the
 * console-buffer policy consumers apply so a long-running feed can't grow a
 * component's state without bound. Exported for reuse + tests.
 */
export function tailCap(text: string, cap: number): string {
	return text.length > cap ? text.slice(text.length - cap) : text;
}

/**
 * Start live text decoding. Returns a handle whose `stop()` cancels the read.
 */
export function playTextStream(opts: TextStreamOptions): LiveMediaHandle {
	const { stream, onText, onStatus, onProgress } = opts;
	let stopped = false;
	let bytesReceived = 0;
	let chars = 0;
	const reader = stream.getReader();
	// Non-fatal: undecodable bytes become U+FFFD instead of killing the tail.
	const decoder = new TextDecoder('utf-8', { fatal: false });

	const pump = async () => {
		onStatus?.('streaming');
		try {
			for (;;) {
				const { done, value } = await reader.read();
				if (stopped) break;
				if (done) break;
				if (!value || value.length === 0) continue;
				bytesReceived += value.length;
				// `stream: true` holds back a trailing partial code point for the
				// next chunk instead of emitting a replacement character.
				const text = decoder.decode(value, { stream: true });
				if (text.length > 0) {
					chars += text.length;
					onText(text);
					onProgress?.(chars, bytesReceived);
				}
			}
			if (!stopped) {
				// Flush any buffered partial sequence (renders as U+FFFD if truncated).
				const rest = decoder.decode();
				if (rest.length > 0) {
					chars += rest.length;
					onText(rest);
					onProgress?.(chars, bytesReceived);
				}
				onStatus?.('ended');
			}
		} catch (e) {
			if (!stopped) onStatus?.('error', e instanceof Error ? e.message : String(e));
		}
	};

	void pump();

	return {
		stop() {
			if (stopped) return;
			stopped = true;
			void reader.cancel().catch(() => {});
			onStatus?.('stopped');
		}
	};
}
