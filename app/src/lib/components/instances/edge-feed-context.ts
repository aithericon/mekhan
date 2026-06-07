/**
 * Svelte context that supplies per-EDGE live-media-feed descriptors to the
 * editor's custom edge component when it's rendered as part of an
 * instance/run-view canvas overlay. Mirrors `runtime-context.ts`: the provider
 * is `WorkflowGraphView.svelte`; the consumer is `DeletableEdge.svelte` (the
 * custom edge every connection renders through).
 *
 * When this context is ABSENT (the regular template editor), `useEdgeFeeds`
 * returns `undefined` — the edge then renders nothing extra, so the editor is
 * unchanged. When present, the edge looks itself up by id and, on a non-null
 * `EdgeFeed`, mounts the on-graph media widget.
 *
 * The getter is closed over a reactive `$derived` map in the provider, so it
 * re-reads fresh each time the marking/executions poll cycle bumps — the edge
 * components stay live without the graph prop (and thus xyflow's edge set)
 * being mutated.
 */
import { getContext, setContext } from 'svelte';
import type { WorkflowGraph, WorkflowNode } from '$lib/api/client';
import { planLiveRender, defaultMseSupported, type LiveRenderPlan } from '$lib/channels/renderers';
import { channelRuntimeFor, type ChannelRuntime } from '$lib/stores/instance-marking.svelte';

/**
 * Everything the on-edge media widget needs to open a live tap and render it:
 * the source channel's identity, the execution that owns its bytes, the
 * classified render plan, and the live (per-poll) channel lifecycle.
 */
export interface EdgeFeed {
	/** The xyflow edge id this feed hangs on. */
	edgeId: string;
	/** Executor `execution_id` whose `/channels/{name}/data` tap carries the bytes. */
	executionId: string;
	/** The source channel name (== the edge `sourceHandle`). */
	channelName: string;
	/** The id of the node producing this feed (the edge's `source`). */
	sourceNodeId: string;
	/**
	 * The producing node's statically-declared robot model, resolved from its
	 * `requirements.constraints` (`capability === 'ros' && field === 'robot_model'`
	 * → `value`), or null when the node declares no such constraint.
	 */
	robotModel: string | null;
	/** The channel element's declared `content_type` (the live tap mime). */
	contentType: string;
	/**
	 * Classified live render plan (mse-video / mjpeg / pcm; see `renderers.ts`),
	 * or `null` when the channel is data-plane binary but has NO live renderer
	 * for its `content_type`. A null plan still produces a feed so the edge can
	 * show a minimal liveness `dot` (no decode, no media element) — see
	 * {@link EdgeMediaWidget}.
	 */
	plan: LiveRenderPlan | null;
	/** Per-poll channel lifecycle (opened / closed / element count). */
	runtime: ChannelRuntime;
	/**
	 * The producing step's latest execution `status` (`running` / `completed` /
	 * `failed` / …), or null when unknown. This — NOT `runtime.opened` — is the
	 * authoritative liveness signal for a DATA channel: its bytes flow OUT OF BAND
	 * (JetStream / nats-latest), so per-element tokens never enter the net marking
	 * (`count(p_{node}_{chan}) == 0` the whole time it streams). The `?follow=1`
	 * tap delivers bytes precisely while the producer is `running`, so the widget
	 * gates on that. See {@link producerRunning} / {@link edgeFeedLifecycle}.
	 */
	producerStatus: string | null;
	/**
	 * Whether the owning instance has reached a terminal status (completed /
	 * failed / cancelled). Combined with `runtime.closed` it drives the widget's
	 * end-state freeze — see {@link edgeFeedLifecycle}.
	 */
	terminal: boolean;
}

/** The presentation lifecycle of an edge feed, derived purely from its inputs. */
export type EdgeFeedLifecycle = 'idle' | 'live' | 'ended';

/** Step-execution statuses that mean the producer is done (no more bytes). */
const TERMINAL_STEP_STATUSES = new Set(['completed', 'failed', 'cancelled', 'skipped', 'timed_out']);

/** True while the producing step is actively producing (the tap delivers bytes). */
export function producerRunning(producerStatus: string | null | undefined): boolean {
	return producerStatus === 'running';
}

/**
 * Pure: classify an edge feed as IDLE (producer hasn't started streaming yet),
 * LIVE (producer running → bytes on the wire), or ENDED.
 *
 * A feed ENDS when ANY of: the channel's close token landed (`runtime.closed`),
 * the owning instance is terminal, or the producing step itself reached a
 * terminal status — at that point the widget freezes the last frame, releases
 * its tap + cap slot, and shows an "ended" badge (it never auto-loads a replay
 * stream).
 *
 * It is LIVE iff the producer step is `running` (the authoritative data-channel
 * liveness signal — element tokens never enter the marking, so `runtime.opened`
 * can't be used). Otherwise IDLE (e.g. the producer is still `pending`).
 *
 * Factored out so the transition is unit-testable without mounting the component.
 */
export function edgeFeedLifecycle(
	runtime: Pick<ChannelRuntime, 'closed'>,
	terminal: boolean,
	producerStatus: string | null = null
): EdgeFeedLifecycle {
	if (runtime.closed || terminal || TERMINAL_STEP_STATUSES.has(producerStatus ?? '')) return 'ended';
	return producerRunning(producerStatus) ? 'live' : 'idle';
}

export type EdgeFeedLookup = (edgeId: string) => EdgeFeed | null;

export const EDGE_FEED_CONTEXT_KEY = Symbol('edge-feed');

export function provideEdgeFeeds(getter: EdgeFeedLookup): void {
	setContext(EDGE_FEED_CONTEXT_KEY, getter);
}

/**
 * The edge-feed lookup, or `undefined` when no provider is present (the
 * template editor). Callers MUST treat `undefined` as "no instance context →
 * render nothing extra".
 */
export function useEdgeFeeds(): EdgeFeedLookup | undefined {
	return getContext<EdgeFeedLookup | undefined>(EDGE_FEED_CONTEXT_KEY);
}

// ---------------------------------------------------------------------------
// Pure derivation — extracted from WorkflowGraphView so it's unit-testable.
// ---------------------------------------------------------------------------

/** The slim shape `instance-marking`'s `channelRuntimeFor` needs (the store). */
type MarkingStoreLike = Parameters<typeof channelRuntimeFor>[0];

/** A minimally-typed view of an instance's step-execution row. */
export interface ExecutionLike {
	execution_id?: string | null;
	/** Latest execution status — the data-channel liveness signal (see EdgeFeed). */
	status?: string | null;
}

/**
 * Resolve a source node's declared channels. `channels` lives only on the
 * `automated_step` arm of the node-data union — mirror the same defensive
 * `data.channels` access shape `edge-lane.ts` uses (structural, not coupled to
 * the discriminated union), so a non-automated source simply yields `[]`.
 */
function channelsOf(node: WorkflowNode | undefined): { name: string; plane: string; element?: unknown }[] {
	if (!node) return [];
	const channels = (node.data as { channels?: { name: string; plane: string; element?: unknown }[] })
		.channels;
	return Array.isArray(channels) ? channels : [];
}

/**
 * Resolve a producing node's statically-declared robot model from its
 * `requirements.constraints`: the `value` of the constraint with
 * `capability === 'ros' && field === 'robot_model'`. Returns null when the node,
 * its `requirements`, the `constraints` array, the constraint, or its `value` is
 * missing/non-string. Traversal: `node.data.requirements.constraints[]`.
 */
function robotModelOf(node: WorkflowNode | undefined): string | null {
	if (!node) return null;
	const requirements = (node.data as { requirements?: unknown }).requirements;
	if (!requirements || typeof requirements !== 'object') return null;
	const constraints = (requirements as { constraints?: unknown }).constraints;
	if (!Array.isArray(constraints)) return null;
	for (const c of constraints) {
		if (c && typeof c === 'object') {
			const { capability, field, value } = c as {
				capability?: unknown;
				field?: unknown;
				value?: unknown;
			};
			if (capability === 'ros' && field === 'robot_model' && typeof value === 'string') {
				return value;
			}
		}
	}
	return null;
}

/** Pull a binary channel's `content_type` off its `element` descriptor, or null. */
function binaryContentType(element: unknown): string | null {
	if (element && typeof element === 'object') {
		const el = element as { type?: string; content_type?: string };
		if (el.type === 'binary' && typeof el.content_type === 'string') return el.content_type;
	}
	return null;
}

/**
 * Pull a `;model=<id>` parameter off a channel content_type, or null. The robot
 * model travels WITH the joint-state stream (e.g.
 * `application/vnd.aithericon.joint-state+x-ndjson;model=xarm6`) so a view-only
 * producer that carries no ROS requirement — a playback node, or any data-plane
 * joint source — still tells the twin which URDF (`robot_description` asset) to
 * load. Complements {@link robotModelOf} (the ROS-requirement path).
 */
function modelFromContentType(contentType: string | null): string | null {
	if (!contentType) return null;
	for (const part of contentType.split(';').slice(1)) {
		const eq = part.indexOf('=');
		if (eq < 0) continue;
		if (part.slice(0, eq).trim().toLowerCase() === 'model') {
			const v = part.slice(eq + 1).trim();
			if (v) return v;
		}
	}
	return null;
}

/**
 * Build the `edgeId → EdgeFeed` map for an instance run.
 *
 * For each edge we resolve its source node and find the source channel decl
 * whose `name === edge.sourceHandle` (the SAME channel-by-sourceHandle lookup
 * `edge-lane.ts` uses). We keep edges that are:
 *  - DATA-plane channels,
 *  - whose element is `binary` with a `content_type`,
 *  - AND whose source node has at least one execution with an `execution_id`
 *    (the latest one's id is the tap target).
 *
 * The channel's `content_type` is classified with `planLiveRender()`:
 *  - a non-null plan (mse / mjpeg / pcm) → the full media widget renders,
 *  - a NULL plan (binary, but no live renderer for that content_type) → the
 *    feed is still emitted with `plan: null` so the edge shows a minimal
 *    liveness `dot` indicator instead of a decoder.
 *
 * Edges with no execution yet (or a control/json channel) produce no feed.
 *
 * `executionsByNode` maps `node_id → ExecutionLike[]` ordered so `.at(-1)` is
 * the latest; we take the latest with a non-null `execution_id`.
 *
 * `terminal` (the owning instance's terminal status) is stamped onto every feed
 * so the widget can freeze its end-state even if it never observed a `close`
 * token — see {@link edgeFeedLifecycle}.
 */
export function deriveEdgeFeeds(
	graph: WorkflowGraph | null,
	nodesById: Map<string, WorkflowNode>,
	executionsByNode: Map<string, ExecutionLike[]>,
	marking: MarkingStoreLike,
	terminal: boolean = false,
	// Injected for testability (jsdom has no `MediaSource`), exactly like
	// `planLiveRender`'s own probe. Production uses the real capability check.
	mseSupported: (mime: string) => boolean = defaultMseSupported
): Map<string, EdgeFeed> {
	const map = new Map<string, EdgeFeed>();
	if (!graph) return map;

	for (const edge of graph.edges) {
		const sourceHandle = edge.sourceHandle;
		if (!sourceHandle) continue;

		const source = nodesById.get(edge.source);
		const channels = channelsOf(source);
		const ch = channels.find((c) => c.name === sourceHandle);
		if (!ch) continue;

		// Data-plane binary channels only.
		if (ch.plane !== 'data') continue;
		const contentType = binaryContentType(ch.element);
		if (!contentType) continue;

		// A null plan is KEPT — the edge shows a liveness dot rather than a decoder.
		const plan = planLiveRender(contentType, mseSupported);

		// Latest execution carrying a tappable execution_id; skip if none yet.
		const rows = executionsByNode.get(edge.source);
		const latest = rows?.at(-1);
		const executionId = latest?.execution_id;
		if (!executionId) continue;

		const runtime = channelRuntimeFor(marking, edge.source, ch.name);

		map.set(edge.id, {
			edgeId: edge.id,
			executionId,
			channelName: ch.name,
			sourceNodeId: edge.source,
			robotModel: robotModelOf(source) ?? modelFromContentType(contentType),
			contentType,
			plan,
			runtime,
			producerStatus: latest?.status ?? null,
			terminal
		});
	}

	return map;
}
