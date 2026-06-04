/**
 * Wrap raw little-endian PCM samples (the bytes an `audio/L16` / `audio/pcm`
 * channel carries) into a self-contained WAV blob the browser `<audio>` element
 * can decode. `<audio>` cannot play headerless PCM, so we synthesise the 44-byte
 * canonical RIFF/WAVE header in front of the sample bytes and re-tag the blob as
 * `audio/wav`.
 *
 * Format params (rate / channels / bits) ride on the content_type, e.g.
 * `audio/L16;rate=16000;channels=1`. `parsePcmParams` pulls them out with sane
 * defaults (channels=1, bits=16).
 *
 * ENDIANNESS: this assumes the producer emits **little-endian** 16-bit samples
 * (which is what our pipeline does — the producer streams the raw frames from a
 * standard WAV via Python's `wave` module, and WAV PCM is little-endian; WAV's
 * data chunk is also little-endian, so no byte-swap is needed). The `audio/L16`
 * label is used loosely here for "raw 16-bit PCM" — note RFC 2586's strict
 * `audio/L16` is big-endian, so a spec-conformant L16 source would need a swap.
 */

export interface PcmParams {
	/** Sample rate in Hz. */
	rate: number;
	/** Channel count (interleaved). Default 1. */
	channels: number;
	/** Bits per sample. Default 16. */
	bits: number;
}

/** A content_type names raw PCM the `<audio>` element can't decode directly. */
export function isRawPcm(contentType: string): boolean {
	const base = contentType.split(';', 1)[0].trim().toLowerCase();
	return base === 'audio/l16' || base === 'audio/pcm';
}

/**
 * Parse `rate` / `channels` / `bits` from a PCM content_type's params, e.g.
 * `audio/L16;rate=16000;channels=1`. `rate` falls back to 16000 when absent
 * (a reasonable VAD/STT default); channels→1, bits→16.
 */
export function parsePcmParams(contentType: string): PcmParams {
	const out: PcmParams = { rate: 16000, channels: 1, bits: 16 };
	const parts = contentType.split(';').slice(1);
	for (const part of parts) {
		const [rawKey, rawVal] = part.split('=');
		if (rawVal === undefined) continue;
		const key = rawKey.trim().toLowerCase();
		const n = Number.parseInt(rawVal.trim(), 10);
		if (!Number.isFinite(n) || n <= 0) continue;
		if (key === 'rate') out.rate = n;
		else if (key === 'channels') out.channels = n;
		else if (key === 'bits' || key === 'bitspersample') out.bits = n;
	}
	return out;
}

/**
 * Prepend a 44-byte canonical PCM WAV header to `pcm` and return an
 * `audio/wav` Blob. `pcm` is the raw interleaved little-endian sample bytes.
 */
export function pcmToWavBlob(pcm: Uint8Array, params: PcmParams): Blob {
	const { rate, channels, bits } = params;
	const bytesPerSample = Math.max(1, Math.floor(bits / 8));
	const blockAlign = channels * bytesPerSample;
	const byteRate = rate * blockAlign;
	const dataLen = pcm.byteLength;

	const header = new ArrayBuffer(44);
	const view = new DataView(header);

	const writeAscii = (offset: number, s: string) => {
		for (let i = 0; i < s.length; i++) view.setUint8(offset + i, s.charCodeAt(i));
	};

	// RIFF chunk descriptor.
	writeAscii(0, 'RIFF');
	view.setUint32(4, 36 + dataLen, true); // ChunkSize = 36 + Subchunk2Size
	writeAscii(8, 'WAVE');
	// "fmt " sub-chunk.
	writeAscii(12, 'fmt ');
	view.setUint32(16, 16, true); // Subchunk1Size (16 for PCM)
	view.setUint16(20, 1, true); // AudioFormat = 1 (PCM)
	view.setUint16(22, channels, true);
	view.setUint32(24, rate, true);
	view.setUint32(28, byteRate, true);
	view.setUint16(32, blockAlign, true);
	view.setUint16(34, bits, true);
	// "data" sub-chunk.
	writeAscii(36, 'data');
	view.setUint32(40, dataLen, true);

	// `BlobPart` wants an `ArrayBuffer`-backed view; a `Uint8Array` over a
	// possibly-`SharedArrayBuffer` widens too far for the strict lib types. Copy
	// the exact sample bytes into a fresh `ArrayBuffer` (also handles a `pcm`
	// that's a sub-view of a larger buffer).
	const body = pcm.slice().buffer as ArrayBuffer;
	return new Blob([header, body], { type: 'audio/wav' });
}
