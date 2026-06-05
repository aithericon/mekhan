import { describe, it, expect } from 'vitest';
import { parseSampleRate, pcmFramesFromChunk } from './livePcmPlayer';

describe('parseSampleRate', () => {
	it('reads rate from an L16 content-type', () => {
		expect(parseSampleRate('audio/L16;rate=16000')).toBe(16000);
		expect(parseSampleRate('audio/L16; rate=44100')).toBe(44100);
		expect(parseSampleRate('audio/L16;rate=8000;channels=1')).toBe(8000);
	});
	it('falls back when absent or malformed', () => {
		expect(parseSampleRate(null)).toBe(16000);
		expect(parseSampleRate('audio/L16')).toBe(16000);
		expect(parseSampleRate('audio/L16;rate=0', 22050)).toBe(22050);
		expect(parseSampleRate('audio/wav', 48000)).toBe(48000);
	});
});

// Helper: build a little-endian Int16 byte array from sample values.
function le16(values: number[]): Uint8Array {
	const out = new Uint8Array(values.length * 2);
	const dv = new DataView(out.buffer);
	values.forEach((v, i) => dv.setInt16(i * 2, v, true));
	return out;
}

describe('pcmFramesFromChunk', () => {
	it('converts whole Int16 frames to Float32 in [-1, 1)', () => {
		const bytes = le16([0, 32767, -32768, 16384]);
		const { samples, carry } = pcmFramesFromChunk(new Uint8Array(0), bytes);
		expect(carry.length).toBe(0);
		expect(samples.length).toBe(4);
		expect(samples[0]).toBeCloseTo(0, 6);
		expect(samples[1]).toBeCloseTo(32767 / 32768, 6);
		expect(samples[2]).toBeCloseTo(-1, 6);
		expect(samples[3]).toBeCloseTo(0.5, 6);
	});

	it('carries a trailing odd byte into the next chunk', () => {
		// 3 bytes: one whole frame + one dangling byte.
		const first = new Uint8Array([0x00, 0x40, 0x11]); // frame {0x4000=16384}, carry 0x11
		const r1 = pcmFramesFromChunk(new Uint8Array(0), first);
		expect(r1.samples.length).toBe(1);
		expect(r1.samples[0]).toBeCloseTo(16384 / 32768, 6);
		expect(Array.from(r1.carry)).toEqual([0x11]);

		// Next chunk supplies the byte that completes the carried frame.
		const second = new Uint8Array([0x22, 0x00, 0x80]); // completes 0x2211, then carry 0x80
		const r2 = pcmFramesFromChunk(r1.carry, second);
		// stitched: 0x11 0x22 | 0x00 0x80 → frames {0x2211, 0x8000=-32768}
		expect(r2.samples.length).toBe(2);
		const dv = new DataView(new Int16Array([0x2211]).buffer);
		expect(r2.samples[0]).toBeCloseTo(dv.getInt16(0, true) / 32768, 6);
		expect(r2.samples[1]).toBeCloseTo(-1, 6);
		expect(r2.carry.length).toBe(0);
	});

	it('reassembles a stream split at arbitrary byte boundaries', () => {
		const full = le16([100, -200, 300, -400, 500, -600]);
		// Split into ragged pieces (some odd-length).
		const pieces = [full.slice(0, 1), full.slice(1, 4), full.slice(4, 5), full.slice(5)];
		let carry: Uint8Array = new Uint8Array(0);
		const got: number[] = [];
		for (const p of pieces) {
			const r = pcmFramesFromChunk(carry, p);
			carry = r.carry;
			got.push(...r.samples);
		}
		expect(carry.length).toBe(0);
		const expected = [100, -200, 300, -400, 500, -600].map((v) => v / 32768);
		expect(got.length).toBe(expected.length);
		got.forEach((s, i) => expect(s).toBeCloseTo(expected[i], 6));
	});

	it('yields no samples for a single carried byte', () => {
		const { samples, carry } = pcmFramesFromChunk(new Uint8Array(0), new Uint8Array([0x7f]));
		expect(samples.length).toBe(0);
		expect(Array.from(carry)).toEqual([0x7f]);
	});
});
