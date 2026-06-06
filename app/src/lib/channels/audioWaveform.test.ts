import { describe, it, expect, vi } from 'vitest';
import {
	rmsPeak,
	rmsPeakByteTimeDomain,
	WaveRing,
	drawWaveform,
	startWaveform,
	type Amplitude
} from './audioWaveform';
import type { LiveRenderPlan } from './renderers';

describe('rmsPeak — Float32 amplitude', () => {
	it('is zero for empty input', () => {
		expect(rmsPeak(new Float32Array(0))).toEqual<Amplitude>({ rms: 0, peak: 0 });
	});

	it('computes RMS and peak for a known block', () => {
		// [1, -1, 1, -1] → rms = sqrt((1+1+1+1)/4) = 1, peak = 1.
		const { rms, peak } = rmsPeak(Float32Array.from([1, -1, 1, -1]));
		expect(rms).toBeCloseTo(1, 6);
		expect(peak).toBeCloseTo(1, 6);
	});

	it('takes the absolute value for peak (negative magnitude counts)', () => {
		const { peak } = rmsPeak(Float32Array.from([0.1, -0.9, 0.2]));
		expect(peak).toBeCloseTo(0.9, 6);
	});

	it('a constant DC block has rms == |value|', () => {
		const { rms, peak } = rmsPeak(Float32Array.from([0.5, 0.5, 0.5]));
		expect(rms).toBeCloseTo(0.5, 6);
		expect(peak).toBeCloseTo(0.5, 6);
	});
});

describe('rmsPeakByteTimeDomain — analyser bytes centred on 128', () => {
	it('is zero for empty input', () => {
		expect(rmsPeakByteTimeDomain(new Uint8Array(0))).toEqual<Amplitude>({ rms: 0, peak: 0 });
	});

	it('treats 128 as silence', () => {
		const { rms, peak } = rmsPeakByteTimeDomain(Uint8Array.from([128, 128, 128]));
		expect(rms).toBeCloseTo(0, 6);
		expect(peak).toBeCloseTo(0, 6);
	});

	it('maps full-scale 0/255 swing to ~1', () => {
		const { peak } = rmsPeakByteTimeDomain(Uint8Array.from([0, 255, 0, 255]));
		expect(peak).toBeCloseTo(1, 1);
	});
});

describe('WaveRing — fixed-capacity scrolling buffer', () => {
	it('returns pushes oldest→newest while under capacity', () => {
		const r = new WaveRing(4);
		r.push({ rms: 0.1, peak: 0.1 });
		r.push({ rms: 0.2, peak: 0.2 });
		expect(r.length).toBe(2);
		expect(r.toArray().map((a) => a.rms)).toEqual([0.1, 0.2]);
	});

	it('overwrites the oldest once full (scrolls left)', () => {
		const r = new WaveRing(3);
		for (const v of [1, 2, 3, 4, 5]) r.push({ rms: v, peak: v });
		expect(r.length).toBe(3);
		// Oldest two (1,2) dropped; newest three remain oldest→newest.
		expect(r.toArray().map((a) => a.rms)).toEqual([3, 4, 5]);
	});

	it('clamps a zero/negative capacity to at least one slot', () => {
		const r = new WaveRing(0);
		r.push({ rms: 0.5, peak: 0.5 });
		r.push({ rms: 0.6, peak: 0.6 });
		expect(r.toArray().map((a) => a.rms)).toEqual([0.6]);
	});
});

describe('drawWaveform — pure 2D draw', () => {
	function recordingCtx() {
		const calls: string[] = [];
		const ctx = {
			globalAlpha: 1,
			fillStyle: '',
			clearRect: (...a: number[]) => calls.push(`clear ${a.join(',')}`),
			fillRect: (...a: number[]) => calls.push(`fill ${a.map((n) => Math.round(n)).join(',')}`)
		} as unknown as CanvasRenderingContext2D;
		return { ctx, calls };
	}

	it('clears and draws nothing for an empty ring', () => {
		const { ctx, calls } = recordingCtx();
		drawWaveform(ctx, 100, 40, [], '#fff');
		expect(calls).toEqual(['clear 0,0,100,40']);
	});

	it('draws two rects (peak + rms) per bar', () => {
		const { ctx, calls } = recordingCtx();
		drawWaveform(ctx, 100, 40, [{ rms: 0.5, peak: 0.8 }], '#fff');
		expect(calls[0]).toBe('clear 0,0,100,40');
		expect(calls.filter((c) => c.startsWith('fill')).length).toBe(2);
	});
});

describe('startWaveform — plan dispatch', () => {
	it('is a no-op handle for a video plan (never called for video, but safe)', () => {
		const plan: LiveRenderPlan = { kind: 'mse', mediaKind: 'video', mime: 'video/mp4' };
		const canvas = document.createElement('canvas');
		const h = startWaveform(canvas, { executionId: 'e', channelName: 'c', plan });
		expect(h.setAudible(true)).toBe(false);
		expect(() => h.stop()).not.toThrow();
	});

	it('drives the PCM path: reads the tap, RMS-pushes, and stops cleanly', async () => {
		const plan: LiveRenderPlan = { kind: 'pcm', mediaKind: 'audio', mime: 'audio/L16;rate=8000' };
		const canvas = document.createElement('canvas');
		canvas.width = 80;
		canvas.height = 32;

		// One PCM chunk: two Int16 frames [16384, -16384] little-endian.
		const pcm = new Uint8Array([0x00, 0x40, 0x00, 0xc0]);
		let released = false;
		const fakeStream = new ReadableStream<Uint8Array>({
			start(controller) {
				controller.enqueue(pcm);
				controller.close();
			}
		});
		const subscribeTap = vi.fn(() => ({
			stream: fakeStream,
			contentType: Promise.resolve('audio/L16;rate=8000'),
			release: () => {
				released = true;
			}
		}));

		// Synchronous-ish RAF: fire once then go quiet so the loop terminates.
		let fired = 0;
		const raf = (cb: FrameRequestCallback) => {
			if (fired++ < 2) queueMicrotask(() => cb(0));
			return fired;
		};
		const cancelRaf = vi.fn();

		const h = startWaveform(canvas, {
			executionId: 'exec-1',
			channelName: 'audio_out',
			plan,
			env: { subscribeTap: subscribeTap as never, raf, cancelRaf }
		});

		expect(subscribeTap).toHaveBeenCalledWith('exec-1', 'audio_out', 'pcm');
		// PCM passive path reports it cannot self-sound.
		expect(h.setAudible(true)).toBe(false);

		// Let the read loop drain.
		await new Promise((r) => setTimeout(r, 0));

		h.stop();
		expect(released).toBe(true);
	});
});
