/**
 * Play a LIVE NDJSON planning-scene stream — a data channel of newline-delimited
 * JSON snapshots — by surfacing the LATEST decoded scene frame to a 3D twin as it
 * arrives. The planning-scene analog of {@link import('./urdfStreamPlayer').playUrdfStream}.
 *
 * Each `write()` is ONE full planning-scene snapshot: the arm's joint state, the
 * world collision objects, and any objects attached to (riding) a gripper link.
 * As with the joint-state player, the datastream tap
 * (`GET .../channels/{c}/data?follow=1`) serves the channel's bytes as ONE
 * concatenated HTTP-chunked stream — per-`write()` boundaries are NOT preserved —
 * so we re-frame client-side on the `\n` (NDJSON). We decode incrementally with a
 * streaming `TextDecoder` (so a multi-byte UTF-8 codepoint split across two chunks
 * is held until complete), split the accumulated text on `\n`, and JSON.parse each
 * COMPLETE line; a trailing partial line (no terminating `\n` yet) is carried
 * across chunks until its newline arrives.
 *
 * Like the joint-state path this pairs naturally with the lossy `nats-latest`
 * transport and is loss-tolerant by design: a dropped snapshot is just a skipped
 * frame, never a corrupt stream. When a single chunk carries several complete
 * lines we emit only the LATEST valid one (drop-to-latest) — the twin only ever
 * wants the current scene, not the intervening history — mirroring the registry's
 * keep-only-latest sink policy in `liveTapRegistry.ts`.
 *
 * Returns a `stop()`-able handle (mirroring `playUrdfStream`) whose `stop()`
 * cancels the source reader.
 */

/** A primitive's solid shape: SolidPrimitive `type` int + its `dimensions`. */
export interface ScenePrimitive {
	/** SolidPrimitive type int: 1=BOX, 2=SPHERE, 3=CYLINDER. */
	type: number;
	/** Shape dims: BOX [x,y,z], SPHERE [radius], CYLINDER [height,radius]. */
	dimensions: number[];
}

/** A rigid pose: position + unit quaternion (x,y,z,w). */
export interface ScenePose {
	position: { x: number; y: number; z: number };
	orientation: { x: number; y: number; z: number; w: number };
}

/** A world collision object in the planning scene (in the `frame` ROS frame). */
export interface SceneObject {
	id: string;
	frame: string;
	/** The object's origin pose; each primitive pose is composed under it (MoveIt
	 *  parks the world transform here and zeroes the primitive poses). */
	origin: ScenePose;
	primitives: ScenePrimitive[];
	poses: ScenePose[];
}

/** A collision object attached to (riding) a URDF link, posed in that link frame. */
export interface SceneAttached {
	link: string;
	id: string;
	/** The object's origin pose RELATIVE to `link` (the grasp offset); each
	 *  primitive pose is composed under it. */
	origin: ScenePose;
	primitives: ScenePrimitive[];
	poses: ScenePose[];
}

/** Identity pose — the default origin when a frame omits it. */
const IDENTITY_POSE: ScenePose = {
	position: { x: 0, y: 0, z: 0 },
	orientation: { x: 0, y: 0, z: 0, w: 1 }
};

/** A decoded planning-scene snapshot: joints + world objects + attached objects. */
export interface SceneFrame {
	joints: { names: string[]; positions: number[] };
	objects: SceneObject[];
	attached: SceneAttached[];
}

export interface SceneStreamOptions {
	/** The tap response's `ReadableStream` body (`?follow=1`). */
	stream: ReadableStream<Uint8Array>;
	/** Called with the LATEST valid scene frame as lines complete. */
	onFrame: (frame: SceneFrame) => void;
	/** Coarse lifecycle status: `streaming` | `ended` | `error` | `stopped`. */
	onStatus?: (status: string) => void;
}

/** A stoppable live-player handle (mirrors `playUrdfStream`). */
export interface SceneStreamHandle {
	stop(): void;
}

/** Validate one `{x,y,z}` triple of finite numbers, or null. */
function parseVec3(value: unknown): { x: number; y: number; z: number } | null {
	if (typeof value !== 'object' || value === null) return null;
	const { x, y, z } = value as Record<string, unknown>;
	if (![x, y, z].every((n) => typeof n === 'number' && Number.isFinite(n))) return null;
	return { x: x as number, y: y as number, z: z as number };
}

/** Validate a unit quaternion `{x,y,z,w}` of finite numbers, or null. */
function parseQuat(
	value: unknown
): { x: number; y: number; z: number; w: number } | null {
	if (typeof value !== 'object' || value === null) return null;
	const { x, y, z, w } = value as Record<string, unknown>;
	if (![x, y, z, w].every((n) => typeof n === 'number' && Number.isFinite(n))) return null;
	return { x: x as number, y: y as number, z: z as number, w: w as number };
}

function parsePose(value: unknown): ScenePose | null {
	if (typeof value !== 'object' || value === null) return null;
	const obj = value as Record<string, unknown>;
	const position = parseVec3(obj.position);
	const orientation = parseQuat(obj.orientation);
	if (!position || !orientation) return null;
	return { position, orientation };
}

function parsePrimitive(value: unknown): ScenePrimitive | null {
	if (typeof value !== 'object' || value === null) return null;
	const obj = value as Record<string, unknown>;
	const { type, dimensions } = obj;
	if (typeof type !== 'number' || !Number.isFinite(type)) return null;
	if (!Array.isArray(dimensions)) return null;
	if (!dimensions.every((d) => typeof d === 'number' && Number.isFinite(d))) return null;
	return { type, dimensions: dimensions as number[] };
}

/** Map+filter an array through a parser, dropping null (invalid) entries. */
function parseList<T>(value: unknown, parse: (v: unknown) => T | null): T[] {
	if (!Array.isArray(value)) return [];
	const out: T[] = [];
	for (const v of value) {
		const parsed = parse(v);
		if (parsed) out.push(parsed);
	}
	return out;
}

function parseObject(value: unknown): SceneObject | null {
	if (typeof value !== 'object' || value === null) return null;
	const obj = value as Record<string, unknown>;
	const id = typeof obj.id === 'string' ? obj.id : '';
	const frame = typeof obj.frame === 'string' ? obj.frame : '';
	return {
		id,
		frame,
		origin: parsePose(obj.origin) ?? IDENTITY_POSE,
		primitives: parseList(obj.primitives, parsePrimitive),
		poses: parseList(obj.poses, parsePose)
	};
}

function parseAttached(value: unknown): SceneAttached | null {
	if (typeof value !== 'object' || value === null) return null;
	const obj = value as Record<string, unknown>;
	const link = typeof obj.link === 'string' ? obj.link : '';
	const id = typeof obj.id === 'string' ? obj.id : '';
	return {
		link,
		id,
		origin: parsePose(obj.origin) ?? IDENTITY_POSE,
		primitives: parseList(obj.primitives, parsePrimitive),
		poses: parseList(obj.poses, parsePose)
	};
}

/**
 * Validate a parsed NDJSON line as a planning-scene frame. Tolerant: missing or
 * malformed `joints` collapse to empty parallel arrays, and missing `objects` /
 * `attached` arrays (or invalid entries within them) collapse to `[]`. Returns
 * the typed frame, or `null` only for a non-object line (a wholly malformed line
 * is dropped, not thrown — loss-tolerant).
 */
export function parseSceneFrame(value: unknown): SceneFrame | null {
	if (typeof value !== 'object' || value === null) return null;
	const obj = value as Record<string, unknown>;

	let names: string[] = [];
	let positions: number[] = [];
	const joints = obj.joints;
	if (joints && typeof joints === 'object') {
		const j = joints as Record<string, unknown>;
		if (Array.isArray(j.names) && j.names.every((n) => typeof n === 'string')) {
			names = j.names as string[];
		}
		if (
			Array.isArray(j.positions) &&
			j.positions.every((p) => typeof p === 'number' && Number.isFinite(p))
		) {
			positions = j.positions as number[];
		}
	}

	return {
		joints: { names, positions },
		objects: parseList(obj.objects, parseObject),
		attached: parseList(obj.attached, parseAttached)
	};
}

/**
 * Start live NDJSON planning-scene playback. Returns a handle whose `stop()`
 * cancels the read.
 */
export function playSceneStream(opts: SceneStreamOptions): SceneStreamHandle {
	const { stream, onFrame, onStatus } = opts;
	let stopped = false;
	const reader = stream.getReader();
	const decoder = new TextDecoder();

	const pump = async () => {
		onStatus?.('streaming');
		// Carries any trailing line fragment (text after the last `\n`) across chunks.
		let pending = '';
		try {
			for (;;) {
				const { done, value } = await reader.read();
				if (stopped) break;
				if (done) break;
				// `stream: true` holds a partial multi-byte UTF-8 codepoint until complete.
				if (value && value.length > 0) pending += decoder.decode(value, { stream: true });
				else continue;

				const nl = pending.lastIndexOf('\n');
				if (nl < 0) continue; // no complete line yet — keep buffering

				// Everything up to the last newline is one-or-more complete lines;
				// the remainder (possibly empty) is the next partial line.
				const complete = pending.slice(0, nl);
				pending = pending.slice(nl + 1);

				// Drop-to-latest: among the complete lines, surface only the newest
				// valid frame. Scan from the end so the first parse wins.
				let latest: SceneFrame | null = null;
				const lines = complete.split('\n');
				for (let i = lines.length - 1; i >= 0; i--) {
					const line = lines[i].trim();
					if (line === '') continue; // ignore blank lines
					try {
						const frame = parseSceneFrame(JSON.parse(line));
						if (frame) {
							latest = frame;
							break;
						}
					} catch {
						/* malformed line — drop it, keep scanning older lines */
					}
				}
				if (latest) onFrame(latest);
			}
			// Flush any final buffered line on a clean close.
			if (!stopped) {
				const line = pending.trim();
				if (line !== '') {
					try {
						const frame = parseSceneFrame(JSON.parse(line));
						if (frame) onFrame(frame);
					} catch {
						/* ignore trailing garbage */
					}
				}
				onStatus?.('ended');
			}
		} catch (e) {
			if (!stopped) onStatus?.(`error: ${e instanceof Error ? e.message : String(e)}`);
		}
	};

	void pump();

	return {
		stop() {
			if (stopped) return;
			stopped = true;
			void reader.cancel().catch(() => {});
			onStatus?.('stopped');
		}
	};
}
