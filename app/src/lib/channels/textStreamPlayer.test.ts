import { describe, it, expect, vi } from 'vitest';
import { playTextStream, tailCap } from './textStreamPlayer';

const enc = new TextEncoder();

/** A source the test drives explicitly: push() a chunk, end() to close, error(). */
function controllableSource() {
	let ctrl!: ReadableStreamDefaultController<Uint8Array>;
	let cancelled = false;
	const stream = new ReadableStream<Uint8Array>({
		start(c) {
			ctrl = c;
		},
		cancel() {
			cancelled = true;
		}
	});
	return {
		stream,
		push: (text: string) => ctrl.enqueue(enc.encode(text)),
		pushBytes: (b: Uint8Array) => ctrl.enqueue(b),
		end: () => ctrl.close(),
		fail: (e: unknown) => ctrl.error(e),
		get cancelled() {
			return cancelled;
		}
	};
}

const tick = () => new Promise((r) => setTimeout(r, 0));

describe('tailCap — console-buffer policy', () => {
	it('passes short text through untouched', () => {
		expect(tailCap('abc', 10)).toBe('abc');
		expect(tailCap('abc', 3)).toBe('abc');
	});

	it('keeps only the trailing cap characters of a long tail', () => {
		expect(tailCap('0123456789', 4)).toBe('6789');
	});
});

describe('playTextStream — live UTF-8 text decode', () => {
	it('decodes chunks in order and reports progress', async () => {
		const src = controllableSource();
		const onText = vi.fn();
		const onStatus = vi.fn();
		const onProgress = vi.fn();
		playTextStream({ stream: src.stream, onText, onStatus, onProgress });
		await tick();
		expect(onStatus).toHaveBeenCalledWith('streaming');

		src.push('hello ');
		src.push('world');
		await tick();
		expect(onText.mock.calls.map((c) => c[0]).join('')).toBe('hello world');
		// (chars, bytes) — both monotonic.
		expect(onProgress).toHaveBeenLastCalledWith(11, 11);

		src.end();
		await tick();
		expect(onStatus).toHaveBeenLastCalledWith('ended');
	});

	it('carries a multi-byte code point split across chunk boundaries', async () => {
		// '€' is 3 bytes in UTF-8 (E2 82 AC); the tap does not preserve write
		// boundaries, so the split MUST NOT emit replacement characters.
		const src = controllableSource();
		const onText = vi.fn();
		playTextStream({ stream: src.stream, onText });
		await tick();

		const euro = enc.encode('€');
		src.pushBytes(euro.slice(0, 1));
		await tick();
		src.pushBytes(euro.slice(1));
		await tick();

		expect(onText.mock.calls.map((c) => c[0]).join('')).toBe('€');
	});

	it('stop() cancels the source read and reports stopped', async () => {
		const src = controllableSource();
		const onStatus = vi.fn();
		const handle = playTextStream({ stream: src.stream, onText: () => {}, onStatus });
		await tick();

		handle.stop();
		await tick();
		expect(onStatus).toHaveBeenLastCalledWith('stopped');
		expect(src.cancelled).toBe(true);
		// Idempotent.
		handle.stop();
	});

	it('surfaces a source error as status error, not a throw', async () => {
		const src = controllableSource();
		const onStatus = vi.fn();
		playTextStream({ stream: src.stream, onText: () => {}, onStatus });
		await tick();

		src.fail(new Error('tap broke'));
		await tick();
		expect(onStatus).toHaveBeenLastCalledWith('error', 'tap broke');
	});
});
