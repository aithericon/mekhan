/**
 * Edge "lane" coloring.
 *
 * An edge leaves a node through a port (an xyflow `Handle`). We tint the edge
 * to match that source port's color and draw it as wide as the port circle, so
 * a connection reads as a colored lane flowing out of its socket. This module
 * is the single source of truth for "what color is the port an edge starts
 * from" — it mirrors the handle colors defined in the node components:
 *
 *   - standard out handles → the node-kind border var (`--node-*`), same as
 *     `workflowNodeHandleClass` in WorkflowNodeCard.svelte
 *   - `error` handles      → red   (#ef4444),  see AutomatedStep/Agent/SubWorkflow
 *   - agent `tools` handle  → purple (#a855f7), see AgentNode
 *   - timeout exception     → amber (#f59e0b), see TimeoutNode
 *   - streaming channels    → plane color (data=amber, control=purple), see
 *     `channelStyle` in AutomatedStepNode.svelte
 *
 * Keep these in sync with the handle styles — they intentionally render the
 * same hue on both ends of the wire.
 */
// Minimal structural shape so both a `WorkflowGraph` node and an xyflow `Node`
// (the live editor representation) can be passed without coupling to either.
type LaneNode = { type?: string; data?: unknown };

/** node.type (NodeKind) → the `--node-*` CSS variable that colors its ports. */
const NODE_COLOR_VAR: Record<string, string> = {
	start: '--node-start',
	end: '--node-end',
	human_task: '--node-human-task',
	automated_step: '--node-automated',
	decision: '--node-decision',
	parallel_split: '--node-parallel',
	join: '--node-parallel',
	loop: '--node-loop',
	map: '--node-map',
	scope: '--node-scope',
	lease_scope: '--node-lease-scope',
	phase_update: '--node-phase-update',
	progress_update: '--node-progress-update',
	failure: '--node-failure',
	delay: '--node-delay',
	timeout: '--node-timeout',
	// Trigger nodes reuse the decision palette (see TriggerNode.svelte).
	trigger: '--node-decision',
	sub_workflow: '--node-sub-workflow',
	agent: '--node-agent'
};

// Special semantic-handle colors — hardcoded to match the inline `style=` on
// the corresponding handles in the node components.
const ERROR_COLOR = '#ef4444';
const TOOLS_COLOR = '#a855f7';
const TIMEOUT_COLOR = '#f59e0b';
const DATA_CHANNEL_COLOR = '#f59e0b';
const CONTROL_CHANNEL_COLOR = '#a855f7';

/** Minimal declared-channel shape (docs/25) — name doubles as the handle id. */
export type LaneChannel = { name: string; plane: string };

/**
 * Resolve the streaming Channel an edge leaves through: the handle id of a
 * channel handle IS the channel name, so match `sourceHandle` back into the
 * source node's declared `channels`. Single source of truth for "which channel
 * does this edge tap" — used by the lane coloring below AND by the canvas'
 * `toFlowEdges` (which stashes the plane on edge data for the join chip), so
 * the two can't drift. Returns `undefined` for non-channel handles or when the
 * node declares no channels (`channels` lives only on the automated-step
 * variant of the node-data union).
 */
export function channelForSourceHandle(
	source: LaneNode | undefined,
	sourceHandle: string | null | undefined
): LaneChannel | undefined {
	if (!source || !sourceHandle) return undefined;
	const channels = (source.data as { channels?: LaneChannel[] } | undefined)?.channels;
	if (!Array.isArray(channels)) return undefined;
	return channels.find((c) => c.name === sourceHandle);
}

/**
 * The base CSS color of the port an edge leaves from. Returns a `var(--node-*)`
 * reference for standard ports (so it tracks the theme), a hex literal for the
 * special semantic handles, or the neutral `--border` var when the source node
 * can't be resolved.
 */
export function edgeLaneColor(
	source: LaneNode | undefined,
	sourceHandle: string | null | undefined
): string {
	if (!source) return 'var(--border)';
	if (sourceHandle === 'error') return ERROR_COLOR;
	if (sourceHandle === 'tools') return TOOLS_COLOR;
	if (source.type === 'timeout' && sourceHandle === 'timeout') return TIMEOUT_COLOR;
	// Streaming-channel handle: the handle id is the channel name. Match it back
	// to the declared channel to recover its plane (mirrors `channelStyle`).
	const ch = channelForSourceHandle(source, sourceHandle);
	if (ch) return ch.plane === 'data' ? DATA_CHANNEL_COLOR : CONTROL_CHANNEL_COLOR;
	return `var(${NODE_COLOR_VAR[source.type ?? ''] ?? '--border'})`;
}

/** Lane width in px — matches the port circle (`!h-3 !w-3` = 12px) so the edge
 *  reads as a lane plugged into the socket. A hair under 12 lets the port ring
 *  still cap the lane visually. */
export const EDGE_LANE_WIDTH_PX = 11;
