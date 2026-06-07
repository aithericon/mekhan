/**
 * Play a LIVE NDJSON joint-state stream — a data channel of newline-delimited
 * JSON joint frames — by surfacing the LATEST decoded frame to a URDF twin as it
 * arrives.
 *
 * This is the joint-stream analog of {@link import('./mjpegStreamPlayer').playMjpegStream}:
 * the datastream tap (`GET .../channels/{c}/data?follow=1`) serves the channel's
 * bytes as ONE concatenated HTTP-chunked stream — the per-`write()` element
 * boundaries are NOT preserved on the wire — so we re-frame client-side. Where
 * MJPEG reframes on the `FF D9` EOI byte marker, a joint stream is text and each
 * `write()` is one compact JSON object terminated by `\n`, so the frame boundary
 * is simply the newline (NDJSON). We decode incrementally with a streaming
 * `TextDecoder` (so a multi-byte UTF-8 codepoint split across two chunks is held
 * until complete), split the accumulated text on `\n`, and JSON.parse each
 * COMPLETE line; a trailing partial line (no terminating `\n` yet) is carried
 * across chunks until its newline arrives.
 *
 * Like the MJPEG path this pairs naturally with the lossy `nats-latest`
 * transport and is loss-tolerant by design: a dropped joint frame is just a
 * skipped pose, never a corrupt stream. When a single chunk carries several
 * complete lines we emit only the LATEST valid one (drop-to-latest) — the twin
 * only ever wants the current pose, not the intervening history — mirroring the
 * registry's 'mjpeg'/'pcm' keep-only-latest sink policy in `liveTapRegistry.ts`.
 *
 * Returns a `stop()`-able handle (mirroring `playMjpegStream`) whose `stop()`
 * cancels the source reader.
 */

/** A decoded joint frame: parallel name/position arrays for a URDF pose. */
export interface UrdfJointFrame {
	joint_names: string[];
	positions: number[];
}

export interface UrdfStreamOptions {
	/** The tap response's `ReadableStream` body (`?follow=1`). */
	stream: ReadableStream<Uint8Array>;
	/** Called with the LATEST valid joint frame as lines complete. */
	onFrame: (frame: UrdfJointFrame) => void;
	/** Coarse lifecycle status: `streaming` | `ended` | `error` | `stopped`. */
	onStatus?: (status: string) => void;
}

/** A stoppable live-player handle (mirrors `playMjpegStream`). */
export interface UrdfStreamHandle {
	stop(): void;
}

/**
 * Validate a parsed NDJSON line as a joint frame: `joint_names` an array of
 * strings, `positions` an array of finite numbers. Returns the typed frame or
 * `null` (a malformed line is dropped, not thrown — loss-tolerant).
 */
export function parseJointFrame(value: unknown): UrdfJointFrame | null {
	if (typeof value !== 'object' || value === null) return null;
	const obj = value as Record<string, unknown>;
	const { joint_names, positions } = obj;
	if (!Array.isArray(joint_names) || !Array.isArray(positions)) return null;
	if (!joint_names.every((n) => typeof n === 'string')) return null;
	if (!positions.every((p) => typeof p === 'number' && Number.isFinite(p))) return null;
	return { joint_names: joint_names as string[], positions: positions as number[] };
}

/**
 * Start live NDJSON joint-stream playback. Returns a handle whose `stop()`
 * cancels the read.
 */
export function playUrdfStream(opts: UrdfStreamOptions): UrdfStreamHandle {
	const { stream, onFrame, onStatus } = opts;
	let stopped = false;
	const reader = stream.getReader();
	const decoder = new TextDecoder();

	const pump = async () => {
		onStatus?.('streaming');
		// Carries any trailing line fragment (text after the last `\n`) across chunks.
		let pending = '';
		try {
			for (;;) {
				const { done, value } = await reader.read();
				if (stopped) break;
				if (done) break;
				// `stream: true` holds a partial multi-byte UTF-8 codepoint until complete.
				if (value && value.length > 0) pending += decoder.decode(value, { stream: true });
				else continue;

				const nl = pending.lastIndexOf('\n');
				if (nl < 0) continue; // no complete line yet — keep buffering

				// Everything up to the last newline is one-or-more complete lines;
				// the remainder (possibly empty) is the next partial line.
				const complete = pending.slice(0, nl);
				pending = pending.slice(nl + 1);

				// Drop-to-latest: among the complete lines, surface only the newest
				// valid frame. Scan from the end so the first parse wins.
				let latest: UrdfJointFrame | null = null;
				const lines = complete.split('\n');
				for (let i = lines.length - 1; i >= 0; i--) {
					const line = lines[i].trim();
					if (line === '') continue; // ignore blank lines
					try {
						const frame = parseJointFrame(JSON.parse(line));
						if (frame) {
							latest = frame;
							break;
						}
					} catch {
						/* malformed line — drop it, keep scanning older lines */
					}
				}
				if (latest) onFrame(latest);
			}
			// Flush any final buffered line on a clean close.
			if (!stopped) {
				const line = pending.trim();
				if (line !== '') {
					try {
						const frame = parseJointFrame(JSON.parse(line));
						if (frame) onFrame(frame);
					} catch {
						/* ignore trailing garbage */
					}
				}
				onStatus?.('ended');
			}
		} catch (e) {
			if (!stopped) onStatus?.(`error: ${e instanceof Error ? e.message : String(e)}`);
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
