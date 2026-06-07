import { describe, it, expect, vi } from 'vitest';
import { playUrdfStream, parseJointFrame, type UrdfJointFrame } from './urdfStreamPlayer';

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

const frame = (i: number): UrdfJointFrame => ({
	joint_names: ['j1', 'j2'],
	positions: [i, i + 0.5]
});
const line = (i: number) => JSON.stringify(frame(i)) + '\n';

describe('parseJointFrame — validation', () => {
	it('accepts well-formed parallel string/number arrays', () => {
		expect(parseJointFrame({ joint_names: ['a'], positions: [1.5] })).toEqual({
			joint_names: ['a'],
			positions: [1.5]
		});
	});

	it('rejects non-objects, missing/non-array fields', () => {
		expect(parseJointFrame(null)).toBeNull();
		expect(parseJointFrame(42)).toBeNull();
		expect(parseJointFrame({ joint_names: 'a', positions: [1] })).toBeNull();
		expect(parseJointFrame({ joint_names: ['a'] })).toBeNull();
	});

	it('rejects non-string names and non-finite/non-number positions', () => {
		expect(parseJointFrame({ joint_names: [1], positions: [1] })).toBeNull();
		expect(parseJointFrame({ joint_names: ['a'], positions: ['x'] })).toBeNull();
		expect(parseJointFrame({ joint_names: ['a'], positions: [NaN] })).toBeNull();
		expect(parseJointFrame({ joint_names: ['a'], positions: [Infinity] })).toBeNull();
	});
});

describe('playUrdfStream — NDJSON reframing', () => {
	it('emits one frame per complete line when lines arrive whole', async () => {
		const src = controllableSource();
		const frames: UrdfJointFrame[] = [];
		const onStatus = vi.fn();
		const h = playUrdfStream({ stream: src.stream, onFrame: (f) => frames.push(f), onStatus });

		await tick();
		expect(onStatus).toHaveBeenCalledWith('streaming');

		src.push(line(1));
		await tick();
		src.push(line(2));
		await tick();
		src.end();
		await tick();

		expect(frames).toEqual([frame(1), frame(2)]);
		expect(onStatus).toHaveBeenLastCalledWith('ended');
		h.stop();
	});

	it('carries a JSON object SPLIT across chunk boundaries until the line completes', async () => {
		const src = controllableSource();
		const frames: UrdfJointFrame[] = [];
		const h = playUrdfStream({ stream: src.stream, onFrame: (f) => frames.push(f) });
		await tick();

		const whole = line(7); // {"joint_names":...,"positions":...}\n
		const cut = Math.floor(whole.length / 2);

		// First half of the object (no newline yet) => NOTHING emitted.
		src.push(whole.slice(0, cut));
		await tick();
		expect(frames).toEqual([]);

		// Second half completes the line (and its newline) => the frame emits.
		src.push(whole.slice(cut));
		await tick();
		expect(frames).toEqual([frame(7)]);

		src.end();
		h.stop();
	});

	it('a trailing PARTIAL line does not emit until its newline arrives in a later chunk', async () => {
		const src = controllableSource();
		const frames: UrdfJointFrame[] = [];
		const h = playUrdfStream({ stream: src.stream, onFrame: (f) => frames.push(f) });
		await tick();

		// One complete line + the START of a second (no terminating newline).
		const partial = JSON.stringify(frame(2)); // no '\n'
		src.push(line(1) + partial);
		await tick();
		// Only the completed line 1 emits; the partial line 2 is buffered.
		expect(frames).toEqual([frame(1)]);

		// The newline (and a third line) arrive next.
		src.push('\n' + line(3));
		await tick();
		// Now line 2 + line 3 both complete in this chunk => drop-to-latest = line 3.
		expect(frames).toEqual([frame(1), frame(3)]);

		src.end();
		h.stop();
	});

	it('drop-to-latest: multiple complete lines concatenated in ONE chunk emit only the newest', async () => {
		const src = controllableSource();
		const frames: UrdfJointFrame[] = [];
		const h = playUrdfStream({ stream: src.stream, onFrame: (f) => frames.push(f) });
		await tick();

		// Three full frames concatenated in a single chunk.
		src.push(line(10) + line(11) + line(12));
		await tick();
		expect(frames).toEqual([frame(12)]);

		src.end();
		h.stop();
	});

	it('ignores blank lines and skips malformed JSON, surfacing the newest VALID frame', async () => {
		const src = controllableSource();
		const frames: UrdfJointFrame[] = [];
		const h = playUrdfStream({ stream: src.stream, onFrame: (f) => frames.push(f) });
		await tick();

		// blank line, a valid frame, then a malformed trailing line => newest VALID wins.
		src.push('\n' + line(5) + 'not json\n');
		await tick();
		expect(frames).toEqual([frame(5)]);

		src.end();
		h.stop();
	});

	it('flushes a final newline-less line on clean close', async () => {
		const src = controllableSource();
		const frames: UrdfJointFrame[] = [];
		const h = playUrdfStream({ stream: src.stream, onFrame: (f) => frames.push(f) });
		await tick();

		src.push(JSON.stringify(frame(9))); // no trailing newline
		await tick();
		expect(frames).toEqual([]); // not yet — no newline

		src.end();
		await tick();
		expect(frames).toEqual([frame(9)]); // flushed on close
		h.stop();
	});

	it('holds a multi-byte UTF-8 codepoint split across chunks (TextDecoder stream:true)', async () => {
		const src = controllableSource();
		const frames: UrdfJointFrame[] = [];
		const h = playUrdfStream({ stream: src.stream, onFrame: (f) => frames.push(f) });
		await tick();

		// A joint name with a 2-byte UTF-8 char (é = 0xC3 0xA9), split mid-codepoint.
		const obj = JSON.stringify({ joint_names: ['é'], positions: [1] }) + '\n';
		const bytes = enc.encode(obj);
		// Find a split point that lands inside the é byte pair.
		const idx = bytes.indexOf(0xc3);
		src.pushBytes(bytes.slice(0, idx + 1)); // up to and including the lead byte
		await tick();
		expect(frames).toEqual([]);
		src.pushBytes(bytes.slice(idx + 1));
		await tick();
		expect(frames).toEqual([{ joint_names: ['é'], positions: [1] }]);

		src.end();
		h.stop();
	});

	it('stop() cancels the source reader', async () => {
		const src = controllableSource();
		const h = playUrdfStream({ stream: src.stream, onFrame: () => {} });
		await tick();
		h.stop();
		await tick();
		expect(src.cancelled).toBe(true);
	});
});
