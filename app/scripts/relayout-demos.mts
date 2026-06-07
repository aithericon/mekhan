/**
 * Re-lay-out every demo graph with the dimension-aware editor layout.
 *
 * The demos' node positions were originally produced by the CLI `auto_layout`
 * (fixed 150px/250px spacing, blind to how tall/wide a card actually renders),
 * so tall nodes (channels, agents, many output fields) overlap their
 * neighbours. This rewrites each `demos/<name>/graph.json` with
 * `layoutWorkflowGraph` — the same code the editor's "Auto-arrange" button
 * uses — reserving every node at its real footprint.
 *
 * Edits are SURGICAL: only each node's `position` (and a container's
 * `width`/`height`) value is rewritten; every other byte is left untouched, so
 * the hand-curated JSON formatting — inline objects, primitive arrays, `0.0`
 * floats — survives and the diff shows only the numbers that moved. A leaf
 * node's stored `width`/`height` is deleted (the only ones are `agent` nodes):
 * a leaf's width is now a function of its type (the card renders a fixed inline
 * width), so a stale stored width would make the xyflow wrapper disagree with
 * the card and mis-place the output handle.
 *
 * Run from the `app/` directory. The layout modules use extensionless
 * relative imports (Vite/TS idiom), which plain `node --strip-types` can't
 * resolve, so bundle first with the repo's own esbuild, then run. Keep
 * `@dagrejs/dagre` external so it loads from `app/node_modules`; the type-only
 * `$lib` imports are erased by the bundle:
 *
 *   ESB=$(ls -d node_modules/.pnpm/esbuild@[0-9]*\/node_modules/esbuild/bin/esbuild)
 *   "$ESB" scripts/relayout-demos.mts --bundle --platform=node --format=esm \
 *     --external:@dagrejs/dagre --outfile=scripts/.relayout.gen.mjs --log-level=error
 *   node scripts/.relayout.gen.mjs && rm scripts/.relayout.gen.mjs
 */

import { readFileSync, writeFileSync, readdirSync, existsSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { layoutWorkflowGraph, type LayoutNode, type LayoutEdge } from '../src/lib/editor/workflow-layout.ts';
import { isContainerKind, getWorkflowNodeDimensions } from '../src/lib/editor/node-dimensions.ts';

const DEMOS_DIR = resolve(process.cwd(), '..', 'demos');

interface RawNode {
	id: string;
	type: string;
	position?: { x: number; y: number };
	data: unknown;
	parentId?: string;
	width?: number;
	height?: number;
}
interface RawEdge {
	id: string;
	source: string;
	target: string;
	sourceHandle?: string;
	targetHandle?: string;
	type?: string;
}
interface RawGraph {
	nodes: RawNode[];
	edges: RawEdge[];
}

// ── Minimal, format-preserving JSON span scanner ────────────────────────────

function scanString(t: string, i: number): number {
	i++; // opening quote
	while (i < t.length) {
		if (t[i] === '\\') {
			i += 2;
			continue;
		}
		if (t[i] === '"') return i + 1;
		i++;
	}
	return i;
}

/** Index just past the bracket pair opened at `i` (`{` or `[`). */
function matchBracket(t: string, i: number): number {
	const open = t[i];
	const close = open === '{' ? '}' : ']';
	let depth = 0;
	for (let j = i; j < t.length; j++) {
		const c = t[j];
		if (c === '"') {
			j = scanString(t, j) - 1;
			continue;
		}
		if (c === open) depth++;
		else if (c === close) {
			depth--;
			if (depth === 0) return j + 1;
		}
	}
	return t.length;
}

const skipWs = (t: string, i: number): number => {
	while (i < t.length && /\s/.test(t[i])) i++;
	return i;
};

/** End index (exclusive) of the JSON value starting at `i`. */
function scanValue(t: string, i: number): number {
	const c = t[i];
	if (c === '"') return scanString(t, i);
	if (c === '{' || c === '[') return matchBracket(t, i);
	let j = i;
	while (j < t.length && !/[,}\]\s]/.test(t[j])) j++;
	return j;
}

interface KeySpan {
	name: string;
	keyStart: number;
	valStart: number;
	valEnd: number;
}

/** Top-level keys of the object whose `{` is at `objStart`. */
function topLevelKeys(t: string, objStart: number): KeySpan[] {
	const objEnd = matchBracket(t, objStart) - 1; // index of `}`
	const keys: KeySpan[] = [];
	let i = objStart + 1;
	while (true) {
		i = skipWs(t, i);
		if (i >= objEnd || t[i] === '}') break;
		if (t[i] === ',') {
			i++;
			continue;
		}
		if (t[i] !== '"') break; // malformed — bail
		const keyStart = i;
		const keyEnd = scanString(t, i);
		const name = JSON.parse(t.slice(keyStart, keyEnd)) as string;
		let j = skipWs(t, keyEnd);
		j++; // skip ':'
		j = skipWs(t, j);
		const valStart = j;
		const valEnd = scanValue(t, j);
		keys.push({ name, keyStart, valStart, valEnd });
		i = valEnd;
	}
	return keys;
}

/** Top-level element object spans `[start, end)` of the array opened at `[`. */
function arrayElementSpans(t: string, arrOpen: number): Array<[number, number]> {
	const arrEnd = matchBracket(t, arrOpen) - 1; // index of `]`
	const spans: Array<[number, number]> = [];
	let i = arrOpen + 1;
	while (true) {
		i = skipWs(t, i);
		if (i >= arrEnd) break;
		if (t[i] === ',') {
			i++;
			continue;
		}
		const end = scanValue(t, i);
		spans.push([i, end]);
		i = end;
	}
	return spans;
}

interface Edit {
	start: number;
	end: number;
	text: string;
}

/** A whole-line deletion span for `"key": value,` (key isn't object-final). */
function lineDeletion(t: string, key: KeySpan): Edit {
	const lineStart = t.lastIndexOf('\n', key.keyStart) + 1;
	let e = key.valEnd;
	if (t[e] === ',') e++;
	while (e < t.length && t[e] !== '\n') e++;
	if (e < t.length) e++; // consume the newline
	return { start: lineStart, end: e, text: '' };
}

let total = 0;
let changed = 0;
const flagged: string[] = [];

for (const name of readdirSync(DEMOS_DIR).sort()) {
	const file = join(DEMOS_DIR, name, 'graph.json');
	if (!existsSync(file)) continue;
	total += 1;

	const before = readFileSync(file, 'utf8');
	const graph: RawGraph = JSON.parse(before);
	if (!Array.isArray(graph.nodes) || graph.nodes.length === 0) continue;

	const layoutNodes: LayoutNode[] = graph.nodes.map((n) => ({
		id: n.id,
		type: n.type,
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		data: n.data as any,
		parentId: n.parentId,
		width: n.width,
		height: n.height
	}));
	const layoutEdges: LayoutEdge[] = graph.edges.map((e) => ({
		source: e.source,
		target: e.target,
		sourceHandle: e.sourceHandle,
		targetHandle: e.targetHandle,
		type: e.type
	}));

	const { positions, containerSizes } = layoutWorkflowGraph(layoutNodes, layoutEdges);

	// Locate the `nodes` array and its element objects (array order matches the
	// parsed `graph.nodes` order).
	const nodesKey = before.indexOf('"nodes"');
	const arrOpen = before.indexOf('[', nodesKey);
	const elemSpans = arrayElementSpans(before, arrOpen);

	const edits: Edit[] = [];
	for (let k = 0; k < graph.nodes.length; k++) {
		const node = graph.nodes[k];
		const [objStart] = elemSpans[k];
		const keys = topLevelKeys(before, objStart);
		const keyOf = (n: string) => keys.find((key) => key.name === n);

		const pos = positions.get(node.id);
		const posKey = keyOf('position');
		if (pos && posKey) {
			edits.push({
				start: posKey.valStart,
				end: posKey.valEnd,
				text: `{ "x": ${pos.x}, "y": ${pos.y} }`
			});
		}

		if (isContainerKind(node.type)) {
			const size = containerSizes.get(node.id);
			if (size) {
				const w = keyOf('width');
				const h = keyOf('height');
				if (w) edits.push({ start: w.valStart, end: w.valEnd, text: String(Math.round(size.width)) });
				if (h) edits.push({ start: h.valStart, end: h.valEnd, text: String(Math.round(size.height)) });
			}
		} else {
			// Leaf width/height are derived at render time — never persist them.
			const w = keyOf('width');
			const h = keyOf('height');
			if (w) edits.push(lineDeletion(before, w));
			if (h) edits.push(lineDeletion(before, h));
		}
	}

	// Apply right-to-left so earlier offsets stay valid.
	edits.sort((a, b) => b.start - a.start);
	let after = before;
	for (const e of edits) after = after.slice(0, e.start) + e.text + after.slice(e.end);

	// Residual overlap check between top-level siblings (debug reporting).
	const tops = graph.nodes.filter((n) => !n.parentId);
	const boxes = tops.map((n) => {
		const size = containerSizes.get(n.id);
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		const dims = size ?? getWorkflowNodeDimensions({ type: n.type, data: n.data as any });
		const p = positions.get(n.id) ?? n.position ?? { x: 0, y: 0 };
		return { id: n.id, x: p.x, y: p.y, w: dims.width, h: dims.height };
	});
	for (let i = 0; i < boxes.length; i++) {
		for (let j = i + 1; j < boxes.length; j++) {
			const a = boxes[i];
			const b = boxes[j];
			if (a.x < b.x + b.w && a.x + a.w > b.x && a.y < b.y + b.h && a.y + a.h > b.y) {
				flagged.push(`${name}: ${a.id} ∩ ${b.id}`);
			}
		}
	}

	if (after !== before) {
		writeFileSync(file, after);
		changed += 1;
		console.log(`relaid  ${name}`);
	}
}

console.log(`\n${changed}/${total} demo graphs rewritten.`);
if (flagged.length) {
	console.log(`\n⚠️  residual top-level overlaps (${flagged.length}):`);
	for (const f of flagged) console.log('   ' + f);
} else {
	console.log('No residual top-level overlaps. ✅');
}
