import { describe, it, expect } from 'vitest';
import { isRawPcm, parsePcmParams, pcmToWavBlob } from './pcmWav';

describe('isRawPcm', () => {
	it('flags L16 / pcm (case + params tolerant)', () => {
		expect(isRawPcm('audio/L16;rate=16000;channels=1')).toBe(true);
		expect(isRawPcm('audio/pcm')).toBe(true);
		expect(isRawPcm('AUDIO/L16')).toBe(true);
	});
	it('rejects already-playable types', () => {
		expect(isRawPcm('audio/wav')).toBe(false);
		expect(isRawPcm('audio/mpeg')).toBe(false);
		expect(isRawPcm('video/mp4')).toBe(false);
	});
});

describe('parsePcmParams', () => {
	it('parses rate/channels/bits', () => {
		expect(parsePcmParams('audio/L16;rate=16000;channels=1')).toEqual({
			rate: 16000,
			channels: 1,
			bits: 16
		});
		expect(parsePcmParams('audio/L16;rate=44100;channels=2;bits=24')).toEqual({
			rate: 44100,
			channels: 2,
			bits: 24
		});
	});
	it('defaults missing params (channels=1, bits=16, rate=16000)', () => {
		expect(parsePcmParams('audio/pcm')).toEqual({ rate: 16000, channels: 1, bits: 16 });
	});
});

describe('pcmToWavBlob', () => {
	it('produces an audio/wav blob with a 44-byte header', () => {
		const pcm = new Uint8Array([0, 0, 1, 0, 2, 0, 3, 0]); // 4 samples, mono 16-bit
		const blob = pcmToWavBlob(pcm, { rate: 16000, channels: 1, bits: 16 });
		expect(blob.type).toBe('audio/wav');
		expect(blob.size).toBe(44 + pcm.byteLength);
	});

	it('writes a correct RIFF/WAVE header', async () => {
		const pcm = new Uint8Array(8);
		const blob = pcmToWavBlob(pcm, { rate: 16000, channels: 1, bits: 16 });
		const buf = new Uint8Array(await blob.arrayBuffer());
		const ascii = (o: number, n: number) =>
			String.fromCharCode(...buf.slice(o, o + n));
		expect(ascii(0, 4)).toBe('RIFF');
		expect(ascii(8, 4)).toBe('WAVE');
		expect(ascii(12, 4)).toBe('fmt ');
		expect(ascii(36, 4)).toBe('data');
		const view = new DataView(buf.buffer);
		expect(view.getUint32(4, true)).toBe(36 + pcm.byteLength); // ChunkSize
		expect(view.getUint16(20, true)).toBe(1); // PCM
		expect(view.getUint16(22, true)).toBe(1); // channels
		expect(view.getUint32(24, true)).toBe(16000); // sample rate
		expect(view.getUint32(28, true)).toBe(16000 * 2); // byte rate
		expect(view.getUint16(32, true)).toBe(2); // block align
		expect(view.getUint16(34, true)).toBe(16); // bits
		expect(view.getUint32(40, true)).toBe(pcm.byteLength); // data size
	});
});
