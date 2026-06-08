import { describe, it, expect, vi } from 'vitest';
import { playSceneStream, parseSceneFrame, type SceneFrame } from './sceneStreamPlayer';

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

const pose = (z: number) => ({
	position: { x: 0, y: 0, z },
	orientation: { x: 0, y: 0, z: 0, w: 1 }
});

const frame = (i: number): SceneFrame => ({
	joints: { names: ['j1', 'j2'], positions: [i, i + 0.5] },
	objects: [
		{ id: 'box', frame: 'link_base', origin: pose(0), primitives: [{ type: 1, dimensions: [0.04, 0.04, 0.04] }], poses: [pose(i)] }
	],
	attached: []
});
const line = (i: number) => JSON.stringify(frame(i)) + '\n';

describe('parseSceneFrame — validation', () => {
	it('accepts a well-formed planning-scene snapshot', () => {
		expect(parseSceneFrame(frame(3))).toEqual(frame(3));
	});

	it('rejects non-objects', () => {
		expect(parseSceneFrame(null)).toBeNull();
		expect(parseSceneFrame(42)).toBeNull();
		expect(parseSceneFrame('x')).toBeNull();
	});

	it('tolerates a missing joints block → empty parallel arrays', () => {
		expect(parseSceneFrame({})).toEqual({
			joints: { names: [], positions: [] },
			objects: [],
			attached: []
		});
	});

	it('tolerates missing objects / attached arrays → []', () => {
		const f = parseSceneFrame({ joints: { names: ['a'], positions: [1] } });
		expect(f).toEqual({
			joints: { names: ['a'], positions: [1] },
			objects: [],
			attached: []
		});
	});

	it('drops invalid joints (non-array / non-finite) to empty, keeping the frame', () => {
		const f = parseSceneFrame({ joints: { names: 'a', positions: [NaN] } });
		expect(f).toEqual({
			joints: { names: [], positions: [] },
			objects: [],
			attached: []
		});
	});

	it('drops invalid objects/primitives/poses within an otherwise valid frame', () => {
		const f = parseSceneFrame({
			objects: [
				{ id: 'good', frame: 'w', primitives: [{ type: 2, dimensions: [0.05] }], poses: [pose(1)] },
				{ id: 'badprim', primitives: [{ type: 'x', dimensions: [1] }], poses: [pose(1)] },
				'not-an-object'
			]
		});
		expect(f?.objects).toEqual([
			{ id: 'good', frame: 'w', origin: pose(0), primitives: [{ type: 2, dimensions: [0.05] }], poses: [pose(1)] },
			// badprim survives as an object but its invalid primitive is dropped.
			{ id: 'badprim', frame: '', origin: pose(0), primitives: [], poses: [pose(1)] }
		]);
	});

	it('extracts the object origin pose when present (MoveIt parks the world transform there)', () => {
		const origin = { position: { x: 0.35, y: 0.07, z: 0.12 }, orientation: { x: 0, y: 0, z: 0, w: 1 } };
		const f = parseSceneFrame({
			objects: [{ id: 's', frame: 'world', origin, primitives: [{ type: 1, dimensions: [0.04, 0.04, 0.08] }], poses: [pose(0)] }],
			attached: [{ link: 'link_tcp', id: 'g', origin, primitives: [{ type: 1, dimensions: [0.04, 0.04, 0.08] }], poses: [pose(0)] }]
		});
		expect(f?.objects[0].origin).toEqual(origin);
		expect(f?.attached[0].origin).toEqual(origin);
	});

	it('defaults a missing origin to identity', () => {
		const f = parseSceneFrame({ objects: [{ id: 's', frame: 'w', primitives: [], poses: [] }] });
		expect(f?.objects[0].origin).toEqual(pose(0));
	});

	it('parses attached objects (link + relative poses)', () => {
		const f = parseSceneFrame({
			attached: [{ link: 'link_eef', id: 'grip', primitives: [{ type: 1, dimensions: [0.04, 0.04, 0.04] }], poses: [pose(0)] }]
		});
		expect(f?.attached).toEqual([
			{ link: 'link_eef', id: 'grip', origin: pose(0), primitives: [{ type: 1, dimensions: [0.04, 0.04, 0.04] }], poses: [pose(0)] }
		]);
	});
});

describe('playSceneStream — NDJSON reframing', () => {
	it('emits one frame per complete line when lines arrive whole', async () => {
		const src = controllableSource();
		const frames: SceneFrame[] = [];
		const onStatus = vi.fn();
		const h = playSceneStream({ stream: src.stream, onFrame: (f) => frames.push(f), onStatus });

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
		const frames: SceneFrame[] = [];
		const h = playSceneStream({ stream: src.stream, onFrame: (f) => frames.push(f) });
		await tick();

		const whole = line(7);
		const cut = Math.floor(whole.length / 2);

		src.push(whole.slice(0, cut));
		await tick();
		expect(frames).toEqual([]);

		src.push(whole.slice(cut));
		await tick();
		expect(frames).toEqual([frame(7)]);

		src.end();
		h.stop();
	});

	it('a trailing PARTIAL line does not emit until its newline arrives in a later chunk', async () => {
		const src = controllableSource();
		const frames: SceneFrame[] = [];
		const h = playSceneStream({ stream: src.stream, onFrame: (f) => frames.push(f) });
		await tick();

		const partial = JSON.stringify(frame(2)); // no '\n'
		src.push(line(1) + partial);
		await tick();
		expect(frames).toEqual([frame(1)]);

		src.push('\n' + line(3));
		await tick();
		// line 2 + line 3 both complete in this chunk => drop-to-latest = line 3.
		expect(frames).toEqual([frame(1), frame(3)]);

		src.end();
		h.stop();
	});

	it('drop-to-latest: multiple complete lines concatenated in ONE chunk emit only the newest', async () => {
		const src = controllableSource();
		const frames: SceneFrame[] = [];
		const h = playSceneStream({ stream: src.stream, onFrame: (f) => frames.push(f) });
		await tick();

		src.push(line(10) + line(11) + line(12));
		await tick();
		expect(frames).toEqual([frame(12)]);

		src.end();
		h.stop();
	});

	it('ignores blank lines and skips malformed JSON, surfacing the newest VALID frame', async () => {
		const src = controllableSource();
		const frames: SceneFrame[] = [];
		const h = playSceneStream({ stream: src.stream, onFrame: (f) => frames.push(f) });
		await tick();

		src.push('\n' + line(5) + 'not json\n');
		await tick();
		expect(frames).toEqual([frame(5)]);

		src.end();
		h.stop();
	});

	it('flushes a final newline-less line on clean close', async () => {
		const src = controllableSource();
		const frames: SceneFrame[] = [];
		const h = playSceneStream({ stream: src.stream, onFrame: (f) => frames.push(f) });
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
		const frames: SceneFrame[] = [];
		const h = playSceneStream({ stream: src.stream, onFrame: (f) => frames.push(f) });
		await tick();

		// An object id with a 2-byte UTF-8 char (é = 0xC3 0xA9), split mid-codepoint.
		const obj =
			JSON.stringify({
				joints: { names: [], positions: [] },
				objects: [{ id: 'café', frame: 'w', primitives: [], poses: [] }],
				attached: []
			}) + '\n';
		const bytes = enc.encode(obj);
		const idx = bytes.indexOf(0xc3);
		src.pushBytes(bytes.slice(0, idx + 1));
		await tick();
		expect(frames).toEqual([]);
		src.pushBytes(bytes.slice(idx + 1));
		await tick();
		expect(frames).toEqual([
			{
				joints: { names: [], positions: [] },
				objects: [{ id: 'café', frame: 'w', origin: pose(0), primitives: [], poses: [] }],
				attached: []
			}
		]);

		src.end();
		h.stop();
	});

	it('stop() cancels the source reader', async () => {
		const src = controllableSource();
		const h = playSceneStream({ stream: src.stream, onFrame: () => {} });
		await tick();
		h.stop();
		await tick();
		expect(src.cancelled).toBe(true);
	});
});
