/**
 * Reusable scrolling-waveform canvas, driven by audio AMPLITUDE rather than a
 * decoded signal — a cheap, always-on PASSIVE visual for an on-edge audio feed.
 *
 * Two source paths behind one {@link startWaveform} entry, branched on the
 * channel's {@link LiveRenderPlan} kind:
 *
 *  - `pcm`  — read the shared `liveTapRegistry` stream, convert each chunk to
 *             Float32 samples with the SAME pure `pcmFramesFromChunk` the live
 *             PCM player uses, compute a per-chunk RMS + peak, push into a ring
 *             buffer and redraw. NO `AudioContext` — this path never makes sound,
 *             it just visualises amplitude, so it's free to run for every audio
 *             edge on the canvas at once.
 *  - `mse` (audio) — fragmented audio can't be RMS'd off the raw bytes, so we
 *             feed it into a MUTED `<audio>` element via `playMseStream` and tap
 *             a Web Audio graph `MediaElementAudioSource → Analyser → Gain(0) →
 *             destination`. `gain.value = 0` keeps it SILENT (passive) while
 *             `analyser.getByteTimeDomainData` per `requestAnimationFrame` gives
 *             us the amplitude to draw. (When this edge becomes the active audio
 *             owner the widget raises the gain to unmute — see the widget.)
 *
 * The amplitude math (`rmsPeak`, the ring `WaveRing`) is pure and unit-tested;
 * the canvas draw + the two acquisition paths are injectable so the orchestration
 * can be exercised under jsdom (which has neither a real `AudioContext` nor a
 * real `MediaSource`).
 */

import { pcmFramesFromChunk } from '$lib/audio/livePcmPlayer';
import { subscribe as subscribeLiveTap } from '$lib/channels/liveTapRegistry';
import { playMseStream } from '$lib/channels/mseStreamPlayer';
import type { LiveRenderPlan } from '$lib/channels/renderers';

/** One amplitude sample: root-mean-square energy + peak magnitude, both [0,1]. */
export interface Amplitude {
	rms: number;
	peak: number;
}

/**
 * Pure: RMS energy + peak magnitude of a Float32 sample block (values in
 * [-1, 1)). Empty input ⇒ `{rms:0, peak:0}`. The unit-testable core of the
 * passive PCM path.
 */
export function rmsPeak(samples: Float32Array): Amplitude {
	const n = samples.length;
	if (n === 0) return { rms: 0, peak: 0 };
	let sumSq = 0;
	let peak = 0;
	for (let i = 0; i < n; i++) {
		const v = samples[i];
		sumSq += v * v;
		const a = v < 0 ? -v : v;
		if (a > peak) peak = a;
	}
	return { rms: Math.sqrt(sumSq / n), peak };
}

/**
 * Pure: amplitude of a byte time-domain buffer as produced by
 * `AnalyserNode.getByteTimeDomainData` — unsigned bytes centred on 128. Returns
 * RMS + peak in [0,1]. Empty ⇒ zero. The unit-testable core of the MSE path.
 */
export function rmsPeakByteTimeDomain(bytes: Uint8Array): Amplitude {
	const n = bytes.length;
	if (n === 0) return { rms: 0, peak: 0 };
	let sumSq = 0;
	let peak = 0;
	for (let i = 0; i < n; i++) {
		const v = (bytes[i] - 128) / 128; // → [-1, 1)
		sumSq += v * v;
		const a = v < 0 ? -v : v;
		if (a > peak) peak = a;
	}
	return { rms: Math.sqrt(sumSq / n), peak };
}

/**
 * A fixed-capacity ring buffer of amplitude bars. `push` overwrites the oldest
 * once full; `toArray` returns them OLDEST→NEWEST so the waveform scrolls left.
 * Pure data structure — no canvas, unit-tested directly.
 */
export class WaveRing {
	private buf: Amplitude[];
	private head = 0; // next write index
	private size = 0;

	constructor(public readonly capacity: number) {
		this.buf = new Array(Math.max(1, capacity));
	}

	push(a: Amplitude): void {
		this.buf[this.head] = a;
		this.head = (this.head + 1) % this.buf.length;
		if (this.size < this.buf.length) this.size += 1;
	}

	/** Oldest→newest snapshot of the current bars. */
	toArray(): Amplitude[] {
		const out: Amplitude[] = [];
		const cap = this.buf.length;
		const start = (this.head - this.size + cap) % cap;
		for (let i = 0; i < this.size; i++) out.push(this.buf[(start + i) % cap]);
		return out;
	}

	get length(): number {
		return this.size;
	}
}

/**
 * Pure draw: render the ring's bars onto a 2D context, oldest on the left. Each
 * bar is a vertical line whose height is the `peak` and whose brighter inner is
 * the `rms`. Drawing is split out so a test can pass a recording stub context.
 */
export function drawWaveform(
	ctx: CanvasRenderingContext2D,
	width: number,
	height: number,
	bars: Amplitude[],
	color: string
): void {
	ctx.clearRect(0, 0, width, height);
	const n = bars.length;
	if (n === 0) return;
	const mid = height / 2;
	const slot = width / n;
	const barW = Math.max(1, slot * 0.7);
	for (let i = 0; i < n; i++) {
		const { rms, peak } = bars[i];
		const x = i * slot + (slot - barW) / 2;
		// Outer (peak) bar, faint.
		const peakH = Math.max(1, peak * height);
		ctx.globalAlpha = 0.4;
		ctx.fillStyle = color;
		ctx.fillRect(x, mid - peakH / 2, barW, peakH);
		// Inner (rms) bar, solid.
		const rmsH = Math.max(1, rms * height);
		ctx.globalAlpha = 1;
		ctx.fillRect(x, mid - rmsH / 2, barW, rmsH);
	}
	ctx.globalAlpha = 1;
}

/** A live waveform handle: stop drawing/reading, and toggle audible output. */
export interface WaveformHandle {
	/** Tear down the read/RAF loop and release any audio nodes. */
	stop(): void;
	/**
	 * Raise/lower the AUDIBLE output of this waveform (only meaningful for the MSE
	 * path, which owns a gain node; the PCM passive path is silent — the widget
	 * plays sound via a separate `playLivePcm` when it claims audio). `audible`
	 * true ⇒ unmute (gain→1 / element unmuted); false ⇒ silent passive.
	 * Returns whether this handle can produce sound on its own (true for MSE).
	 */
	setAudible(audible: boolean): boolean;
}

/** Window/raf seams so the orchestration is testable under jsdom. */
export interface WaveformEnv {
	/** Defaults to `requestAnimationFrame`. */
	raf?: (cb: FrameRequestCallback) => number;
	/** Defaults to `cancelAnimationFrame`. */
	cancelRaf?: (h: number) => void;
	/** AudioContext factory (MSE path). Defaults to the real constructor. */
	makeAudioContext?: () => AudioContext;
	/** Live-tap subscribe seam (PCM path). Defaults to the registry. */
	subscribeTap?: typeof subscribeLiveTap;
	/** MSE player seam. Defaults to `playMseStream`. */
	mse?: typeof playMseStream;
}

export interface WaveformOptions {
	/** The execution whose channel bytes drive the waveform. */
	executionId: string;
	/** The source channel name. */
	channelName: string;
	/** The classified render plan (must be pcm, or mse+audio). */
	plan: LiveRenderPlan;
	/** Bars across the canvas (ring capacity). Defaults to ~64. */
	bars?: number;
	/** Bar color (CSS). Defaults to a neutral foreground. */
	color?: string;
	/** Seams for testing. */
	env?: WaveformEnv;
}

const DEFAULT_BARS = 64;
const DEFAULT_COLOR = '#a3e635';

/**
 * Start a passive scrolling waveform on `canvas`, driven by the channel's
 * amplitude. Branches on `plan.kind`:
 *   - `pcm`            → {@link startPcmWaveform}
 *   - `mse` + audio    → {@link startMseWaveform}
 * Any other plan is a no-op handle (the widget should not call us for video).
 */
export function startWaveform(canvas: HTMLCanvasElement, opts: WaveformOptions): WaveformHandle {
	if (opts.plan.kind === 'pcm') return startPcmWaveform(canvas, opts);
	if (opts.plan.kind === 'mse' && opts.plan.mediaKind === 'audio') {
		return startMseWaveform(canvas, opts);
	}
	return { stop() {}, setAudible: () => false };
}

/** Resolve canvas dims + 2D context once; redraw helper closed over `ring`. */
function makeRenderer(canvas: HTMLCanvasElement, ring: WaveRing, color: string) {
	const ctx = canvas.getContext('2d');
	return () => {
		if (!ctx) return;
		const w = canvas.width || canvas.clientWidth || 120;
		const h = canvas.height || canvas.clientHeight || 36;
		drawWaveform(ctx, w, h, ring.toArray(), color);
	};
}

/**
 * PCM passive waveform: read the shared tap, RMS each chunk, scroll. NO audio
 * output (the widget plays PCM to speakers separately when it claims audio).
 */
export function startPcmWaveform(canvas: HTMLCanvasElement, opts: WaveformOptions): WaveformHandle {
	const env = opts.env ?? {};
	const subscribeTap = env.subscribeTap ?? subscribeLiveTap;
	const raf = env.raf ?? ((cb) => requestAnimationFrame(cb));
	const cancelRaf = env.cancelRaf ?? ((h) => cancelAnimationFrame(h));

	const ring = new WaveRing(opts.bars ?? DEFAULT_BARS);
	const render = makeRenderer(canvas, ring, opts.color ?? DEFAULT_COLOR);

	let stopped = false;
	let carry: Uint8Array = new Uint8Array(0);
	let dirty = false;
	let rafHandle: number | null = null;

	const sub = subscribeTap(opts.executionId, opts.channelName, 'pcm');
	const reader = sub.stream.getReader();

	// Coalesced redraw: chunks land faster than frames; draw at most once per RAF.
	const tick = () => {
		rafHandle = null;
		if (stopped) return;
		if (dirty) {
			dirty = false;
			render();
		}
		if (!stopped) rafHandle = raf(tick);
	};
	rafHandle = raf(tick);

	void (async () => {
		try {
			for (;;) {
				const { done, value } = await reader.read();
				if (stopped || done) break;
				if (!value || value.length === 0) continue;
				const { samples, carry: nextCarry } = pcmFramesFromChunk(carry, value);
				carry = nextCarry;
				if (samples.length > 0) {
					ring.push(rmsPeak(samples));
					dirty = true;
				}
			}
		} catch {
			/* source ended / cancelled — the loop just stops */
		}
	})();

	return {
		stop() {
			if (stopped) return;
			stopped = true;
			if (rafHandle !== null) cancelRaf(rafHandle);
			void reader.cancel().catch(() => {});
			sub.release();
		},
		// PCM passive path produces no sound itself; the widget handles audible
		// playback through playLivePcm. Report false so the widget knows that.
		setAudible: () => false
	};
}

/**
 * MSE audio passive waveform: feed a muted `<audio>` element via `playMseStream`
 * and analyse it through `MediaElementAudioSource → Analyser → Gain(0) →
 * destination`, drawing `getByteTimeDomainData` each RAF. `gain.value = 0` keeps
 * it silent; `setAudible(true)` raises the gain to make this edge audible.
 */
export function startMseWaveform(canvas: HTMLCanvasElement, opts: WaveformOptions): WaveformHandle {
	const env = opts.env ?? {};
	const mse = env.mse ?? playMseStream;
	const raf = env.raf ?? ((cb) => requestAnimationFrame(cb));
	const cancelRaf = env.cancelRaf ?? ((h) => cancelAnimationFrame(h));
	const makeAudioContext =
		env.makeAudioContext ??
		(() =>
			new (window.AudioContext ||
				(window as unknown as { webkitAudioContext: typeof AudioContext })
					.webkitAudioContext)());

	const ring = new WaveRing(opts.bars ?? DEFAULT_BARS);
	const render = makeRenderer(canvas, ring, opts.color ?? DEFAULT_COLOR);

	let stopped = false;
	let rafHandle: number | null = null;

	// A hidden, muted media element carries the decoded audio for analysis. Muted
	// so the ELEMENT itself never sounds; the Gain(0) node keeps the graph silent.
	const audioEl = document.createElement('audio');
	audioEl.muted = true;
	audioEl.autoplay = true;
	(audioEl as HTMLAudioElement).setAttribute('playsinline', '');

	const sub = subscribeLiveTap(opts.executionId, opts.channelName, 'mse');

	const player = mse({
		stream: sub.stream,
		mimeType: opts.plan.mime,
		media: audioEl
	});

	// Web Audio analysis graph. Wrapped in try/catch: jsdom has no AudioContext,
	// and a test injecting a fake may still throw on the source node.
	let ctx: AudioContext | null = null;
	let analyser: AnalyserNode | null = null;
	let gain: GainNode | null = null;
	let data: Uint8Array<ArrayBuffer> | null = null;
	try {
		ctx = makeAudioContext();
		const srcNode = ctx.createMediaElementSource(audioEl);
		analyser = ctx.createAnalyser();
		analyser.fftSize = 1024;
		gain = ctx.createGain();
		gain.gain.value = 0; // silent passive
		srcNode.connect(analyser);
		analyser.connect(gain);
		gain.connect(ctx.destination);
		data = new Uint8Array(new ArrayBuffer(analyser.fftSize));
	} catch {
		ctx = null;
		analyser = null;
		gain = null;
		data = null;
	}

	const tick = () => {
		rafHandle = null;
		if (stopped) return;
		if (analyser && data) {
			analyser.getByteTimeDomainData(data);
			ring.push(rmsPeakByteTimeDomain(data));
			render();
		}
		if (!stopped) rafHandle = raf(tick);
	};
	rafHandle = raf(tick);

	return {
		stop() {
			if (stopped) return;
			stopped = true;
			if (rafHandle !== null) cancelRaf(rafHandle);
			try {
				player.stop();
			} catch {
				/* ignore */
			}
			sub.release();
			try {
				void ctx?.close?.();
			} catch {
				/* ignore */
			}
		},
		setAudible(audible: boolean): boolean {
			if (gain) gain.gain.value = audible ? 1 : 0;
			// The muted element + gain together gate sound; unmute the element too.
			audioEl.muted = !audible;
			if (audible) void ctx?.resume?.();
			return true; // the MSE path can produce sound on its own
		}
	};
}
