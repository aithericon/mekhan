/**
 * Play a LIVE 16-bit PCM byte stream through Web Audio, scheduling each chunk
 * the moment it arrives so audio starts while the producer is still emitting.
 *
 * The datastream tap (`GET .../channels/{c}/data?follow=1`) serves raw
 * little-endian Int16 mono PCM (content-type `audio/L16;rate=…`) as an
 * HTTP-chunked stream. An `<audio>` element can't progressively decode headerless
 * PCM, so we read the `ReadableStream` ourselves, convert each chunk to
 * Float32 samples, and schedule them back-to-back on an `AudioContext` timeline —
 * the classic gapless streaming-PCM technique. Network chunks don't align to
 * sample (2-byte) boundaries, so a trailing odd byte is carried into the next
 * chunk by {@link pcmFramesFromChunk}.
 */

/** Parse the sample rate out of an `audio/L16;rate=16000` content-type. */
export function parseSampleRate(contentType: string | null, fallback = 16000): number {
	if (!contentType) return fallback;
	const m = /rate=(\d+)/i.exec(contentType);
	const rate = m ? Number(m[1]) : NaN;
	return Number.isFinite(rate) && rate > 0 ? rate : fallback;
}

/**
 * Convert one incoming byte chunk (prepended with any carried trailing byte from
 * the previous chunk) into Float32 samples in [-1, 1), returning the leftover
 * odd byte to carry forward. Pure — the unit-testable core of the player.
 */
export function pcmFramesFromChunk(
	prevCarry: Uint8Array,
	incoming: Uint8Array
): { samples: Float32Array; carry: Uint8Array } {
	// Stitch the carried odd byte onto the front (avoid a copy when there's none).
	let bytes: Uint8Array;
	if (prevCarry.length === 0) {
		bytes = incoming;
	} else {
		bytes = new Uint8Array(prevCarry.length + incoming.length);
		bytes.set(prevCarry, 0);
		bytes.set(incoming, prevCarry.length);
	}
	const usable = bytes.length - (bytes.length % 2); // whole Int16 frames only
	const n = usable / 2;
	const samples = new Float32Array(n);
	const view = new DataView(bytes.buffer, bytes.byteOffset, usable);
	for (let i = 0; i < n; i++) {
		samples[i] = view.getInt16(i * 2, true) / 32768; // little-endian → [-1, 1)
	}
	const carry = usable < bytes.length ? bytes.slice(usable) : EMPTY;
	return { samples, carry };
}

const EMPTY = new Uint8Array(0);

export type LivePcmStatus = 'streaming' | 'ended' | 'stopped' | 'error';

export interface LivePcmHandle {
	/** Stop playback, cancel the network read, and release the AudioContext. */
	stop(): void;
}

export interface LivePcmOptions {
	/** The tap response's `ReadableStream` body. */
	stream: ReadableStream<Uint8Array>;
	/** Sample rate (parse from the content-type with {@link parseSampleRate}). */
	sampleRate: number;
	/** Inject an AudioContext (tests pass a fake). Defaults to a real one. */
	audioContext?: AudioContext;
	onStatus?: (status: LivePcmStatus, error?: string) => void;
	/** Reports `(secondsScheduled, bytesReceived)` as chunks land. */
	onProgress?: (secondsScheduled: number, bytesReceived: number) => void;
}

/**
 * Start live playback. Returns a handle whose `stop()` tears everything down.
 * Must be called from a user gesture (button click) so the browser lets the
 * AudioContext start.
 */
export function playLivePcm(opts: LivePcmOptions): LivePcmHandle {
	const { stream, sampleRate, onStatus, onProgress } = opts;
	const ctx =
		opts.audioContext ??
		new (window.AudioContext || (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext)(
			{ sampleRate }
		);
	const ownsContext = !opts.audioContext;

	let stopped = false;
	let carry: Uint8Array = EMPTY;
	let playHead = ctx.currentTime;
	let bytesReceived = 0;
	let samplesScheduled = 0;
	const reader = stream.getReader();

	void ctx.resume?.();
	onStatus?.('streaming');

	function schedule(samples: Float32Array) {
		if (samples.length === 0) return;
		const buffer = ctx.createBuffer(1, samples.length, sampleRate);
		buffer.getChannelData(0).set(samples);
		const src = ctx.createBufferSource();
		src.buffer = buffer;
		src.connect(ctx.destination);
		// Never schedule in the past — a slow producer lets currentTime overtake.
		const at = Math.max(playHead, ctx.currentTime);
		src.start(at);
		playHead = at + buffer.duration;
		samplesScheduled += samples.length;
	}

	async function pump() {
		try {
			for (;;) {
				const { done, value } = await reader.read();
				if (done || stopped) break;
				if (!value || value.length === 0) continue;
				bytesReceived += value.length;
				const { samples, carry: nextCarry } = pcmFramesFromChunk(carry, value);
				carry = nextCarry;
				schedule(samples);
				onProgress?.(samplesScheduled / sampleRate, bytesReceived);
			}
			if (!stopped) onStatus?.('ended');
		} catch (e) {
			if (!stopped) onStatus?.('error', e instanceof Error ? e.message : String(e));
		} finally {
			if (ownsContext) {
				// Let the already-scheduled tail finish, then release.
				const tailMs = Math.max(0, (playHead - ctx.currentTime) * 1000) + 200;
				setTimeout(() => void ctx.close?.(), tailMs);
			}
		}
	}

	void pump();

	return {
		stop() {
			if (stopped) return;
			stopped = true;
			void reader.cancel().catch(() => {});
			if (ownsContext) void ctx.close?.();
			onStatus?.('stopped');
		}
	};
}
