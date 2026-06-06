/**
 * Play a LIVE Motion-JPEG byte stream — a data channel of self-contained JPEG
 * frames — by swapping each decoded frame into an `<img>` as it arrives.
 *
 * This is the `mjpeg` render adapter the {@link planLiveRender} registry
 * dispatches to for an `image/jpeg` data channel, the image-sequence analog of
 * the fragmented-media MSE path ({@link import('./mseStreamPlayer').playMseStream}).
 * The datastream tap (`GET .../channels/{c}/data?follow=1`) serves the channel's
 * bytes as ONE concatenated HTTP-chunked stream — the per-`write()` element
 * boundaries are NOT preserved on the wire — so we re-frame client-side by
 * scanning for the JPEG End-Of-Image marker (`FF D9`): every byte run from one
 * frame's start through its EOI is a complete, independently-decodable JPEG.
 * Each complete frame becomes a `Blob` object-URL set on the `<img>`; the prior
 * URL is revoked so a fast feed can't leak blobs.
 *
 * Mirrors `playMseStream`'s shape (a `stop()`-able handle, `onStatus`/
 * `onProgress` callbacks) so the panel drives every live renderer uniformly.
 * Because each chunk is a whole frame, this pairs naturally with the lossy
 * `nats-latest` transport: a dropped frame is just a skipped image, never a
 * corrupt stream (unlike a dropped fragmented-MP4 fragment).
 */

import type { LiveMediaHandle, LiveMediaStatus } from './mseStreamPlayer';

export interface MjpegStreamOptions {
	/** The tap response's `ReadableStream` body (`?follow=1`). */
	stream: ReadableStream<Uint8Array>;
	/** The `<img>` element each decoded frame is swapped into. */
	img: HTMLImageElement;
	/** Blob MIME for each frame (defaults to `image/jpeg`). */
	mime?: string;
	onStatus?: (status: LiveMediaStatus, error?: string) => void;
	/** Reports `(framesRendered, bytesReceived)` as frames land. */
	onProgress?: (frames: number, bytesReceived: number) => void;
}

function concat(a: Uint8Array, b: Uint8Array): Uint8Array {
	const out = new Uint8Array(a.length + b.length);
	out.set(a);
	out.set(b, a.length);
	return out;
}

/**
 * Index of the byte completing the next JPEG EOI marker (`FF D9`) at or after
 * `from`, or -1. Returns the position of the `D9` so the inclusive frame slice
 * is `buf[0 .. idx+1]`. Within JPEG entropy-coded data a literal `FF` is always
 * byte-stuffed (`FF 00`) or a restart marker (`FF D0..D7`), so `FF D9` is
 * unambiguously the end-of-image for the clean baseline JPEGs cv2 emits.
 */
function indexOfEoi(buf: Uint8Array, from: number): number {
	for (let i = Math.max(1, from); i < buf.length; i++) {
		if (buf[i - 1] === 0xff && buf[i] === 0xd9) return i;
	}
	return -1;
}

/** First byte of the next JPEG SOI marker (`FF D8`) at or after `from`, or -1. */
function indexOfSoi(buf: Uint8Array, from: number): number {
	for (let i = Math.max(0, from); i + 1 < buf.length; i++) {
		if (buf[i] === 0xff && buf[i + 1] === 0xd8) return i;
	}
	return -1;
}

/**
 * Start live MJPEG playback into `img`. Returns a handle whose `stop()` cancels
 * the read and revokes the last frame URL.
 */
export function playMjpegStream(opts: MjpegStreamOptions): LiveMediaHandle {
	const { stream, img, mime = 'image/jpeg', onStatus, onProgress } = opts;
	let stopped = false;
	let bytesReceived = 0;
	let frames = 0;
	let lastUrl: string | null = null;
	const reader = stream.getReader();

	const renderFrame = (bytes: Uint8Array) => {
		const url = URL.createObjectURL(new Blob([bytes as BlobPart], { type: mime }));
		img.src = url;
		// The previous frame is already displayed; revoking its blob is safe.
		if (lastUrl) URL.revokeObjectURL(lastUrl);
		lastUrl = url;
	};

	const pump = async () => {
		onStatus?.('streaming');
		// Buffer until the first SOI so any lead-in bytes can't corrupt frame 0.
		let pending: Uint8Array = new Uint8Array(0);
		let sawSoi = false;
		try {
			for (;;) {
				const { done, value } = await reader.read();
				if (stopped) break;
				if (done) break;
				if (!value || value.length === 0) continue;
				bytesReceived += value.length;
				pending = concat(pending, value);

				if (!sawSoi) {
					const soi = indexOfSoi(pending, 0);
					if (soi < 0) continue; // no frame start yet
					if (soi > 0) pending = pending.slice(soi);
					sawSoi = true;
				}

				// Extract every complete SOI..EOI frame now buffered.
				let eoi = indexOfEoi(pending, 0);
				while (eoi !== -1) {
					renderFrame(pending.slice(0, eoi + 1));
					frames += 1;
					onProgress?.(frames, bytesReceived);
					pending = pending.slice(eoi + 1);
					// Re-anchor to the next SOI (drop any inter-frame padding).
					const next = indexOfSoi(pending, 0);
					if (next < 0) {
						sawSoi = false;
						pending = new Uint8Array(0);
						break;
					}
					if (next > 0) pending = pending.slice(next);
					eoi = indexOfEoi(pending, 0);
				}
			}
			if (!stopped) onStatus?.('ended');
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
			if (lastUrl) {
				URL.revokeObjectURL(lastUrl);
				lastUrl = null;
			}
			onStatus?.('stopped');
		}
	};
}
