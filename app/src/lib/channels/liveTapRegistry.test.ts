import { describe, it, expect, afterEach, vi } from 'vitest';
import { subscribe, _entryCount, _reset, MSE_QUEUE_LIMIT_FOR_TEST } from './liveTapRegistry';

afterEach(() => _reset());

/** A source the test drives explicitly: push() a chunk, end() to close. */
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
		push: (chunk: Uint8Array) => ctrl.enqueue(chunk),
		end: () => ctrl.close(),
		get cancelled() {
			return cancelled;
		}
	};
}

/** A fake fetch returning `body` with an optional content-type. */
function fakeFetch(body: ReadableStream<Uint8Array>, contentType: string | null = 'audio/L16;rate=16000') {
	const fn = vi.fn(async (_url: string) => {
		return new Response(body, {
			status: 200,
			headers: contentType ? { 'content-type': contentType } : {}
		});
	});
	return fn;
}

/** Read all chunks until the stream closes (mse keeps order, no drops). */
async function readAll(stream: ReadableStream<Uint8Array>): Promise<Uint8Array[]> {
	const reader = stream.getReader();
	const out: Uint8Array[] = [];
	for (;;) {
		const { done, value } = await reader.read();
		if (done) break;
		if (value) out.push(value);
	}
	return out;
}

const tick = () => new Promise((r) => setTimeout(r, 0));

describe('liveTapRegistry — fan-out', () => {
	it('delivers every source chunk to N sinks (mse, order preserved)', async () => {
		const src = controllableSource();
		const fetchImpl = fakeFetch(src.stream);

		const a = subscribe('exec1', 'chan', 'mse', fetchImpl);
		const b = subscribe('exec1', 'chan', 'mse', fetchImpl);

		// Only ONE source fetch despite two subscribers.
		expect(fetchImpl).toHaveBeenCalledTimes(1);
		expect(_entryCount()).toBe(1);

		await tick(); // let the async tap open + startSource run

		src.push(new Uint8Array([1]));
		src.push(new Uint8Array([2]));
		src.push(new Uint8Array([3]));
		src.end();

		const [ra, rb] = await Promise.all([readAll(a.stream), readAll(b.stream)]);
		expect(ra.map((c) => c[0])).toEqual([1, 2, 3]);
		expect(rb.map((c) => c[0])).toEqual([1, 2, 3]);

		a.release();
		b.release();
	});
});

describe('liveTapRegistry — mse backpressure', () => {
	it('marks a slow mse sink degraded without stalling a fast sink', async () => {
		const src = controllableSource();
		const fetchImpl = fakeFetch(src.stream);

		const fast = subscribe('exec1', 'chan', 'mse', fetchImpl);
		const slow = subscribe('exec1', 'chan', 'mse', fetchImpl);

		await tick();

		// The fast sink drains as it goes.
		const fastReader = fast.stream.getReader();
		const fastSeen: number[] = [];
		const fastPump = (async () => {
			for (;;) {
				const { done, value } = await fastReader.read();
				if (done) break;
				if (value) fastSeen.push(value[0]);
			}
		})();

		// The slow sink NEVER reads — push more than the bounded queue limit.
		const total = MSE_QUEUE_LIMIT_FOR_TEST + 10;
		for (let i = 0; i < total; i++) {
			src.push(new Uint8Array([i & 0xff]));
			// Yield so the fast sink's pull can drain between pushes.
			if (i % 8 === 0) await tick();
		}
		await tick();
		src.end();
		await fastPump;

		// Fast sink saw every chunk (no stall from the slow one).
		expect(fastSeen.length).toBe(total);

		// Slow sink errored (overflow) — reading it rejects.
		const slowReader = slow.stream.getReader();
		await expect(slowReader.read()).rejects.toThrow(/overflow|gap/i);

		fast.release();
		slow.release();
	});
});

describe('liveTapRegistry — drop-to-latest', () => {
	it('mjpeg keeps only the newest pending chunk for a slow sink', async () => {
		const src = controllableSource();
		const fetchImpl = fakeFetch(src.stream, 'image/jpeg');

		const sink = subscribe('exec1', 'chan', 'mjpeg', fetchImpl);
		const reader = sink.stream.getReader();
		await tick();

		// Push a burst with NO read in between. The stream's one eager start-pull
		// takes the first chunk straight through; the rest land with no pending
		// pull, so drop-to-latest keeps only the newest.
		src.push(new Uint8Array([10]));
		src.push(new Uint8Array([20]));
		src.push(new Uint8Array([30]));
		src.push(new Uint8Array([40]));
		await tick();

		// First read drains the straight-through chunk (10).
		expect((await reader.read()).value?.[0]).toBe(10);
		// Second read returns the latest retained (40); 20 and 30 were dropped.
		expect((await reader.read()).value?.[0]).toBe(40);

		reader.releaseLock();
		sink.release();
	});

	it('pcm keeps only the newest pending chunk for a slow sink', async () => {
		const src = controllableSource();
		const fetchImpl = fakeFetch(src.stream, 'audio/L16;rate=16000');

		const sink = subscribe('exec1', 'chan', 'pcm', fetchImpl);
		const reader = sink.stream.getReader();
		await tick();

		// Burst with no read between: one straight-through (eager pull), rest
		// collapse to the latest.
		src.push(new Uint8Array([1]));
		src.push(new Uint8Array([2]));
		src.push(new Uint8Array([3]));
		src.push(new Uint8Array([99]));
		await tick();

		expect((await reader.read()).value?.[0]).toBe(1);
		expect((await reader.read()).value?.[0]).toBe(99);

		reader.releaseLock();
		sink.release();
	});
});

describe('liveTapRegistry — ref-count', () => {
	it('two subscribers share one source fetch; release-all cancels the source', async () => {
		const src = controllableSource();
		const fetchImpl = fakeFetch(src.stream);

		const a = subscribe('execX', 'c', 'pcm', fetchImpl);
		const b = subscribe('execX', 'c', 'pcm', fetchImpl);
		await tick();

		expect(fetchImpl).toHaveBeenCalledTimes(1);
		expect(_entryCount()).toBe(1);
		expect(src.cancelled).toBe(false);

		// Releasing one keeps the source alive.
		a.release();
		expect(_entryCount()).toBe(1);
		expect(src.cancelled).toBe(false);

		// Releasing the last cancels the source + drops the entry.
		b.release();
		await tick();
		expect(_entryCount()).toBe(0);
		expect(src.cancelled).toBe(true);
	});

	it('release() is idempotent', async () => {
		const src = controllableSource();
		const a = subscribe('execY', 'c', 'pcm', fakeFetch(src.stream));
		const b = subscribe('execY', 'c', 'pcm', fakeFetch(src.stream));
		await tick();
		a.release();
		a.release(); // no-op — must NOT drop b's ref
		expect(_entryCount()).toBe(1);
		b.release();
		await tick();
		expect(_entryCount()).toBe(0);
	});

	it('exposes the resolved content-type from the source response', async () => {
		const src = controllableSource();
		const sub = subscribe('execZ', 'c', 'pcm', fakeFetch(src.stream, 'audio/L16;rate=44100'));
		await expect(sub.contentType).resolves.toBe('audio/L16;rate=44100');
		sub.release();
	});
});
