import { describe, it, expect } from 'vitest';
import {
	deriveEdgeFeeds,
	edgeFeedLifecycle,
	streamSourceExecutionId,
	type ExecutionLike
} from './edge-feed-context';
import type { WorkflowGraph, WorkflowNode } from '$lib/api/client';

/**
 * A minimal marking-store stand-in: `deriveEdgeFeeds` only calls
 * `channelRuntimeFor(store, …)`, which uses `hasPlace` / `count`. We declare a
 * couple of element places as "known" so the channel reads as opened.
 */
function fakeMarking(openPlaces: Record<string, number> = {}) {
	return {
		hasPlace: (p: string) => p in openPlaces,
		count: (p: string) => openPlaces[p] ?? 0
	} as unknown as Parameters<typeof deriveEdgeFeeds>[3];
}

/** An automated_step node carrying one out data-binary channel. */
function nodeWithChannel(
	id: string,
	channel: { name: string; plane: string; element: unknown }
): WorkflowNode {
	return {
		id,
		type: 'automated_step',
		position: { x: 0, y: 0 },
		data: {
			type: 'automated_step',
			channels: [channel]
		}
	} as unknown as WorkflowNode;
}

/** A stream_source node carrying one out data-binary channel. */
function streamSourceNode(
	id: string,
	channel: { name: string; plane: string; element: unknown }
): WorkflowNode {
	return {
		id,
		type: 'stream_source',
		position: { x: 0, y: 0 },
		data: {
			type: 'stream_source',
			channels: [channel]
		}
	} as unknown as WorkflowNode;
}

function nodesById(...nodes: WorkflowNode[]): Map<string, WorkflowNode> {
	const m = new Map<string, WorkflowNode>();
	for (const n of nodes) m.set(n.id, n);
	return m;
}

function graph(edges: WorkflowGraph['edges']): WorkflowGraph {
	return { nodes: [], edges } as unknown as WorkflowGraph;
}

const VIDEO_CT = 'video/mp4;codecs="avc1.42E01E"';
const JPEG_CT = 'image/jpeg';

const execs = (id: string): Map<string, ExecutionLike[]> =>
	new Map([['src', [{ execution_id: id }]]]);

describe('deriveEdgeFeeds', () => {
	it('yields a feed for a data-binary VIDEO channel edge with an execution', () => {
		const src = nodeWithChannel('src', {
			name: 'frames',
			plane: 'data',
			element: { type: 'binary', content_type: VIDEO_CT }
		});
		const g = graph([
			{ id: 'e1', source: 'src', target: 'sink', sourceHandle: 'frames', type: 'default' }
		]);
		const map = deriveEdgeFeeds(
			g,
			nodesById(src),
			execs('mekhan-net-abc'),
			fakeMarking({ p_src_frames: 1 }),
			false,
			null,
			() => true // jsdom has no MediaSource; assert MSE support for the test.
		);
		const feed = map.get('e1');
		expect(feed).toBeDefined();
		expect(feed?.executionId).toBe('mekhan-net-abc');
		expect(feed?.channelName).toBe('frames');
		expect(feed?.contentType).toBe(VIDEO_CT);
		expect(feed?.plan?.kind).toBe('mse');
		expect(feed?.plan?.mediaKind).toBe('video');
		expect(feed?.runtime.opened).toBe(true);
		expect(feed?.terminal).toBe(false);
	});

	it('yields an MJPEG feed for an image/jpeg data channel', () => {
		const src = nodeWithChannel('src', {
			name: 'cam',
			plane: 'data',
			element: { type: 'binary', content_type: JPEG_CT }
		});
		const g = graph([
			{ id: 'e1', source: 'src', target: 'sink', sourceHandle: 'cam', type: 'default' }
		]);
		const map = deriveEdgeFeeds(g, nodesById(src), execs('x'), fakeMarking());
		expect(map.get('e1')?.plan?.kind).toBe('mjpeg');
	});

	it('yields NO feed for a control-plane channel', () => {
		const src = nodeWithChannel('src', {
			name: 'detections',
			plane: 'control',
			element: { type: 'binary', content_type: JPEG_CT }
		});
		const g = graph([
			{ id: 'e1', source: 'src', target: 'sink', sourceHandle: 'detections', type: 'default' }
		]);
		const map = deriveEdgeFeeds(g, nodesById(src), execs('x'), fakeMarking());
		expect(map.has('e1')).toBe(false);
	});

	it('yields NO feed for a JSON-element data channel (no content_type)', () => {
		const src = nodeWithChannel('src', {
			name: 'rows',
			plane: 'data',
			element: { type: 'json', schema: {} }
		});
		const g = graph([
			{ id: 'e1', source: 'src', target: 'sink', sourceHandle: 'rows', type: 'default' }
		]);
		const map = deriveEdgeFeeds(g, nodesById(src), execs('x'), fakeMarking());
		expect(map.has('e1')).toBe(false);
	});

	it('yields a binary-only feed (plan null) for a non-renderable content_type', () => {
		// Data-plane binary but no live renderer → the edge still gets a feed so it
		// can show a minimal liveness dot; `plan` is null (no decode).
		const src = nodeWithChannel('src', {
			name: 'blob',
			plane: 'data',
			element: { type: 'binary', content_type: 'application/octet-stream' }
		});
		const g = graph([
			{ id: 'e1', source: 'src', target: 'sink', sourceHandle: 'blob', type: 'default' }
		]);
		const map = deriveEdgeFeeds(g, nodesById(src), execs('x'), fakeMarking());
		const feed = map.get('e1');
		expect(feed).toBeDefined();
		expect(feed?.plan).toBeNull();
		expect(feed?.contentType).toBe('application/octet-stream');
	});

	it('yields NO feed when the source node has no execution yet', () => {
		const src = nodeWithChannel('src', {
			name: 'frames',
			plane: 'data',
			element: { type: 'binary', content_type: VIDEO_CT }
		});
		const g = graph([
			{ id: 'e1', source: 'src', target: 'sink', sourceHandle: 'frames', type: 'default' }
		]);
		// No executions for 'src'.
		const map = deriveEdgeFeeds(g, nodesById(src), new Map(), fakeMarking());
		expect(map.has('e1')).toBe(false);
	});

	it('uses the LATEST execution_id (last row) for the tap target', () => {
		const src = nodeWithChannel('src', {
			name: 'frames',
			plane: 'data',
			element: { type: 'binary', content_type: VIDEO_CT }
		});
		const g = graph([
			{ id: 'e1', source: 'src', target: 'sink', sourceHandle: 'frames', type: 'default' }
		]);
		const rows: Map<string, ExecutionLike[]> = new Map([
			['src', [{ execution_id: 'first' }, { execution_id: 'latest' }]]
		]);
		const map = deriveEdgeFeeds(g, nodesById(src), rows, fakeMarking(), false, null, () => true);
		expect(map.get('e1')?.executionId).toBe('latest');
	});

	it('yields NO feed for an edge whose sourceHandle is not a channel', () => {
		const src = nodeWithChannel('src', {
			name: 'frames',
			plane: 'data',
			element: { type: 'binary', content_type: VIDEO_CT }
		});
		const g = graph([
			// sourceHandle is the standard out handle, not the channel name.
			{ id: 'e1', source: 'src', target: 'sink', sourceHandle: 'out', type: 'default' }
		]);
		const map = deriveEdgeFeeds(g, nodesById(src), execs('x'), fakeMarking());
		expect(map.has('e1')).toBe(false);
	});

	it('returns an empty map for a null graph', () => {
		expect(deriveEdgeFeeds(null, new Map(), new Map(), fakeMarking()).size).toBe(0);
	});

	it('stamps `terminal` onto every feed (drives the end-state freeze)', () => {
		const src = nodeWithChannel('src', {
			name: 'frames',
			plane: 'data',
			element: { type: 'binary', content_type: JPEG_CT }
		});
		const g = graph([
			{ id: 'e1', source: 'src', target: 'sink', sourceHandle: 'frames', type: 'default' }
		]);
		const live = deriveEdgeFeeds(g, nodesById(src), execs('x'), fakeMarking(), false);
		expect(live.get('e1')?.terminal).toBe(false);
		const done = deriveEdgeFeeds(g, nodesById(src), execs('x'), fakeMarking(), true);
		expect(done.get('e1')?.terminal).toBe(true);
	});
});

describe('deriveEdgeFeeds — stream_source producers', () => {
	const srcChannel = {
		name: 'frames',
		plane: 'data',
		element: { type: 'binary', content_type: JPEG_CT }
	};

	it('derives the execution id deterministically (st-<instance>-<node>)', () => {
		const src = streamSourceNode('ingress', srcChannel);
		const g = graph([
			{ id: 'e1', source: 'ingress', target: 'sink', sourceHandle: 'frames', type: 'default' }
		]);
		// NO executions for 'ingress' — a stream source never gets a step row.
		const map = deriveEdgeFeeds(g, nodesById(src), new Map(), fakeMarking(), false, 'inst-42');
		const feed = map.get('e1');
		expect(feed).toBeDefined();
		expect(feed?.executionId).toBe('st-inst-42-ingress');
		expect(feed?.executionId).toBe(streamSourceExecutionId('inst-42', 'ingress'));
		expect(feed?.plan?.kind).toBe('mjpeg');
	});

	it('synthesizes producerStatus running while the instance is live', () => {
		const src = streamSourceNode('ingress', srcChannel);
		const g = graph([
			{ id: 'e1', source: 'ingress', target: 'sink', sourceHandle: 'frames', type: 'default' }
		]);
		const live = deriveEdgeFeeds(g, nodesById(src), new Map(), fakeMarking(), false, 'inst-42');
		expect(live.get('e1')?.producerStatus).toBe('running');
		// → the widget classifies it LIVE without any step-execution row.
		expect(edgeFeedLifecycle({ closed: false }, false, live.get('e1')!.producerStatus)).toBe(
			'live'
		);
		const done = deriveEdgeFeeds(g, nodesById(src), new Map(), fakeMarking(), true, 'inst-42');
		expect(done.get('e1')?.producerStatus).toBe('completed');
	});

	it('yields NO feed when the instance id is unavailable', () => {
		const src = streamSourceNode('ingress', srcChannel);
		const g = graph([
			{ id: 'e1', source: 'ingress', target: 'sink', sourceHandle: 'frames', type: 'default' }
		]);
		const map = deriveEdgeFeeds(g, nodesById(src), new Map(), fakeMarking(), false, null);
		expect(map.has('e1')).toBe(false);
	});

	it('still requires a data-plane binary channel (json source yields no feed)', () => {
		const src = streamSourceNode('ingress', {
			name: 'rows',
			plane: 'data',
			element: { type: 'json', schema: {} }
		});
		const g = graph([
			{ id: 'e1', source: 'ingress', target: 'sink', sourceHandle: 'rows', type: 'default' }
		]);
		const map = deriveEdgeFeeds(g, nodesById(src), new Map(), fakeMarking(), false, 'inst-42');
		expect(map.has('e1')).toBe(false);
	});
});

describe('edgeFeedLifecycle — producer-driven liveness + end-state', () => {
	it('is LIVE while the producer step is running (NOT runtime.opened)', () => {
		// The whole point of the fix: a data channel never shows element tokens in
		// the marking, so liveness must come from the producer step status.
		expect(edgeFeedLifecycle({ closed: false }, false, 'running')).toBe('live');
	});

	it('is IDLE before the producer starts streaming (e.g. still pending)', () => {
		expect(edgeFeedLifecycle({ closed: false }, false, 'pending')).toBe('idle');
		// No producer status known yet → idle, not live.
		expect(edgeFeedLifecycle({ closed: false }, false)).toBe('idle');
	});

	it('ends when the producing step reaches a terminal status', () => {
		expect(edgeFeedLifecycle({ closed: false }, false, 'completed')).toBe('ended');
		expect(edgeFeedLifecycle({ closed: false }, false, 'failed')).toBe('ended');
		expect(edgeFeedLifecycle({ closed: false }, false, 'cancelled')).toBe('ended');
	});

	it('ends when the channel close token lands (even while still running)', () => {
		expect(edgeFeedLifecycle({ closed: true }, false, 'running')).toBe('ended');
	});

	it('ends when the instance is terminal even if the step still reads running', () => {
		expect(edgeFeedLifecycle({ closed: false }, true, 'running')).toBe('ended');
	});
});

describe('deriveEdgeFeeds — producer status', () => {
	it('stamps the latest execution status as the liveness signal', () => {
		const src = nodeWithChannel('src', {
			name: 'frames',
			plane: 'data',
			element: { type: 'binary', content_type: JPEG_CT }
		});
		const g = graph([
			{ id: 'e1', source: 'src', target: 'sink', sourceHandle: 'frames', type: 'default' }
		]);
		const rows: Map<string, ExecutionLike[]> = new Map([
			['src', [{ execution_id: 'x', status: 'running' }]]
		]);
		expect(deriveEdgeFeeds(g, nodesById(src), rows, fakeMarking()).get('e1')?.producerStatus).toBe(
			'running'
		);
	});
});
